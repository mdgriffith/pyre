use crate::{ast, typecheck};
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

#[derive(Debug)]
pub struct SeedResult {
    pub response: JsonValue,
}

/// Insert schema-shaped fixture or import data without applying query permissions or sync metadata.
pub async fn seed<T: Serialize>(conn: &libsql::Connection, input: T) -> Result<SeedResult, Error> {
    let input = serde_json::to_value(input).map_err(Error::Json)?;
    let JsonValue::Object(input) = input else {
        return Err(Error::InvalidInput(
            "seed input must be an object".to_string(),
        ));
    };

    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await
        .map_err(Error::Database)?;
    let result = async {
        let loaded = crate::server::schema::load_schema_from_database(&tx)
            .await
            .map_err(Error::Schema)?;
        let context = loaded.context().map_err(Error::Schema)?;
        let tables = physical_tables(context);
        let mut response = serde_json::Map::new();

        for (table_name, value) in input {
            let table = tables.get(&table_name).copied().ok_or_else(|| {
                Error::InvalidInput(format!("unknown seed table '{}'", table_name))
            })?;
            let JsonValue::Array(rows) = value else {
                return Err(Error::InvalidInput(format!(
                    "{}. expected an array of rows",
                    table_name
                )));
            };
            let inserted = insert_rows(&tx, context, table, rows, table_name.clone()).await?;
            response.insert(table_name, JsonValue::Array(inserted));
        }

        Ok(SeedResult {
            response: JsonValue::Object(response),
        })
    }
    .await;

    match result {
        Ok(result) => {
            tx.commit().await.map_err(Error::Database)?;
            Ok(result)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

fn physical_tables(context: &typecheck::Context) -> HashMap<String, &typecheck::Table> {
    context
        .tables
        .values()
        .map(|table| {
            (
                ast::get_tablename(&table.record.name, &table.record.fields),
                table,
            )
        })
        .collect()
}

fn insert_rows<'a>(
    conn: &'a libsql::Connection,
    context: &'a typecheck::Context,
    table: &'a typecheck::Table,
    rows: Vec<JsonValue>,
    path: String,
) -> Pin<Box<dyn Future<Output = Result<Vec<JsonValue>, Error>> + 'a>> {
    Box::pin(async move {
        let mut inserted = Vec::with_capacity(rows.len());
        for (index, row) in rows.into_iter().enumerate() {
            inserted
                .push(insert_row(conn, context, table, row, format!("{}[{}]", path, index)).await?);
        }
        Ok(inserted)
    })
}

fn insert_row<'a>(
    conn: &'a libsql::Connection,
    context: &'a typecheck::Context,
    table: &'a typecheck::Table,
    row: JsonValue,
    path: String,
) -> Pin<Box<dyn Future<Output = Result<JsonValue, Error>> + 'a>> {
    Box::pin(async move {
        let JsonValue::Object(mut input) = row else {
            return Err(Error::InvalidInput(format!("{} must be an object", path)));
        };
        let columns = ast::collect_columns(&table.record.fields);
        let links = ast::collect_links(&table.record.fields);

        for key in input.keys() {
            if !columns.iter().any(|column| column.name == *key)
                && !links.iter().any(|link| link.link_name == *key)
            {
                return Err(Error::InvalidInput(format!(
                    "{}.{} is not a column or link",
                    path, key
                )));
            }
        }

        // To-one links supply foreign keys needed before this row can be inserted.
        let mut inserted_to_one = Vec::new();
        for link in links.iter().filter(|link| !is_parent_to_child(table, link)) {
            let Some(nested) = input.remove(&link.link_name) else {
                continue;
            };
            validate_single_column_link(link, &path)?;
            let linked_table = typecheck::get_linked_table(context, link).ok_or_else(|| {
                Error::InvalidInput(format!("{}.{} has no linked table", path, link.link_name))
            })?;
            let nested_row = insert_row(
                conn,
                context,
                linked_table,
                nested,
                format!("{}.{}", path, link.link_name),
            )
            .await?;
            let derived = nested_row
                .get(&link.foreign.fields[0])
                .cloned()
                .ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "{}.{} did not return '{}'",
                        path, link.link_name, link.foreign.fields[0]
                    ))
                })?;
            merge_derived_value(&mut input, &link.local_ids[0], derived, &path)?;
            inserted_to_one.push((link.link_name.clone(), nested_row));
        }

        let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
        let mut physical_values = Vec::new();
        for column in &columns {
            if let Some(value) = input.get(&column.name) {
                serialize_column(
                    context,
                    column,
                    &column.name,
                    value,
                    &format!("{}.{}", path, column.name),
                    &mut physical_values,
                )?;
            }
        }
        let physical_row = execute_insert(conn, &table_name, physical_values, &path).await?;
        let mut result = format_row(context, table, &physical_row)?;
        for (link_name, nested_row) in inserted_to_one {
            result
                .as_object_mut()
                .expect("formatted rows are objects")
                .insert(link_name, nested_row);
        }

        // To-many links inherit keys returned by the inserted parent.
        for link in links.iter().filter(|link| is_parent_to_child(table, link)) {
            let Some(nested) = input.remove(&link.link_name) else {
                continue;
            };
            validate_single_column_link(link, &path)?;
            let JsonValue::Array(mut nested_rows) = nested else {
                return Err(Error::InvalidInput(format!(
                    "{}.{} must be an array",
                    path, link.link_name
                )));
            };
            let inherited = result.get(&link.local_ids[0]).cloned().ok_or_else(|| {
                Error::InvalidInput(format!("{} did not return '{}'", path, link.local_ids[0]))
            })?;
            for (index, nested_row) in nested_rows.iter_mut().enumerate() {
                let JsonValue::Object(nested_object) = nested_row else {
                    return Err(Error::InvalidInput(format!(
                        "{}.{}[{}] must be an object",
                        path, link.link_name, index
                    )));
                };
                merge_derived_value(
                    nested_object,
                    &link.foreign.fields[0],
                    inherited.clone(),
                    &format!("{}.{}[{}]", path, link.link_name, index),
                )?;
            }
            let linked_table = typecheck::get_linked_table(context, link).ok_or_else(|| {
                Error::InvalidInput(format!("{}.{} has no linked table", path, link.link_name))
            })?;
            let nested_result = insert_rows(
                conn,
                context,
                linked_table,
                nested_rows,
                format!("{}.{}", path, link.link_name),
            )
            .await?;
            result
                .as_object_mut()
                .expect("formatted rows are objects")
                .insert(link.link_name.clone(), JsonValue::Array(nested_result));
        }

        Ok(result)
    })
}

fn is_parent_to_child(table: &typecheck::Table, link: &ast::LinkDetails) -> bool {
    let primary_key = ast::get_primary_id_field_name(&table.record.fields);
    link.local_ids
        .iter()
        .all(|field| primary_key.as_ref() == Some(field))
}

fn validate_single_column_link(link: &ast::LinkDetails, path: &str) -> Result<(), Error> {
    if link.local_ids.len() == 1 && link.foreign.fields.len() == 1 {
        Ok(())
    } else {
        Err(Error::Unsupported(format!(
            "{}.{} uses a composite link",
            path, link.link_name
        )))
    }
}

fn merge_derived_value(
    row: &mut serde_json::Map<String, JsonValue>,
    field: &str,
    value: JsonValue,
    path: &str,
) -> Result<(), Error> {
    if let Some(existing) = row.get(field) {
        if existing != &value {
            return Err(Error::InvalidInput(format!(
                "{}.{} conflicts with the value derived from its link",
                path, field
            )));
        }
    } else {
        row.insert(field.to_string(), value);
    }
    Ok(())
}

fn serialize_column(
    context: &typecheck::Context,
    column: &ast::Column,
    physical_name: &str,
    value: &JsonValue,
    path: &str,
    output: &mut Vec<(String, libsql::Value)>,
) -> Result<(), Error> {
    if value.is_null() {
        if !column.nullable && !matches!(column.type_, ast::ColumnType::Nullable(_)) {
            return Err(Error::InvalidInput(format!("{} cannot be null", path)));
        }
        output.push((physical_name.to_string(), libsql::Value::Null));
        return Ok(());
    }

    match &column.type_ {
        ast::ColumnType::Custom(type_name) => {
            serialize_constructed(context, type_name, physical_name, value, path, output)
        }
        ast::ColumnType::Nullable(inner) => {
            serialize_value(context, inner, physical_name, value, path, output)
        }
        type_ => serialize_value(context, type_, physical_name, value, path, output),
    }
}

fn serialize_value(
    context: &typecheck::Context,
    type_: &ast::ColumnType,
    physical_name: &str,
    value: &JsonValue,
    path: &str,
    output: &mut Vec<(String, libsql::Value)>,
) -> Result<(), Error> {
    use ast::ColumnType;
    let sql_value = match type_ {
        ColumnType::Bool => libsql::Value::Integer(
            value
                .as_bool()
                .map(i64::from)
                .ok_or_else(|| invalid_type(path, "a boolean"))?,
        ),
        ColumnType::DateTime => libsql::Value::Integer(
            datetime_to_epoch(value)
                .ok_or_else(|| invalid_type(path, "RFC3339 or Unix seconds"))?,
        ),
        ColumnType::Int | ColumnType::IdInt { .. } => libsql::Value::Integer(
            value
                .as_i64()
                .ok_or_else(|| invalid_type(path, "an integer"))?,
        ),
        ColumnType::ForeignKey {
            serialization_type, ..
        } => serialize_concrete(
            serialization_type
                .as_ref()
                .unwrap_or(&ast::ConcreteSerializationType::Integer),
            value,
            path,
        )?,
        ColumnType::Float => libsql::Value::Real(
            value
                .as_f64()
                .ok_or_else(|| invalid_type(path, "a number"))?,
        ),
        ColumnType::String | ColumnType::Date | ColumnType::IdUuid { .. } => libsql::Value::Text(
            value
                .as_str()
                .ok_or_else(|| invalid_type(path, "a string"))?
                .to_string(),
        ),
        ColumnType::Json | ColumnType::JsonTyped(_) | ColumnType::List(_) | ColumnType::Dict(_) => {
            libsql::Value::Text(value.to_string())
        }
        ColumnType::Custom(type_name) => {
            return serialize_constructed(context, type_name, physical_name, value, path, output)
        }
        ColumnType::Nullable(inner) => {
            return serialize_value(context, inner, physical_name, value, path, output)
        }
    };
    output.push((physical_name.to_string(), sql_value));
    Ok(())
}

fn serialize_constructed(
    context: &typecheck::Context,
    type_name: &str,
    physical_name: &str,
    value: &JsonValue,
    path: &str,
    output: &mut Vec<(String, libsql::Value)>,
) -> Result<(), Error> {
    let (tag, object) = match value {
        JsonValue::String(tag) => (tag.as_str(), None),
        JsonValue::Object(object) => {
            for legacy in ["type", "type_", "$"] {
                if object.contains_key(legacy) {
                    return Err(Error::InvalidInput(format!(
                        "{} must use '_type' as its discriminator",
                        path
                    )));
                }
            }
            let tag = object
                .get("_type")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| Error::InvalidInput(format!("{} requires '_type'", path)))?;
            (tag, Some(object))
        }
        _ => return Err(invalid_type(path, "a constructed value")),
    };
    let Some((_, typecheck::Type::OneOf { variants })) = context.types.get(type_name) else {
        return Err(Error::InvalidInput(format!(
            "{} references unknown type '{}'",
            path, type_name
        )));
    };
    let variant = variants
        .iter()
        .find(|variant| variant.name == tag)
        .ok_or_else(|| Error::InvalidInput(format!("{} has unknown variant '{}'", path, tag)))?;
    output.push((
        physical_name.to_string(),
        libsql::Value::Text(tag.to_string()),
    ));
    if let Some(fields) = &variant.fields {
        let object = object.ok_or_else(|| {
            Error::InvalidInput(format!("{} variant '{}' requires fields", path, tag))
        })?;
        for field in fields {
            let ast::Field::Column(column) = field else {
                continue;
            };
            let Some(value) = object.get(&column.name) else {
                if !column.nullable && !matches!(column.type_, ast::ColumnType::Nullable(_)) {
                    return Err(Error::InvalidInput(format!(
                        "{}.{} is required for variant '{}'",
                        path, column.name, tag
                    )));
                }
                continue;
            };
            serialize_column(
                context,
                column,
                &format!("{}__{}", physical_name, column.name),
                value,
                &format!("{}.{}", path, column.name),
                output,
            )?;
        }
    } else if let Some(object) = object {
        if let Some(key) = object.keys().find(|key| key.as_str() != "_type") {
            return Err(Error::InvalidInput(format!(
                "{}.{} is not a field of variant '{}'",
                path, key, tag
            )));
        }
    }
    if let Some(fields) = &variant.fields {
        if let Some(object) = object {
            for key in object.keys().filter(|key| key.as_str() != "_type") {
                if !fields
                    .iter()
                    .any(|field| matches!(field, ast::Field::Column(column) if column.name == *key))
                {
                    return Err(Error::InvalidInput(format!(
                        "{}.{} is not a field of variant '{}'",
                        path, key, tag
                    )));
                }
            }
        }
    }
    Ok(())
}

fn serialize_concrete(
    type_: &ast::ConcreteSerializationType,
    value: &JsonValue,
    path: &str,
) -> Result<libsql::Value, Error> {
    use ast::ConcreteSerializationType as Type;
    match type_ {
        Type::Integer | Type::IdInt => value
            .as_i64()
            .map(libsql::Value::Integer)
            .ok_or_else(|| invalid_type(path, "an integer")),
        Type::Real => value
            .as_f64()
            .map(libsql::Value::Real)
            .ok_or_else(|| invalid_type(path, "a number")),
        Type::Text | Type::Date | Type::IdUuid => value
            .as_str()
            .map(|value| libsql::Value::Text(value.to_string()))
            .ok_or_else(|| invalid_type(path, "a string")),
        Type::DateTime => datetime_to_epoch(value)
            .map(libsql::Value::Integer)
            .ok_or_else(|| invalid_type(path, "RFC3339 or Unix seconds")),
        Type::JsonB => Ok(libsql::Value::Text(value.to_string())),
        Type::Blob | Type::VectorBlob { .. } => {
            let bytes = value
                .as_array()
                .ok_or_else(|| invalid_type(path, "an array of bytes"))?
                .iter()
                .map(|value| {
                    value
                        .as_u64()
                        .and_then(|value| u8::try_from(value).ok())
                        .ok_or_else(|| invalid_type(path, "an array of bytes"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(libsql::Value::Blob(bytes))
        }
    }
}

async fn execute_insert(
    conn: &libsql::Connection,
    table: &str,
    values: Vec<(String, libsql::Value)>,
    path: &str,
) -> Result<HashMap<String, libsql::Value>, Error> {
    let sql = if values.is_empty() {
        format!("INSERT INTO {} DEFAULT VALUES RETURNING *", quote(table))
    } else {
        let columns = values
            .iter()
            .map(|(name, _)| quote(name))
            .collect::<Vec<_>>()
            .join(", ");
        let placeholders = std::iter::repeat("?")
            .take(values.len())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "INSERT INTO {} ({}) VALUES ({}) RETURNING *",
            quote(table),
            columns,
            placeholders
        )
    };
    let params = values
        .into_iter()
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
    let mut rows = if params.is_empty() {
        conn.query(&sql, ()).await
    } else {
        conn.query(&sql, libsql::params_from_iter(params)).await
    }
    .map_err(|error| Error::DatabaseAt {
        path: path.to_string(),
        error,
    })?;
    let columns = (0..rows.column_count())
        .map(|index| rows.column_name(index).unwrap_or("").to_string())
        .collect::<Vec<_>>();
    let row = rows
        .next()
        .await
        .map_err(|error| Error::DatabaseAt {
            path: path.to_string(),
            error,
        })?
        .ok_or_else(|| Error::InvalidInput(format!("insert into '{}' returned no row", table)))?;
    let mut result = HashMap::new();
    for (index, column) in columns.into_iter().enumerate() {
        result.insert(
            column,
            row.get::<libsql::Value>(index as i32)
                .map_err(|error| Error::DatabaseAt {
                    path: path.to_string(),
                    error,
                })?,
        );
    }
    Ok(result)
}

fn format_row(
    context: &typecheck::Context,
    table: &typecheck::Table,
    physical: &HashMap<String, libsql::Value>,
) -> Result<JsonValue, Error> {
    let mut result = serde_json::Map::new();
    for column in ast::collect_columns(&table.record.fields) {
        let value = format_type_value(context, &column.type_, &column.name, physical)?;
        result.insert(column.name, value);
    }
    Ok(JsonValue::Object(result))
}

fn format_type_value(
    context: &typecheck::Context,
    type_: &ast::ColumnType,
    physical_name: &str,
    physical: &HashMap<String, libsql::Value>,
) -> Result<JsonValue, Error> {
    match type_ {
        ast::ColumnType::Custom(type_name) => {
            reconstruct_constructed(context, type_name, physical_name, physical)
        }
        ast::ColumnType::Nullable(inner) => {
            format_type_value(context, inner, physical_name, physical)
        }
        ast::ColumnType::Bool => Ok(physical
            .get(physical_name)
            .map(|value| JsonValue::Bool(matches!(value, libsql::Value::Integer(v) if *v != 0)))
            .unwrap_or(JsonValue::Null)),
        ast::ColumnType::Json
        | ast::ColumnType::JsonTyped(_)
        | ast::ColumnType::List(_)
        | ast::ColumnType::Dict(_) => match physical.get(physical_name) {
            Some(libsql::Value::Text(raw)) => serde_json::from_str(raw).map_err(Error::Json),
            Some(libsql::Value::Null) | None => Ok(JsonValue::Null),
            Some(value) => Ok(libsql_to_json(value.clone())),
        },
        _ => Ok(physical
            .get(physical_name)
            .cloned()
            .map(libsql_to_json)
            .unwrap_or(JsonValue::Null)),
    }
}

fn reconstruct_constructed(
    context: &typecheck::Context,
    type_name: &str,
    physical_name: &str,
    physical: &HashMap<String, libsql::Value>,
) -> Result<JsonValue, Error> {
    let Some(value) = physical.get(physical_name) else {
        return Ok(JsonValue::Null);
    };
    let JsonValue::String(tag) = libsql_to_json(value.clone()) else {
        return Ok(JsonValue::Null);
    };
    let Some((_, typecheck::Type::OneOf { variants })) = context.types.get(type_name) else {
        return Ok(JsonValue::String(tag));
    };
    let Some(variant) = variants.iter().find(|variant| variant.name == tag) else {
        return Ok(JsonValue::String(tag));
    };
    let Some(fields) = &variant.fields else {
        return Ok(JsonValue::String(tag));
    };
    let mut result = serde_json::Map::new();
    result.insert("_type".to_string(), JsonValue::String(tag));
    for field in fields {
        let ast::Field::Column(column) = field else {
            continue;
        };
        let name = format!("{}__{}", physical_name, column.name);
        let value = format_type_value(context, &column.type_, &name, physical)?;
        result.insert(column.name.clone(), value);
    }
    Ok(JsonValue::Object(result))
}

fn datetime_to_epoch(value: &JsonValue) -> Option<i64> {
    if let Some(value) = value.as_i64() {
        return Some(value);
    }
    let raw = value.as_str()?.trim();
    raw.parse::<i64>().ok().or_else(|| {
        chrono::DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|value| value.timestamp())
    })
}

fn invalid_type(path: &str, expected: &str) -> Error {
    Error::InvalidInput(format!("{} must be {}", path, expected))
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

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[derive(Debug)]
pub enum Error {
    Database(libsql::Error),
    DatabaseAt { path: String, error: libsql::Error },
    InvalidInput(String),
    Json(serde_json::Error),
    Schema(crate::server::schema::Error),
    Unsupported(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Database(error) => write!(f, "database error: {}", error),
            Error::DatabaseAt { path, error } => {
                write!(f, "database error at {}: {}", path, error)
            }
            Error::InvalidInput(message) => write!(f, "invalid seed input: {}", message),
            Error::Json(error) => write!(f, "json error: {}", error),
            Error::Schema(error) => write!(f, "schema error: {}", error),
            Error::Unsupported(message) => write!(f, "unsupported seed input: {}", message),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_links_are_rejected_explicitly() {
        let link = ast::LinkDetails {
            link_name: "parent".to_string(),
            local_ids: vec!["leftId".to_string(), "rightId".to_string()],
            foreign: ast::Qualified {
                schema: String::new(),
                table: "Parent".to_string(),
                fields: vec!["leftId".to_string(), "rightId".to_string()],
            },
            start_name: None,
            end_name: None,
            inline_comment: None,
        };

        let error = validate_single_column_link(&link, "children[0]").unwrap_err();
        assert!(error.to_string().contains("uses a composite link"));
    }
}
