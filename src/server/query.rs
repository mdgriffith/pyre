use crate::server::manifest::{normalize_sql_value, Manifest, PyreSession, QueryManifest, SqlInfo};
use crate::sync_deltas::AffectedRowTableGroup;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};

#[cfg(test)]
mod codec_parity_tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn compiled_recursive_inputs_are_complete_and_preserve_nested_values() {
        let source = format!(
            "{}\n{}",
            include_str!("../../packages/server/fixtures/compiled-batch/session.pyre"),
            include_str!("../../packages/server/fixtures/compiled-batch/schema.pyre")
        );
        let mut schema = crate::ast::Schema::default();
        crate::parser::run("schema.pyre", &source, &mut schema).unwrap();
        let context = crate::typecheck::check_schema(&crate::ast::Database {
            schemas: vec![schema],
        })
        .unwrap();
        let mut queries = crate::parser::parse_query(
            "queries.pyre",
            include_str!("../../packages/server/fixtures/compiled-batch/queries.pyre"),
        )
        .unwrap();
        crate::generated_queries::append_generated_crud_queries(&mut queries, &context);
        let info = crate::typecheck::check_queries(&queries, &context).unwrap();
        let mut files = vec![];
        crate::generate::manifest::generate_queries(&context, &queries, &info, &mut files);
        let manifest: Manifest = serde_json::from_str(
            &files
                .iter()
                .find(|file| file.path.ends_with("manifest.json"))
                .unwrap()
                .contents,
        )
        .unwrap();
        let create = manifest
            .queries
            .values()
            .find(|query| {
                query
                    .generated_edit
                    .as_ref()
                    .is_some_and(|edit| edit.kind == "create")
            })
            .unwrap();
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch("create table entries(id text primary key, release text, enabled integer, count integer, role text, details blob, updatedAt integer); create table _pyre_sync(id integer primary key, database_epoch text, server_revision integer); insert into _pyre_sync values(1,'e1',0);").await.unwrap();
        let fingerprint = manifest.fingerprint();
        let binding = BatchBinding {
            database_id: "tenant-1",
            namespace: &create.primary_db,
            manifest: &fingerprint,
            instance: "tab-1",
            auth_generation: 2,
        };
        let details = json!({"_type":"Bundle", "when":"2026-01-01T00:00:00Z", "role":"Member", "children":[{"_type":"Note","count":2,"enabled":false}], "byName":{"empty":{"_type":"Empty"}}, "note":null});
        let session_value = json!({"userId":7,"role":"Member","unrelated":"value","applicationClaim":true,"context":details});
        let session = PyreSession::new(session_value.clone(), &manifest.session_schema).unwrap();
        let mut claims = session_value.clone();
        claims["context"] = json!({"_type":"Note","count":2,"enabled":1,"hostile__count":999});
        let effective = PyreSession::new(claims.clone(), &manifest.session_schema).unwrap();
        assert_eq!(
            serde_json::from_str::<JsonValue>(
                effective.sql_args()["session_context"].as_str().unwrap()
            )
            .unwrap(),
            json!({"_type":"Note","count":2,"enabled":true})
        );
        claims["context"].as_object_mut().unwrap().remove("count");
        assert!(PyreSession::new(claims, &manifest.session_schema).is_err());
        conn.execute("insert into entries(id, details) values ('note', jsonb('{\"_type\":\"Note\",\"count\":2,\"enabled\":true}'))", ()).await.unwrap();
        let context_query = manifest
            .queries
            .values()
            .find(|query| query.operation == "query")
            .unwrap();
        let context_result = run(&conn, &manifest, &context_query.id, json!({}), &effective)
            .await
            .unwrap();
        assert_eq!(context_result.response, json!({"entry":[{"id":"note"}]}));
        let input = json!({"id":"uuid-is-a-string-codec","release":"release","enabled":true,"count":1,"role":{"_type":"Member"},"details":details});
        let mut request = BatchRequest {
            version: 1,
            database_id: "tenant-1".into(),
            namespace: create.primary_db.clone(),
            manifest: fingerprint.clone(),
            instance: "tab-1".into(),
            auth_generation: 2,
            database_epoch: "e1".into(),
            request_id: "request-1".into(),
            sequence: 1,
            operations: vec![BatchOperation {
                operation: create.id.clone(),
                input: input.clone(),
            }],
        };
        let result = run_batch(&conn, &manifest, &binding, &request, &session)
            .await
            .unwrap();
        assert_eq!(result.response["status"], "accepted");
        let row = conn
            .query("select json(details) from entries where id <> 'note'", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        let stored: JsonValue = serde_json::from_str(&row.get::<String>(0).unwrap()).unwrap();
        let mut expected = details.clone();
        expected["when"] = json!(1767225600);
        expected["role"] = json!({"_type":"Member"});
        assert_eq!(stored, expected);
        let raw = json!({"_type":"Raw","data":{"arbitrary":[null,true,{"_type":"Uninterpreted","extra":"retain"}]},"values":[1,null,2],"scalar":null});
        let mut raw_input = input.clone();
        raw_input["id"] = json!("raw");
        raw_input["details"] = raw.clone();
        request.operations[0].input = raw_input;
        run_batch(&conn, &manifest, &binding, &request, &session)
            .await
            .unwrap();
        let row = conn
            .query("select json(details) from entries where id = 'raw'", ())
            .await
            .unwrap()
            .next()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<JsonValue>(&row.get::<String>(0).unwrap()).unwrap(),
            raw
        );
        for (key, value) in [
            ("count", json!(1.5)),
            ("count", json!(9007199254740992_i64)),
            ("enabled", json!(1)),
            ("details", json!({"_type":"Note","count":1})),
            (
                "details",
                json!({"_type":"Note","count":1,"enabled":true,"unknown":1}),
            ),
            (
                "details",
                json!({"_type":"Note","count":null,"enabled":true}),
            ),
        ] {
            let mut invalid = input.clone();
            invalid[key] = value;
            request.operations[0].input = invalid;
            let error = run_batch(&conn, &manifest, &binding, &request, &session)
                .await
                .unwrap_err();
            assert_eq!(error.code(), "InvalidRequest");
            assert_eq!(error.operation_index(), Some(0));
        }
        let mut missing_nullable = input.clone();
        missing_nullable["details"]
            .as_object_mut()
            .unwrap()
            .remove("note");
        request.operations[0].input = missing_nullable;
        assert_eq!(
            run_batch(&conn, &manifest, &binding, &request, &session)
                .await
                .unwrap_err()
                .code(),
            "InvalidRequest"
        );
        let mut bad_session = session_value;
        bad_session["context"]["children"] = json!([{"_type":"Note","count":1}]);
        assert!(PyreSession::new(bad_session, &manifest.session_schema).is_err());
    }
}

pub const MAX_BATCH_OPERATIONS: usize = 100;
pub const MAX_BATCH_PAYLOAD_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BatchOperation {
    pub operation: String,
    pub input: JsonValue,
}

/// Trusted server binding, selected after authentication and database authorization.
pub struct BatchBinding<'a> {
    pub database_id: &'a str,
    pub namespace: &'a str,
    pub manifest: &'a str,
    pub instance: &'a str,
    pub auth_generation: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BatchRequest {
    pub version: u32,
    pub database_id: String,
    pub namespace: String,
    pub manifest: String,
    pub instance: String,
    pub auth_generation: u64,
    pub database_epoch: String,
    pub request_id: String,
    pub sequence: u64,
    pub operations: Vec<BatchOperation>,
}

#[derive(Debug)]
pub struct BatchResult {
    pub response: JsonValue,
    pub affected_rows: Vec<AffectedRowTableGroup>,
}

/// Execute an allowlisted ordered batch. No callbacks or publication run before commit.
/// Hosts must bound the raw HTTP body too, before decoding it into this request.
pub async fn run_batch(
    conn: &libsql::Connection,
    manifest: &Manifest,
    binding: &BatchBinding<'_>,
    request: &BatchRequest,
    session: &PyreSession,
) -> Result<BatchResult, Error> {
    if request.version != 1
        || manifest.version != 1
        || request.database_id != binding.database_id
        || request.namespace != binding.namespace
        || request.manifest != binding.manifest
        || binding.manifest != manifest.fingerprint()
        || request.instance != binding.instance
        || request.auth_generation != binding.auth_generation
        || request.request_id.is_empty()
        || request.instance.is_empty()
        || request.sequence == 0
        || binding.namespace.is_empty()
        || binding.manifest.is_empty()
    {
        return Err(Error::InvalidInput("request fence mismatch".into()));
    }
    crate::server::database_id::require_database_id(binding.database_id)
        .map_err(|_| Error::InvalidInput("invalid database identity".into()))?;
    if request.operations.len() > MAX_BATCH_OPERATIONS
        || serde_json::to_vec(request).map_err(Error::Json)?.len() > MAX_BATCH_PAYLOAD_BYTES
    {
        return Err(Error::InvalidInput("batch limit exceeded".into()));
    }
    let session = session
        .revalidate(&manifest.session_schema)
        .map_err(|error| Error::InvalidSession(error.to_string()))?;
    let mut prepared = Vec::new();
    for (index, operation) in request.operations.iter().enumerate() {
        let prepare = || {
            let query = manifest
                .queries
                .get(&operation.operation)
                .ok_or_else(|| Error::UnknownQuery(operation.operation.clone()))?;
            if query.id != operation.operation
                || query.primary_db != binding.namespace
                || !query.attached_dbs.is_empty()
                || !matches!(
                    query.operation.as_str(),
                    "insert" | "update" | "delete" | "transaction"
                )
            {
                return Err(Error::InvalidInput(
                    "operation is outside authorized mutation scope".into(),
                ));
            }
            if let Some(edit) = &query.generated_edit {
                if !matches!(edit.kind.as_str(), "create" | "update" | "delete")
                    || edit.write_statement_indices.len() != 1
                    || edit.write_statement_indices[0] >= query.sql.len()
                    || !query.sql[edit.write_statement_indices[0]].include
                {
                    return Err(Error::InvalidInput(
                        "invalid generated edit metadata".into(),
                    ));
                }
                if edit.kind == "update"
                    && !edit
                        .writable_inputs
                        .iter()
                        .any(|key| operation.input.get(key).is_some())
                {
                    return Err(Error::InvalidEdit);
                }
            }
            Ok((query, build_args(query, operation.input.clone(), &session)?))
        };
        prepared.push(prepare().map_err(|error| error.operation(index))?);
    }
    let mut response = serde_json::json!({
        "requestId": request.request_id, "databaseId": request.database_id,
        "instance": request.instance, "authGeneration": request.auth_generation,
        "databaseEpoch": request.database_epoch, "namespace": request.namespace,
        "manifest": request.manifest, "status": "confirmed", "results": []
    });
    if prepared.is_empty() {
        return Ok(BatchResult {
            response,
            affected_rows: Vec::new(),
        });
    }
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .map_err(Error::Database)?;
    let execution = async {
        let mut databases = tx.query("PRAGMA database_list", ()).await.map_err(Error::Database)?;
        while let Some(database) = databases.next().await.map_err(Error::Database)? {
            let name = database.get::<String>(1).map_err(Error::Database)?;
            if name != "main" && name != "temp" {
                return Err(Error::InvalidInput("attached databases are not supported in batches".into()));
            }
        }
        drop(databases);
        let mut epoch = tx.query("SELECT database_epoch FROM _pyre_sync WHERE id = 1", ()).await.map_err(Error::Database)?;
        let epoch = epoch.next().await.map_err(Error::Database)?.ok_or_else(|| Error::InvalidInput("missing database epoch".into()))?
            .get::<String>(0).map_err(Error::Database)?;
        if epoch != request.database_epoch { return Err(Error::InvalidInput("database epoch mismatch".into())); }
        let mut results = Vec::new();
        let mut affected_rows = Vec::new();
        for (index, (query, args)) in prepared.iter().enumerate() {
            let result = execute_generated_sql(&tx, &query.sql, args, query.generated_edit.as_ref()).await
                .map_err(|error| error.operation(index))?;
            results.push(serde_json::json!({"index": index, "operation": request.operations[index].operation, "value": result.response}));
            affected_rows.extend(result.affected_rows);
        }
        let (_, revision) = crate::server::sync::next_server_revision(&tx).await
            .map_err(|error| Error::UnsupportedRuntime(error.to_string()))?;
        response["status"] = JsonValue::from("accepted");
        response["results"] = JsonValue::Array(results);
        response["commitRevision"] = JsonValue::from(revision);
        // Conservative full-scope invalidation covers deletes and permission-dependent writes.
        response["reconciliation"] = serde_json::json!({"kind": "replaceRequired", "atLeast": revision, "invalidate": true, "minimumSafeRevision": revision});
        Ok(BatchResult { response, affected_rows })
    }.await;
    match execution {
        Ok(result) => {
            tx.commit().await.map_err(|_| Error::OutcomeUnknown)?;
            Ok(result)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

#[derive(Debug)]
pub struct QueryResult {
    pub response: JsonValue,
    pub affected_rows: Vec<AffectedRowTableGroup>,
}

#[derive(Debug)]
pub struct CommittedRevision {
    pub database_epoch: String,
    pub revision: i64,
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
    run_inner(conn, manifest, query_id, input, session, false, false)
        .await
        .map(|(result, _)| result)
}

pub async fn run_sync(
    conn: &libsql::Connection,
    manifest: &Manifest,
    query_id: &str,
    input: JsonValue,
    session: &PyreSession,
) -> Result<QueryResult, Error> {
    run_inner(conn, manifest, query_id, input, session, true, false)
        .await
        .map(|(result, _)| result)
}

/// Execute a named operation with a revision committed in the same transaction.
/// Reads allocate no revision; successful mutations include named no-ops. The
/// declared response remains unchanged, and publication must reuse this revision.
pub async fn run_with_revision(
    conn: &libsql::Connection,
    manifest: &Manifest,
    query_id: &str,
    input: JsonValue,
    session: &PyreSession,
    sync_mode: bool,
) -> Result<(QueryResult, Option<CommittedRevision>), Error> {
    run_inner(conn, manifest, query_id, input, session, sync_mode, true).await
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

/// Reject generated statements that remote libSQL cannot parse or execute.
pub fn validate_remote_manifest(manifest: &Manifest) -> Result<(), Error> {
    let mut unsupported_queries = manifest
        .queries
        .values()
        .filter(|query| {
            query
                .sql
                .iter()
                .chain(query.sync_sql.iter().flatten())
                .any(|statement| uses_temporary_table(&statement.sql))
        })
        .map(|query| query.id.clone())
        .collect::<Vec<_>>();

    unsupported_queries.sort();
    unsupported_queries.dedup();
    if unsupported_queries.is_empty() {
        Ok(())
    } else {
        Err(Error::UnsupportedRuntime(format!(
            "remote libSQL does not support the temporary tables required by nested inserts (queries: {}). Run these mutations against local SQLite; remote nested inserts require a future result-binding SQL strategy",
            unsupported_queries.join(", ")
        )))
    }
}

fn uses_temporary_table(sql: &str) -> bool {
    let normalized = sql.trim_start().to_ascii_uppercase();
    normalized.starts_with("CREATE TEMP TABLE") || normalized.starts_with("CREATE TEMPORARY TABLE")
}

async fn run_inner(
    conn: &libsql::Connection,
    manifest: &Manifest,
    query_id: &str,
    input: JsonValue,
    session: &PyreSession,
    sync_mode: bool,
    commit_revision: bool,
) -> Result<(QueryResult, Option<CommittedRevision>), Error> {
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

    if query.operation == "query" {
        return execute_generated_sql(conn, sql, &args, None)
            .await
            .map(|result| (result, None));
    }

    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .map_err(|error| Error::Database(error).execution("begin transaction", None))?;
    let execution = async {
        let result = execute_generated_sql(&tx, sql, &args, None).await?;
        let revision = if commit_revision {
            let (database_epoch, revision) =
                crate::server::sync::next_server_revision(&tx)
                    .await
                    .map_err(|error| Error::UnsupportedRuntime(error.to_string()))?;
            Some(CommittedRevision {
                database_epoch,
                revision,
            })
        } else {
            None
        };
        Ok((result, revision))
    }
    .await;
    match execution {
        Ok(result) => {
            tx.commit()
                .await
                .map_err(|error| Error::Database(error).execution("commit transaction", None))?;
            Ok(result)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

async fn execute_generated_sql(
    conn: &libsql::Connection,
    sql: &[SqlInfo],
    args: &HashMap<String, JsonValue>,
    edit: Option<&crate::server::manifest::GeneratedEdit>,
) -> Result<QueryResult, Error> {
    let mut included_result_sets = Vec::new();
    let mut identity = None;

    for (index, statement) in sql.iter().enumerate() {
        async {
            let (sql, values) = statement_args(statement, args)?;

            if statement.include {
                included_result_sets.push(query_result_set(conn, &sql, values).await?);
            } else if sql.to_uppercase().contains("RETURNING") {
                let mut rows = query_rows(conn, &sql, values).await?;
                while rows.next().await.map_err(Error::Database)?.is_some() {}
            } else {
                execute_statement(conn, &sql, values).await?;
            }
            if edit.is_some_and(|edit| edit.write_statement_indices.contains(&index)) {
                // changes() excludes trigger writes and is read before any later statement.
                let mut rows = conn
                    .query("SELECT changes()", ())
                    .await
                    .map_err(Error::Database)?;
                let count = rows
                    .next()
                    .await
                    .map_err(Error::Database)?
                    .ok_or(Error::TargetNotWritable)?
                    .get::<i64>(0)
                    .map_err(Error::Database)?;
                if count != 1 {
                    return Err(Error::TargetNotWritable);
                }
                identity = included_result_sets
                    .last()
                    .and_then(|set| set.rows.first())
                    .and_then(|row| row.get("_pyreEditId"))
                    .filter(|id| !id.is_null())
                    .cloned();
                if identity.is_none() {
                    return Err(Error::TargetNotWritable);
                }
            }
            Ok::<_, Error>(())
        }
        .await
        .map_err(|error| error.execution("execute", Some(index + 1)))?;
    }

    Ok(QueryResult {
        response: if edit.is_some() {
            serde_json::json!({"id": identity.ok_or(Error::TargetNotWritable)?})
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

        crate::server::manifest::validate_field(name, value, schema).map_err(|_| {
            Error::InvalidInput(format!("input field '{}' must be {}", name, schema.type_))
        })?;
        let value = if json_args.contains(name) && !value.is_null() {
            JsonValue::String(
                crate::server::manifest::normalize_json_value(value, schema).to_string(),
            )
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
            if session_arg.contains("__") {
                args.insert(sql_arg, JsonValue::Null);
                continue;
            }
            return Err(Error::InvalidSession(format!(
                "missing session field '{}'",
                session_arg
            )));
        };
        args.insert(sql_arg, value.clone());
    }

    Ok(args)
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
        if !result_set
            .columns
            .iter()
            .any(|column| column == "_affectedRows")
        {
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
    OutcomeUnknown,
    InvalidEdit,
    TargetNotWritable,
    Operation {
        index: usize,
        source: Box<Error>,
    },
    Database(libsql::Error),
    Execution {
        stage: &'static str,
        statement_index: Option<usize>,
        source: Box<Error>,
    },
    InvalidInput(String),
    InvalidSession(String),
    Json(serde_json::Error),
    UnsupportedRuntime(String),
    UnknownQuery(String),
}

impl Error {
    /// Safe wire code; never serialize database error text or successful prefixes.
    pub fn code(&self) -> &'static str {
        match self {
            Self::OutcomeUnknown => "OutcomeUnknown",
            Self::InvalidEdit => "InvalidEdit",
            Self::TargetNotWritable => "TargetNotWritable",
            Self::InvalidSession(_) => "InvalidSession",
            Self::InvalidInput(_) | Self::UnknownQuery(_) | Self::Json(_) => "InvalidRequest",
            Self::Database(_) | Self::UnsupportedRuntime(_) => "TransactionFailed",
            Self::Execution { source, .. } | Self::Operation { source, .. } => source.code(),
        }
    }

    pub fn operation_index(&self) -> Option<usize> {
        match self {
            Self::Operation { index, .. } => Some(*index),
            Self::Execution { source, .. } => source.operation_index(),
            _ => None,
        }
    }
    fn operation(self, index: usize) -> Self {
        Self::Operation {
            index,
            source: Box::new(self),
        }
    }
    fn execution(self, stage: &'static str, statement_index: Option<usize>) -> Self {
        Self::Execution {
            stage,
            statement_index,
            source: Box::new(self),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::OutcomeUnknown => write!(f, "OutcomeUnknown"),
            Error::InvalidEdit => write!(f, "InvalidEdit"),
            Error::TargetNotWritable => write!(f, "TargetNotWritable"),
            Error::Operation { index, source } => write!(f, "operation {}: {}", index, source),
            Error::Database(error) => write!(f, "database error: {}", error),
            Error::Execution {
                stage,
                statement_index,
                source,
            } => {
                write!(f, "{}", stage)?;
                if let Some(index) = statement_index {
                    write!(f, " SQL statement {} (1-based)", index)?;
                }
                write!(f, ": {}", source)
            }
            Error::InvalidInput(message) => write!(f, "invalid input: {}", message),
            Error::InvalidSession(message) => write!(f, "invalid session: {}", message),
            Error::Json(error) => write!(f, "json error: {}", error),
            Error::UnsupportedRuntime(message) => write!(f, "unsupported runtime: {}", message),
            Error::UnknownQuery(query_id) => write!(f, "unknown query: {}", query_id),
        }
    }
}

impl std::error::Error for Error {}
