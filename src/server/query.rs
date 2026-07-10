use crate::server::manifest::{FieldSchema, Manifest, PyreSession, QueryManifest, SqlInfo};
use crate::sync_deltas::AffectedRowTableGroup;
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub struct QueryResult {
    pub response: JsonValue,
    pub affected_rows: Vec<AffectedRowTableGroup>,
}

#[derive(Debug)]
pub struct ExplainStatement {
    pub include: bool,
    pub sql: String,
    pub params: Vec<String>,
    pub values: Vec<JsonValue>,
    pub plan: Vec<HashMap<String, JsonValue>>,
    pub error: Option<String>,
}

struct ResultSet {
    columns: Vec<String>,
    rows: Vec<HashMap<String, JsonValue>>,
}

/// Execute a generated manifest query or mutation against a libSQL connection.
///
/// This performs the same runtime transformations as the TypeScript server:
/// JSON input serialization, omittable `__is_set` flags, session SQL args,
/// response formatting, and `_affectedRows` extraction for live sync deltas.
pub async fn run(
    conn: &libsql::Connection,
    manifest: &Manifest,
    query_id: &str,
    input: JsonValue,
    session: &PyreSession,
) -> Result<QueryResult, Error> {
    run_inner(conn, manifest, query_id, input, session, false).await
}

pub async fn run_sync(
    conn: &libsql::Connection,
    manifest: &Manifest,
    query_id: &str,
    input: JsonValue,
    session: &PyreSession,
) -> Result<QueryResult, Error> {
    run_inner(conn, manifest, query_id, input, session, true).await
}

pub async fn explain(
    conn: &libsql::Connection,
    manifest: &Manifest,
    query_id: &str,
    input: JsonValue,
    session: &PyreSession,
) -> Result<Vec<ExplainStatement>, Error> {
    let query = manifest
        .queries
        .get(query_id)
        .ok_or_else(|| Error::UnknownQuery(query_id.to_string()))?;
    let args = build_args(query, input, session)?;
    let mut statements = Vec::new();

    for statement in &query.sql {
        let (sql, values) = statement_args(statement, &args)?;
        let json_values = values
            .iter()
            .cloned()
            .map(libsql_to_json)
            .collect::<Vec<_>>();
        let (plan, error) = explain_statement(conn, &sql, values).await?;
        statements.push(ExplainStatement {
            include: statement.include,
            sql,
            params: statement.params.clone(),
            values: json_values,
            plan,
            error,
        });
    }

    Ok(statements)
}

async fn run_inner(
    conn: &libsql::Connection,
    manifest: &Manifest,
    query_id: &str,
    input: JsonValue,
    session: &PyreSession,
    sync_mode: bool,
) -> Result<QueryResult, Error> {
    let query = manifest
        .queries
        .get(query_id)
        .ok_or_else(|| Error::UnknownQuery(query_id.to_string()))?;
    let args = build_args(query, input, session)?;
    let sql = if sync_mode {
        query.sync_sql.as_ref().unwrap_or(&query.sql)
    } else {
        &query.sql
    };
    let mut included_result_sets = Vec::new();

    for statement in sql {
        let (sql, values) = statement_args(statement, &args)?;

        if statement.include {
            included_result_sets.push(query_result_set(conn, &sql, values).await?);
        } else if sql.to_uppercase().contains("RETURNING") {
            let mut rows = query_rows(conn, &sql, values).await?;
            while rows.next().await.map_err(Error::Database)?.is_some() {}
        } else {
            execute_statement(conn, &sql, values).await?;
        }
    }

    Ok(QueryResult {
        response: if sync_mode {
            JsonValue::Object(serde_json::Map::new())
        } else {
            format_response(&included_result_sets)?
        },
        affected_rows: extract_affected_rows(&included_result_sets)?,
    })
}

fn build_args(
    query: &QueryManifest,
    input: JsonValue,
    session: &PyreSession,
) -> Result<HashMap<String, JsonValue>, Error> {
    let JsonValue::Object(input_object) = input else {
        return Err(Error::InvalidInput(
            "input must be a JSON object".to_string(),
        ));
    };
    let mut args = HashMap::new();
    let optional_args = query
        .optional_input_args
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let json_args = query
        .json_input_args
        .iter()
        .cloned()
        .collect::<HashSet<_>>();

    for key in &query.optional_input_args {
        args.insert(format!("{}__is_set", key), JsonValue::Bool(false));
    }

    for (name, schema) in &query.input_schema {
        let Some(value) = input_object.get(name) else {
            if schema.omittable {
                continue;
            }
            return Err(Error::InvalidInput(format!(
                "missing input field '{}'",
                name
            )));
        };

        validate_value(name, value, schema)?;
        let value = if json_args.contains(name) && !value.is_null() {
            JsonValue::String(value.to_string())
        } else {
            normalize_sql_value(value, schema)
        };

        args.insert(name.clone(), value);
        if optional_args.contains(name) {
            args.insert(format!("{}__is_set", name), JsonValue::Bool(true));
        }
    }

    for key in input_object.keys() {
        if !query.input_schema.contains_key(key) {
            return Err(Error::InvalidInput(format!(
                "unknown input field '{}'",
                key
            )));
        }
    }

    for session_arg in &query.session_args {
        let sql_arg = format!("session_{}", session_arg);
        let Some(value) = session.sql_args().get(&sql_arg) else {
            return Err(Error::InvalidSession(format!(
                "missing session field '{}'",
                session_arg
            )));
        };
        args.insert(sql_arg, value.clone());
    }

    Ok(args)
}

fn validate_value(name: &str, value: &JsonValue, schema: &FieldSchema) -> Result<(), Error> {
    if value.is_null() {
        return if schema.nullable {
            Ok(())
        } else {
            Err(Error::InvalidInput(format!(
                "input field '{}' cannot be null",
                name
            )))
        };
    }

    let valid = match schema.type_.as_str() {
        "String" | "DateTime" => value.is_string(),
        "Int" | "Float" => value.is_number(),
        "Bool" => value.is_boolean() || value.as_i64().map(|n| n == 0 || n == 1).unwrap_or(false),
        type_ if type_.starts_with("Id.Int") => value.is_number(),
        type_ if type_.starts_with("Id.Uuid") => value.is_string(),
        type_ if type_.starts_with("Json") => true,
        _ => true,
    };

    if valid {
        Ok(())
    } else {
        Err(Error::InvalidInput(format!(
            "input field '{}' must be {}",
            name, schema.type_
        )))
    }
}

fn normalize_sql_value(value: &JsonValue, schema: &FieldSchema) -> JsonValue {
    if schema.type_ == "Bool" {
        return JsonValue::from(
            if value == &JsonValue::Bool(true) || value.as_i64() == Some(1) {
                1
            } else {
                0
            },
        );
    }

    value.clone()
}

fn statement_args(
    statement: &SqlInfo,
    args: &HashMap<String, JsonValue>,
) -> Result<(String, Vec<libsql::Value>), Error> {
    let mut sql = String::with_capacity(statement.sql.len());
    let mut values = Vec::new();
    let params = statement.params.iter().cloned().collect::<HashSet<_>>();
    let mut chars = statement.sql.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '$' {
            sql.push(ch);
            continue;
        }

        let mut param = String::new();
        while let Some(next) = chars.peek() {
            if next.is_alphanumeric() || *next == '_' {
                param.push(chars.next().expect("peeked char should exist"));
            } else {
                break;
            }
        }

        if param.is_empty() {
            sql.push(ch);
            continue;
        }

        if params.contains(&param) {
            sql.push('?');
            let value = args.get(&param).cloned().unwrap_or(JsonValue::Null);
            values.push(json_to_libsql(value)?);
        } else {
            sql.push('$');
            sql.push_str(&param);
        }
    }

    Ok((sql, values))
}

async fn execute_statement(
    conn: &libsql::Connection,
    sql: &str,
    values: Vec<libsql::Value>,
) -> Result<(), Error> {
    if values.is_empty() {
        conn.execute(sql, ()).await.map_err(Error::Database)?;
    } else {
        conn.execute(sql, libsql::params_from_iter(values))
            .await
            .map_err(Error::Database)?;
    }
    Ok(())
}

async fn query_rows(
    conn: &libsql::Connection,
    sql: &str,
    values: Vec<libsql::Value>,
) -> Result<libsql::Rows, Error> {
    if values.is_empty() {
        conn.query(sql, ()).await.map_err(Error::Database)
    } else {
        conn.query(sql, libsql::params_from_iter(values))
            .await
            .map_err(Error::Database)
    }
}

async fn query_result_set(
    conn: &libsql::Connection,
    sql: &str,
    values: Vec<libsql::Value>,
) -> Result<ResultSet, Error> {
    let mut rows = query_rows(conn, sql, values).await?;
    let columns = (0..rows.column_count())
        .map(|index| rows.column_name(index).unwrap_or("").to_string())
        .collect::<Vec<_>>();
    let mut result_rows = Vec::new();

    while let Some(row) = rows.next().await.map_err(Error::Database)? {
        let mut result_row = HashMap::new();
        for (index, column) in columns.iter().enumerate() {
            let value = row
                .get::<libsql::Value>(index as i32)
                .map_err(Error::Database)?;
            result_row.insert(column.clone(), libsql_to_json(value));
        }
        result_rows.push(result_row);
    }

    Ok(ResultSet {
        columns,
        rows: result_rows,
    })
}

async fn explain_statement(
    conn: &libsql::Connection,
    sql: &str,
    values: Vec<libsql::Value>,
) -> Result<(Vec<HashMap<String, JsonValue>>, Option<String>), Error> {
    let result_set =
        match query_result_set(conn, &format!("EXPLAIN QUERY PLAN {sql}"), values.clone()).await {
            Ok(result_set) => result_set,
            Err(Error::Database(_)) => {
                match query_result_set(conn, &format!("EXPLAIN {sql}"), values).await {
                    Ok(result_set) => result_set,
                    Err(Error::Database(error)) => {
                        return Ok((Vec::new(), Some(error.to_string())))
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
    Ok((result_set.rows, None))
}

fn format_response(result_sets: &[ResultSet]) -> Result<JsonValue, Error> {
    let mut response = serde_json::Map::new();

    for result_set in result_sets {
        for column in &result_set.columns {
            if column.starts_with('_') {
                continue;
            }
            response
                .entry(column.clone())
                .or_insert_with(|| JsonValue::Array(Vec::new()));
            for row in &result_set.rows {
                let Some(JsonValue::String(raw)) = row.get(column) else {
                    continue;
                };
                let parsed = serde_json::from_str::<JsonValue>(raw).map_err(Error::Json)?;
                if parsed.is_array() {
                    response.insert(column.clone(), parsed);
                } else {
                    response
                        .entry(column.clone())
                        .or_insert_with(|| JsonValue::Array(Vec::new()))
                        .as_array_mut()
                        .expect("mutation response values are arrays")
                        .push(parsed);
                }
            }
        }
    }

    Ok(JsonValue::Object(response))
}

fn extract_affected_rows(result_sets: &[ResultSet]) -> Result<Vec<AffectedRowTableGroup>, Error> {
    let mut groups = Vec::new();

    for result_set in result_sets {
        if !result_set.columns.iter().any(|column| column == "_affectedRows") {
            continue;
        }

        for row in &result_set.rows {
            let Some(raw) = row.get("_affectedRows") else {
                continue;
            };
            let parsed = match raw {
                JsonValue::String(raw) => {
                    serde_json::from_str::<JsonValue>(raw).map_err(Error::Json)?
                }
                value => value.clone(),
            };

            if let JsonValue::Array(items) = parsed {
                for item in items {
                    groups.push(serde_json::from_value(item).map_err(Error::Json)?);
                }
            } else if !parsed.is_null() {
                groups.push(serde_json::from_value(parsed).map_err(Error::Json)?);
            }
        }
    }

    Ok(groups)
}

fn json_to_libsql(value: JsonValue) -> Result<libsql::Value, Error> {
    Ok(match value {
        JsonValue::Null => libsql::Value::Null,
        JsonValue::Bool(value) => libsql::Value::Integer(if value { 1 } else { 0 }),
        JsonValue::Number(value) => {
            if let Some(value) = value.as_i64() {
                libsql::Value::Integer(value)
            } else if let Some(value) = value.as_f64() {
                libsql::Value::Real(value)
            } else {
                return Err(Error::InvalidInput("unsupported number value".to_string()));
            }
        }
        JsonValue::String(value) => libsql::Value::Text(value),
        JsonValue::Array(_) | JsonValue::Object(_) => libsql::Value::Text(value.to_string()),
    })
}

fn libsql_to_json(value: libsql::Value) -> JsonValue {
    match value {
        libsql::Value::Null => JsonValue::Null,
        libsql::Value::Integer(value) => JsonValue::from(value),
        libsql::Value::Real(value) => JsonValue::from(value),
        libsql::Value::Text(value) => JsonValue::String(value),
        libsql::Value::Blob(value) => {
            JsonValue::Array(value.into_iter().map(JsonValue::from).collect())
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Database(libsql::Error),
    InvalidInput(String),
    InvalidSession(String),
    Json(serde_json::Error),
    UnknownQuery(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Database(error) => write!(f, "database error: {}", error),
            Error::InvalidInput(message) => write!(f, "invalid input: {}", message),
            Error::InvalidSession(message) => write!(f, "invalid session: {}", message),
            Error::Json(error) => write!(f, "json error: {}", error),
            Error::UnknownQuery(query_id) => write!(f, "unknown query: {}", query_id),
        }
    }
}

impl std::error::Error for Error {}
