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
    Json(String),
    Blob(Vec<u8>),
}

#[derive(Debug, Serialize)]
pub struct Insert {
    pub sql: String,
    pub args: Vec<SqlValue>,
    pub path: String,
}

pub type ReturnedRow = serde_json::Map<String, JsonValue>;

/// Read on the same write transaction that will execute the fixture inserts.
pub const TRIGGER_TABLES_SQL: &str = "select json_group_object(lower(t.name), t.sql) as trigger_tables
    from main.sqlite_schema as t
    where t.type in ('table', 'view') and (exists (
        select 1 from main.sqlite_schema as tr where tr.type = 'trigger' and tr.tbl_name = t.name collate nocase
    ) or exists (
        select 1 from temp.sqlite_schema as tr where tr.type = 'trigger' and tr.tbl_name = t.name collate nocase
    ))";

pub struct InsertPolicies(HashMap<String, bool>);

impl InsertPolicies {
    pub fn from_trigger_tables(json: &str) -> Result<Self, serde_json::Error> {
        let tables: HashMap<String, String> = serde_json::from_str(json)?;
        Ok(Self(
            tables
                .into_iter()
                .map(|(name, ddl)| (name, has_replace_policy(&ddl)))
                .collect(),
        ))
    }

    fn insert_prefix(&self, table: &str, path: &str) -> Result<&'static str, Error> {
        match self.0.get(&table.to_ascii_lowercase()) {
            None => Ok("INSERT OR ABORT"),
            Some(false) => Ok("INSERT"),
            Some(true) => Err(Error::Unsupported(format!(
                "{path}: cannot seed '{table}' with both triggers and an ON CONFLICT REPLACE constraint"
            ))),
        }
    }
}

// SQLite's statement-level policy also overrides conflict handling inside triggers.
// A plain INSERT preserves triggers but cannot safely run against a REPLACE
// constraint. Detect the clause in persisted DDL, ignoring quotes and comments.
fn has_replace_policy(ddl: &str) -> bool {
    let mut bytes = ddl.as_bytes();
    let mut matched = 0;
    let keywords: [&[u8]; 3] = [b"on", b"conflict", b"replace"];
    let identifier_byte = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'$') || c >= 128;
    while let Some((&first, rest)) = bytes.split_first() {
        bytes = match first {
            c if c.is_ascii_whitespace() => rest,
            b'-' if rest.starts_with(b"-") => {
                &rest[rest.iter().position(|&c| c == b'\n').unwrap_or(rest.len())..]
            }
            b'/' if rest.starts_with(b"*") => {
                let body = &rest[1..];
                &body[body
                    .windows(2)
                    .position(|w| w == b"*/")
                    .map_or(body.len(), |i| i + 2)..]
            }
            quote @ (b'\'' | b'"' | b'`' | b'[') => {
                matched = 0;
                let end = if quote == b'[' { b']' } else { quote };
                let mut quoted = rest;
                loop {
                    let Some(i) = quoted.iter().position(|&c| c == end) else {
                        break &quoted[quoted.len()..];
                    };
                    quoted = &quoted[i + 1..];
                    if quote == b'[' || !quoted.starts_with(&[end]) {
                        break quoted;
                    }
                    quoted = &quoted[1..];
                }
            }
            c if identifier_byte(c) => {
                let end = 1 + rest.iter().take_while(|&&c| identifier_byte(c)).count();
                let token = &bytes[..end];
                if token.eq_ignore_ascii_case(keywords[matched]) {
                    matched += 1;
                    if matched == keywords.len() {
                        return true;
                    }
                } else {
                    matched = usize::from(token.eq_ignore_ascii_case(keywords[0]));
                }
                &bytes[end..]
            }
            _ => {
                matched = 0;
                rest
            }
        };
    }
    false
}

struct Engine<'a> {
    context: &'a typecheck::Context,
    policies: &'a InsertPolicies,
}

/// Run the fixture against one schema. The host executes each insert in the same
/// transaction and returns its physical row before dependent inserts are prepared.
pub async fn ingest<F, Fut, E>(
    context: &typecheck::Context,
    namespace: &str,
    input: JsonValue,
    policies: &InsertPolicies,
    execute: F,
) -> Result<JsonValue, E>
where
    F: FnMut(Insert) -> Fut,
    Fut: Future<Output = Result<ReturnedRow, E>>,
    E: From<Error>,
{
    let JsonValue::Object(input) = input else {
        return Err(Error::InvalidInput("seed input must be an object".to_string()).into());
    };
    ingest_entries(context, namespace, input, policies, execute).await
}

/// Like [`ingest`], but preserve the host's top-level entry order.
pub async fn ingest_entries<F, Fut, E>(
    context: &typecheck::Context,
    namespace: &str,
    input: impl IntoIterator<Item = (String, JsonValue)>,
    policies: &InsertPolicies,
    mut execute: F,
) -> Result<JsonValue, E>
where
    F: FnMut(Insert) -> Fut,
    Fut: Future<Output = Result<ReturnedRow, E>>,
    E: From<Error>,
{
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
    let engine = Engine { context, policies };
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
        let inserted = insert_rows(&mut execute, &engine, table, rows, table_name.clone()).await?;
        response.insert(table_name, JsonValue::Array(inserted));
    }
    Ok(JsonValue::Object(response))
}

async fn insert_rows<F, Fut, E>(
    execute: &mut F,
    engine: &Engine<'_>,
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
            .push(insert_row(execute, engine, table, row, format!("{}[{}]", path, index)).await?);
    }
    Ok(inserted)
}

fn insert_row<'a, F, Fut, E>(
    execute: &'a mut F,
    engine: &'a Engine<'_>,
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
        let context = engine.context;
        let table_name = ast::get_tablename(&table.record.name, &table.record.fields);
        let insert_prefix = engine.policies.insert_prefix(&table_name, &path)?;
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
                engine,
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
        let physical_row = execute(insert_statement(
            context,
            table,
            &table_name,
            insert_prefix,
            physical_values,
            path.clone(),
        ))
        .await?;
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
                    insert_rows(execute, engine, linked_table, nested_rows, nested_path).await?,
                )
            } else {
                insert_row(
                    execute,
                    engine,
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
            // A required SQL column can still hold a JSON null if its JSON
            // type allows it. Use the normal JSON validation and serialization.
            return match column.type_ {
                ast::ColumnType::Json | ast::ColumnType::JsonTyped(_) => {
                    serialize_value(context, &column.type_, physical_name, value, path, output)
                }
                _ => Err(Error::InvalidInput(format!("{} cannot be null", path))),
            };
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
        ColumnType::Json => SqlValue::Json(value.to_string()),
        ColumnType::JsonTyped(_) | ColumnType::List(_) | ColumnType::Dict(_) => {
            SqlValue::Json(normalize_json(context, type_, value, path)?.to_string())
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

fn normalize_json(
    context: &typecheck::Context,
    type_: &ast::ColumnType,
    value: &JsonValue,
    path: &str,
) -> Result<JsonValue, Error> {
    use ast::ColumnType;
    match type_ {
        ColumnType::Json => Ok(value.clone()),
        ColumnType::JsonTyped(inner) => normalize_json(context, inner, value, path),
        ColumnType::Nullable(_) if value.is_null() => Ok(JsonValue::Null),
        ColumnType::Nullable(inner) => normalize_json(context, inner, value, path),
        ColumnType::List(inner) => {
            let values = value
                .as_array()
                .ok_or_else(|| invalid_type(path, "an array"))?;
            values
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    normalize_json(context, inner, item, &format!("{path}[{index}]"))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(JsonValue::Array)
        }
        ColumnType::Dict(inner) => {
            let values = value
                .as_object()
                .ok_or_else(|| invalid_type(path, "an object"))?;
            values
                .iter()
                .map(|(key, item)| {
                    normalize_json(context, inner, item, &format!("{path}[{key:?}]"))
                        .map(|item| (key.clone(), item))
                })
                .collect::<Result<serde_json::Map<_, _>, _>>()
                .map(JsonValue::Object)
        }
        ColumnType::Custom(type_name) => {
            let Constructed {
                tag,
                fields,
                is_enum,
            } = constructed_fields(context, type_name, value, path)?;
            if is_enum {
                return Ok(JsonValue::String(tag.to_owned()));
            }
            let mut result = serde_json::Map::new();
            result.insert("_type".to_owned(), JsonValue::String(tag.to_owned()));
            for (column, item) in fields {
                let normalized = if column.nullable && item.is_null() {
                    JsonValue::Null
                } else {
                    normalize_json(
                        context,
                        &column.type_,
                        item,
                        &format!("{path}.{}", column.name),
                    )?
                };
                result.insert(column.name.clone(), normalized);
            }
            Ok(JsonValue::Object(result))
        }
        // Reuse fixture scalar validation without changing the JSON representation.
        scalar => {
            serialize_value(context, scalar, "", value, path, &mut Vec::new())?;
            Ok(value.clone())
        }
    }
}

struct Constructed<'a> {
    tag: &'a str,
    fields: Vec<(&'a ast::Column, &'a JsonValue)>,
    is_enum: bool,
}

fn constructed_fields<'a>(
    context: &'a typecheck::Context,
    type_name: &str,
    value: &'a JsonValue,
    path: &str,
) -> Result<Constructed<'a>, Error> {
    let (tag, object) = match value {
        JsonValue::String(tag) => (tag.as_str(), None),
        JsonValue::Object(object) => {
            let tag = object
                .get("_type")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| {
                    Error::InvalidInput(format!("{} must use '_type' as its discriminator", path))
                })?;
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
    let mut values = Vec::new();
    let columns = variant
        .fields
        .iter()
        .flatten()
        .filter_map(|field| match field {
            ast::Field::Column(column) => Some(column),
            _ => None,
        });
    if let Some(object) = object {
        for key in object.keys().filter(|key| key.as_str() != "_type") {
            if !columns.clone().any(|column| column.name == *key) {
                return Err(Error::InvalidInput(format!(
                    "{}.{} is not a field of variant '{}'",
                    path, key, tag
                )));
            }
        }
        for column in columns {
            let Some(value) = object.get(&column.name) else {
                if !column.nullable && !matches!(column.type_, ast::ColumnType::Nullable(_)) {
                    return Err(Error::InvalidInput(format!(
                        "{}.{} is required for variant '{}'",
                        path, column.name, tag
                    )));
                }
                continue;
            };
            values.push((column, value));
        }
    } else if columns.count() != 0 {
        return Err(Error::InvalidInput(format!(
            "{} variant '{}' requires fields",
            path, tag
        )));
    }
    Ok(Constructed {
        tag,
        fields: values,
        is_enum: is_pure_enum(variants),
    })
}

fn is_pure_enum(variants: &[ast::Variant]) -> bool {
    variants.iter().all(|variant| variant.fields.is_none())
}

fn serialize_constructed(
    context: &typecheck::Context,
    type_name: &str,
    physical_name: &str,
    value: &JsonValue,
    path: &str,
    output: &mut Vec<(String, SqlValue)>,
) -> Result<(), Error> {
    let Constructed { tag, fields, .. } = constructed_fields(context, type_name, value, path)?;
    output.push((physical_name.to_string(), SqlValue::Text(tag.to_string())));
    for (column, value) in fields {
        serialize_column(
            context,
            column,
            &format!("{physical_name}__{}", column.name),
            value,
            &format!("{path}.{}", column.name),
            output,
        )?;
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
        Type::JsonB => Ok(SqlValue::Json(value.to_string())),
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

fn insert_statement(
    context: &typecheck::Context,
    table: &typecheck::Table,
    table_name: &str,
    insert_prefix: &str,
    values: Vec<(String, SqlValue)>,
    path: String,
) -> Insert {
    let table_name = quote(table_name);
    let mut sql = if values.is_empty() {
        format!("{insert_prefix} INTO main.{table_name} DEFAULT VALUES")
    } else {
        let columns = values
            .iter()
            .map(|(name, _)| quote(name))
            .collect::<Vec<_>>()
            .join(", ");
        let placeholders = values
            .iter()
            .map(|(_, value)| {
                if matches!(value, SqlValue::Json(_)) {
                    "jsonb(?)"
                } else {
                    "?"
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("{insert_prefix} INTO main.{table_name} ({columns}) VALUES ({placeholders})")
    };
    // Decode both JSONB and text JSON defaults in SQLite, before crossing the
    // driver boundary. Keep the other returned fields in their native types.
    let physical = crate::db::diff::physical_columns(context, &table.record.fields);
    let returning = physical
        .info
        .iter()
        .map(|column| {
            let name = quote(&column.name);
            if physical.json.contains(&column.name) {
                format!("json({table_name}.{name}) AS {name}")
            } else {
                format!("{table_name}.{name}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    sql.push_str(" RETURNING ");
    sql.push_str(&returning);
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
    if is_pure_enum(variants) {
        return Ok(JsonValue::String(tag));
    }
    let mut result = serde_json::Map::new();
    result.insert("_type".to_string(), JsonValue::String(tag));
    for field in variant.fields.iter().flatten() {
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
