use crate::{ast, typecheck};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

/// SQLite bind values, independent of the driver that executes the insert.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum SqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

#[derive(Debug, Serialize)]
pub struct Insert {
    pub sql: String,
    pub args: Vec<SqlValue>,
    pub path: String,
}

pub type ReturnedRow = serde_json::Map<String, JsonValue>;

/// Run the fixture against one schema. The host executes each insert in the same
/// transaction and returns its physical row before dependent inserts are prepared.
pub async fn ingest<F, Fut, E>(
    context: &typecheck::Context,
    namespace: &str,
    input: JsonValue,
    mut execute: F,
) -> Result<JsonValue, E>
where
    F: FnMut(Insert) -> Fut,
    Fut: Future<Output = Result<ReturnedRow, E>>,
    E: From<Error>,
{
    let JsonValue::Object(input) = input else {
        return Err(Error::InvalidInput("seed input must be an object".to_string()).into());
    };
    let tables: HashMap<_, _> = context
        .tables
        .values()
        .filter(|table| table.schema == namespace)
        .map(|table| {
            (
                ast::get_tablename(&table.record.name, &table.record.fields),
                table,
            )
        })
        .collect();
    let mut response = serde_json::Map::new();

    for (table_name, value) in input {
        let table = tables
            .get(&table_name)
            .copied()
            .ok_or_else(|| Error::InvalidInput(format!("unknown seed table '{}'", table_name)))?;
        let JsonValue::Array(rows) = value else {
            return Err(
                Error::InvalidInput(format!("{}. expected an array of rows", table_name)).into(),
            );
        };
        let inserted = insert_rows(&mut execute, context, table, rows, table_name.clone()).await?;
        response.insert(table_name, JsonValue::Array(inserted));
    }
    Ok(JsonValue::Object(response))
}

async fn insert_rows<F, Fut, E>(
    execute: &mut F,
    context: &typecheck::Context,
    table: &typecheck::Table,
    rows: Vec<JsonValue>,
    path: String,
) -> Result<Vec<JsonValue>, E>
where
    F: FnMut(Insert) -> Fut,
    Fut: Future<Output = Result<ReturnedRow, E>>,
    E: From<Error>,
{
    let mut inserted = Vec::with_capacity(rows.len());
    for (index, row) in rows.into_iter().enumerate() {
        inserted
            .push(insert_row(execute, context, table, row, format!("{}[{}]", path, index)).await?);
    }
    Ok(inserted)
}

fn insert_row<'a, F, Fut, E>(
    execute: &'a mut F,
    context: &'a typecheck::Context,
    table: &'a typecheck::Table,
    row: JsonValue,
    path: String,
) -> Pin<Box<dyn Future<Output = Result<JsonValue, E>> + 'a>>
where
    F: FnMut(Insert) -> Fut + 'a,
    Fut: Future<Output = Result<ReturnedRow, E>> + 'a,
    E: From<Error> + 'a,
{
    Box::pin(async move {
        let JsonValue::Object(mut input) = row else {
            return Err(Error::InvalidInput(format!("{} must be an object", path)).into());
        };
        let columns = ast::collect_columns(&table.record.fields);
        let links = ast::collect_links(&table.record.fields);

        for key in input.keys() {
            if columns
                .iter()
                .any(|column| column.name == *key && ast::is_sequence(column))
            {
                return Err(Error::InvalidInput(format!(
                    "{}.{} is a server-managed sequence and cannot be supplied",
                    path, key
                ))
                .into());
            }
            if !columns.iter().any(|column| column.name == *key)
                && !links.iter().any(|link| link.link_name == *key)
            {
                return Err(Error::InvalidInput(format!(
                    "{}.{} is not a column or link",
                    path, key
                ))
                .into());
            }
        }

        // To-one links supply foreign keys needed before this row can be inserted.
        let mut inserted_to_one = Vec::new();
        for link in links.iter().filter(|link| !is_parent_to_child(table, link)) {
            let Some(nested) = input.remove(&link.link_name) else {
                continue;
            };
            validate_single_column_link(link, &path)?;
            let linked_table = local_link_table(context, table, link, &path)?;
            let nested_row = insert_row(
                execute,
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
            } else if ast::is_primary_key(column)
                && matches!(column.type_, ast::ColumnType::IdUuid { .. })
            {
                physical_values.push((
                    column.name.clone(),
                    SqlValue::Text(uuid::Uuid::now_v7().to_string()),
                ));
            }
        }
        let physical_row =
            execute(insert_statement(&table_name, physical_values, path.clone())).await?;
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
            let linked_table = local_link_table(context, table, link, &path)?;
            let nested_path = format!("{}.{}", path, link.link_name);
            let (mut nested_rows, collection) = match nested {
                JsonValue::Array(rows) => (rows, true),
                JsonValue::Object(_) => (vec![nested], false),
                _ => {
                    return Err(Error::InvalidInput(format!(
                        "{} must be an object or an array",
                        nested_path
                    ))
                    .into())
                }
            };
            let inherited = result.get(&link.local_ids[0]).cloned().ok_or_else(|| {
                Error::InvalidInput(format!("{} did not return '{}'", path, link.local_ids[0]))
            })?;
            for (index, nested_row) in nested_rows.iter_mut().enumerate() {
                let row_path = if collection {
                    format!("{}[{}]", nested_path, index)
                } else {
                    nested_path.clone()
                };
                let JsonValue::Object(nested_object) = nested_row else {
                    return Err(
                        Error::InvalidInput(format!("{} must be an object", row_path)).into(),
                    );
                };
                merge_derived_value(
                    nested_object,
                    &link.foreign.fields[0],
                    inherited.clone(),
                    &row_path,
                )?;
            }
            let nested_result = if collection {
                JsonValue::Array(
                    insert_rows(execute, context, linked_table, nested_rows, nested_path).await?,
                )
            } else {
                insert_row(
                    execute,
                    context,
                    linked_table,
                    nested_rows.remove(0),
                    nested_path,
                )
                .await?
            };
            result
                .as_object_mut()
                .expect("formatted rows are objects")
                .insert(link.link_name.clone(), nested_result);
        }

        Ok(result)
    })
}

fn local_link_table<'a>(
    context: &'a typecheck::Context,
    table: &typecheck::Table,
    link: &'a ast::LinkDetails,
    path: &str,
) -> Result<&'a typecheck::Table, Error> {
    if link.foreign.schema != table.schema {
        return Err(Error::Unsupported(format!(
            "{}.{} points to another database; supply its identity explicitly",
            path, link.link_name
        )));
    }
    typecheck::get_linked_table(context, link).ok_or_else(|| {
        Error::InvalidInput(format!("{}.{} has no linked table", path, link.link_name))
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
    output: &mut Vec<(String, SqlValue)>,
) -> Result<(), Error> {
    if ast::is_sequence(column) {
        return Err(Error::InvalidInput(format!(
            "{} is a server-managed sequence and cannot be supplied",
            path
        )));
    }
    if value.is_null() {
        if !column.nullable && !matches!(column.type_, ast::ColumnType::Nullable(_)) {
            return Err(Error::InvalidInput(format!("{} cannot be null", path)));
        }
        output.push((physical_name.to_string(), SqlValue::Null));
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
    output: &mut Vec<(String, SqlValue)>,
) -> Result<(), Error> {
    use ast::ColumnType;
    let sql_value = match type_ {
        ColumnType::Bool => SqlValue::Integer(
            value
                .as_bool()
                .map(i64::from)
                .ok_or_else(|| invalid_type(path, "a boolean"))?,
        ),
        ColumnType::DateTime => SqlValue::Integer(
            datetime_to_epoch(value)
                .ok_or_else(|| invalid_type(path, "RFC3339 or Unix seconds"))?,
        ),
        ColumnType::Int | ColumnType::SequenceInt | ColumnType::IdInt { .. } => SqlValue::Integer(
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
        ColumnType::Float => SqlValue::Real(
            value
                .as_f64()
                .ok_or_else(|| invalid_type(path, "a number"))?,
        ),
        ColumnType::IdUuid { .. } => serialize_uuid(value, path)?,
        ColumnType::String | ColumnType::Date => SqlValue::Text(
            value
                .as_str()
                .ok_or_else(|| invalid_type(path, "a string"))?
                .to_string(),
        ),
        ColumnType::Json | ColumnType::JsonTyped(_) | ColumnType::List(_) | ColumnType::Dict(_) => {
            SqlValue::Text(value.to_string())
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
    output: &mut Vec<(String, SqlValue)>,
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
    output.push((physical_name.to_string(), SqlValue::Text(tag.to_string())));
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
) -> Result<SqlValue, Error> {
    use ast::ConcreteSerializationType as Type;
    match type_ {
        Type::Integer | Type::IdInt => value
            .as_i64()
            .map(SqlValue::Integer)
            .ok_or_else(|| invalid_type(path, "an integer")),
        Type::Real => value
            .as_f64()
            .map(SqlValue::Real)
            .ok_or_else(|| invalid_type(path, "a number")),
        Type::IdUuid => serialize_uuid(value, path),
        Type::Text | Type::Date => value
            .as_str()
            .map(|value| SqlValue::Text(value.to_string()))
            .ok_or_else(|| invalid_type(path, "a string")),
        Type::DateTime => datetime_to_epoch(value)
            .map(SqlValue::Integer)
            .ok_or_else(|| invalid_type(path, "RFC3339 or Unix seconds")),
        Type::JsonB => Ok(SqlValue::Text(value.to_string())),
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
            Ok(SqlValue::Blob(bytes))
        }
    }
}

fn serialize_uuid(value: &JsonValue, path: &str) -> Result<SqlValue, Error> {
    let raw = value.as_str().ok_or_else(|| invalid_type(path, "a UUID"))?;
    uuid::Uuid::parse_str(raw).map_err(|_| invalid_type(path, "a UUID"))?;
    Ok(SqlValue::Text(raw.to_owned()))
}

fn insert_statement(table: &str, values: Vec<(String, SqlValue)>, path: String) -> Insert {
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
    Insert {
        sql,
        args: values.into_iter().map(|(_, value)| value).collect(),
        path,
    }
}

fn format_row(
    context: &typecheck::Context,
    table: &typecheck::Table,
    physical: &ReturnedRow,
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
    physical: &ReturnedRow,
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
            .and_then(JsonValue::as_i64)
            .map(|value| JsonValue::Bool(value != 0))
            .unwrap_or(JsonValue::Null)),
        ast::ColumnType::Json
        | ast::ColumnType::JsonTyped(_)
        | ast::ColumnType::List(_)
        | ast::ColumnType::Dict(_) => match physical.get(physical_name) {
            Some(JsonValue::String(raw)) => serde_json::from_str(raw).map_err(Error::Json),
            value => Ok(value.cloned().unwrap_or(JsonValue::Null)),
        },
        _ => Ok(physical
            .get(physical_name)
            .cloned()
            .unwrap_or(JsonValue::Null)),
    }
}

fn reconstruct_constructed(
    context: &typecheck::Context,
    type_name: &str,
    physical_name: &str,
    physical: &ReturnedRow,
) -> Result<JsonValue, Error> {
    let Some(value) = physical.get(physical_name) else {
        return Ok(JsonValue::Null);
    };
    let JsonValue::String(tag) = value.clone() else {
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

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[derive(Debug)]
pub enum Error {
    InvalidInput(String),
    Json(serde_json::Error),
    Unsupported(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidInput(message) => write!(f, "invalid seed input: {}", message),
            Error::Json(error) => write!(f, "json error: {}", error),
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
