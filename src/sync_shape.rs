use crate::ast;
use crate::sync_deltas::AffectedRowTableGroup;
use crate::typecheck;
use serde_json::{Map, Value as JsonValue};

#[derive(Debug)]
pub struct SyncShapeError {
    pub table_name: String,
    pub column_name: String,
    pub row_index: usize,
    pub source: serde_json::Error,
}

impl std::fmt::Display for SyncShapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid JSON in sync row {} for {}.{}: {}",
            self.row_index, self.table_name, self.column_name, self.source
        )
    }
}

impl std::error::Error for SyncShapeError {}

pub fn normalize_json_columns(
    table_groups: &[AffectedRowTableGroup],
    context: &typecheck::Context,
) -> Result<Vec<AffectedRowTableGroup>, SyncShapeError> {
    let mut normalized = table_groups.to_vec();

    for table_group in &mut normalized {
        let Some(table) = context.tables.values().find(|table| {
            ast::get_tablename(&table.record.name, &table.record.fields) == table_group.table_name
        }) else {
            continue;
        };
        let header_indexes = table_group
            .headers
            .iter()
            .enumerate()
            .map(|(index, header)| (header.as_str(), index))
            .collect::<std::collections::HashMap<_, _>>();

        for (row_index, row) in table_group.rows.iter_mut().enumerate() {
            for field in &table.record.fields {
                if let ast::Field::Column(column) = field {
                    normalize_column_value(
                        context,
                        &table_group.table_name,
                        &header_indexes,
                        row,
                        row_index,
                        &column.name,
                        &column.type_,
                    )?;
                }
            }
        }
    }

    Ok(normalized)
}

fn normalize_column_value(
    context: &typecheck::Context,
    table_name: &str,
    header_indexes: &std::collections::HashMap<&str, usize>,
    row: &mut [JsonValue],
    row_index: usize,
    prefix: &str,
    column_type: &ast::ColumnType,
) -> Result<(), SyncShapeError> {
    if column_type.is_json_like() {
        let Some(index) = header_indexes.get(prefix).copied() else {
            return Ok(());
        };
        let JsonValue::String(raw) = &row[index] else {
            return Ok(());
        };
        let parsed = serde_json::from_str(raw).map_err(|source| SyncShapeError {
            table_name: table_name.to_string(),
            column_name: prefix.to_string(),
            row_index,
            source,
        })?;
        row[index] = parse_nested_json_container(parsed).map_err(|source| SyncShapeError {
            table_name: table_name.to_string(),
            column_name: prefix.to_string(),
            row_index,
            source,
        })?;
        return Ok(());
    }

    let Some(type_name) = column_type.get_custom_type_name() else {
        return Ok(());
    };
    let Some(index) = header_indexes.get(prefix).copied() else {
        return Ok(());
    };
    let JsonValue::String(variant_name) = &row[index] else {
        return Ok(());
    };
    let Some((_definfo, typecheck::Type::OneOf { variants })) = context.types.get(type_name) else {
        return Ok(());
    };
    let Some(variant) = variants
        .iter()
        .find(|variant| variant.name == *variant_name)
    else {
        return Ok(());
    };

    if let Some(fields) = &variant.fields {
        for field in fields {
            if let ast::Field::Column(column) = field {
                normalize_column_value(
                    context,
                    table_name,
                    header_indexes,
                    row,
                    row_index,
                    &format!("{}__{}", prefix, column.name),
                    &column.type_,
                )?;
            }
        }
    }

    Ok(())
}

fn parse_nested_json_container(value: JsonValue) -> Result<JsonValue, serde_json::Error> {
    let JsonValue::String(raw) = value else {
        return Ok(value);
    };
    let trimmed = raw.trim();

    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        serde_json::from_str(trimmed)
    } else {
        Ok(JsonValue::String(raw))
    }
}

pub fn reshape_table_groups(
    table_groups: &[AffectedRowTableGroup],
    context: &typecheck::Context,
) -> Vec<AffectedRowTableGroup> {
    table_groups
        .iter()
        .map(|group| reshape_table_group(group, context))
        .collect()
}

fn reshape_table_group(
    table_group: &AffectedRowTableGroup,
    context: &typecheck::Context,
) -> AffectedRowTableGroup {
    let Some(table) = context.tables.values().find(|table| {
        ast::get_tablename(&table.record.name, &table.record.fields) == table_group.table_name
    }) else {
        return table_group.clone();
    };

    let output_headers = table
        .record
        .fields
        .iter()
        .filter_map(|field| match field {
            ast::Field::Column(column) => Some(column.name.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();

    let rows = table_group
        .rows
        .iter()
        .map(|row| {
            let row_object = row_array_to_object(&table_group.headers, row);
            output_headers
                .iter()
                .map(|header| reshape_field_value(context, table, &row_object, header))
                .collect()
        })
        .collect();

    AffectedRowTableGroup {
        table_name: table_group.table_name.clone(),
        headers: output_headers,
        rows,
    }
}

fn row_array_to_object(headers: &[String], row: &[JsonValue]) -> Map<String, JsonValue> {
    let mut object = Map::with_capacity(headers.len());

    for (index, header) in headers.iter().enumerate() {
        if let Some(value) = row.get(index) {
            object.insert(header.clone(), value.clone());
        }
    }

    object
}

fn reshape_field_value(
    context: &typecheck::Context,
    table: &typecheck::Table,
    row: &Map<String, JsonValue>,
    field_name: &str,
) -> JsonValue {
    let column = table.record.fields.iter().find_map(|field| match field {
        ast::Field::Column(column) if column.name == field_name => Some(column),
        _ => None,
    });

    match column {
        Some(column) => reshape_column_value(context, row, field_name, &column.type_),
        None => row.get(field_name).cloned().unwrap_or(JsonValue::Null),
    }
}

fn reshape_column_value(
    context: &typecheck::Context,
    row: &Map<String, JsonValue>,
    prefix: &str,
    column_type: &ast::ColumnType,
) -> JsonValue {
    let value = row.get(prefix).cloned().unwrap_or(JsonValue::Null);

    let Some(type_name) = column_type.get_custom_type_name() else {
        return value;
    };

    match value {
        JsonValue::Null => JsonValue::Null,
        JsonValue::Object(_) => value,
        JsonValue::String(variant_name) => {
            let Some((_definfo, type_)) = context.types.get(type_name) else {
                return JsonValue::String(variant_name);
            };

            let typecheck::Type::OneOf { variants } = type_ else {
                return JsonValue::String(variant_name);
            };

            let Some(variant) = variants.iter().find(|variant| variant.name == variant_name) else {
                return JsonValue::String(variant_name);
            };

            let mut object = Map::new();
            object.insert("_type".to_string(), JsonValue::String(variant_name));

            if let Some(fields) = &variant.fields {
                for field in fields {
                    if let ast::Field::Column(column) = field {
                        let nested_key = format!("{}__{}", prefix, column.name);
                        object.insert(
                            column.name.clone(),
                            reshape_column_value(context, row, &nested_key, &column.type_),
                        );
                    }
                }
            }

            JsonValue::Object(object)
        }
        _ => value,
    }
}
