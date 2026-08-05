use crate::ast;
use crate::sync::SessionValue;
use crate::typecheck::{self, Type};
use chrono::DateTime;
use serde_json::Value as JsonValue;
use std::collections::HashMap;

pub fn prepare_session(
    context: &typecheck::Context,
    value: &JsonValue,
) -> Result<HashMap<String, SessionValue>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "Session must be an object".to_string())?;
    let session = context
        .session
        .as_ref()
        .ok_or_else(|| "Schema does not define a Session".to_string())?;
    let mut flattened = HashMap::new();

    for field in &session.fields {
        let ast::Field::Column(column) = field else {
            continue;
        };
        let value = object.get(&column.name).unwrap_or(&JsonValue::Null);
        if value.is_null() && !column.nullable {
            return Err(format!("Missing required Session field '{}'", column.name));
        }
        flatten_column(context, column, value, &column.name, &mut flattened)?;
    }

    Ok(flattened)
}

fn flatten_column(
    context: &typecheck::Context,
    column: &ast::Column,
    value: &JsonValue,
    physical_name: &str,
    flattened: &mut HashMap<String, SessionValue>,
) -> Result<(), String> {
    if value.is_null() {
        flattened.insert(physical_name.to_string(), SessionValue::Null);
        fill_descendants_with_null(context, &column.type_, physical_name, flattened);
        return Ok(());
    }

    let type_ = unwrap_nullable(&column.type_);
    if let Some(type_name) = type_.get_custom_type_name() {
        if let Some((_, Type::OneOf { variants })) = context.types.get(type_name) {
            let tag = value
                .as_str()
                .or_else(|| value.get("_type").and_then(JsonValue::as_str))
                .ok_or_else(|| {
                    format!(
                        "Session field '{}' must contain a tagged-union discriminator",
                        column.name
                    )
                })?;
            let variant = variants
                .iter()
                .find(|variant| variant.name == tag)
                .ok_or_else(|| format!("Unknown variant '{}' for Session.{}", tag, column.name))?;
            flattened.insert(
                physical_name.to_string(),
                SessionValue::Text(tag.to_string()),
            );
            fill_variant_descendants_with_null(context, variants, physical_name, flattened);

            if let Some(fields) = &variant.fields {
                let object = value.as_object().ok_or_else(|| {
                    format!("Session field '{}' must be an object", column.name)
                })?;
                for field in fields {
                    let ast::Field::Column(payload) = field else {
                        continue;
                    };
                    let payload_value = object.get(&payload.name).unwrap_or(&JsonValue::Null);
                    if payload_value.is_null() && !payload.nullable {
                        return Err(format!(
                            "Missing required Session field '{}.{}.{}'",
                            column.name, tag, payload.name
                        ));
                    }
                    flatten_column(
                        context,
                        payload,
                        payload_value,
                        &format!("{}__{}", physical_name, payload.name),
                        flattened,
                    )?;
                }
            }
            return Ok(());
        }
    }

    flattened.insert(
        physical_name.to_string(),
        scalar_session_value(type_, value).ok_or_else(|| {
            format!(
                "Invalid value for Session field '{}' of type {}",
                column.name,
                column.type_.to_string()
            )
        })?,
    );
    Ok(())
}

fn unwrap_nullable(type_: &ast::ColumnType) -> &ast::ColumnType {
    match type_ {
        ast::ColumnType::Nullable(inner) => unwrap_nullable(inner),
        other => other,
    }
}

fn scalar_session_value(type_: &ast::ColumnType, value: &JsonValue) -> Option<SessionValue> {
    match type_ {
        ast::ColumnType::String | ast::ColumnType::IdUuid { .. } => {
            value.as_str().map(|value| SessionValue::Text(value.to_string()))
        }
        ast::ColumnType::Int
        | ast::ColumnType::IdInt { .. }
        | ast::ColumnType::ForeignKey {
            serialization_type: Some(ast::ConcreteSerializationType::Integer),
            ..
        } => value.as_i64().map(SessionValue::Integer),
        ast::ColumnType::Float => value.as_f64().map(SessionValue::Real),
        ast::ColumnType::Bool => value
            .as_bool()
            .map(|value| SessionValue::Integer(if value { 1 } else { 0 }))
            .or_else(|| value.as_i64().map(SessionValue::Integer)),
        ast::ColumnType::DateTime => value
            .as_i64()
            .map(SessionValue::Integer)
            .or_else(|| {
                value.as_str().and_then(|value| {
                    value.parse::<i64>().ok().or_else(|| {
                        DateTime::parse_from_rfc3339(value)
                            .ok()
                            .map(|date| date.timestamp())
                    })
                })
                .map(SessionValue::Integer)
            }),
        ast::ColumnType::Date => value
            .as_str()
            .map(|value| SessionValue::Text(value.to_string())),
        ast::ColumnType::Json
        | ast::ColumnType::JsonTyped(_)
        | ast::ColumnType::List(_)
        | ast::ColumnType::Dict(_)
        | ast::ColumnType::Custom(_)
        | ast::ColumnType::ForeignKey { .. } => Some(SessionValue::Text(value.to_string())),
        ast::ColumnType::Nullable(inner) => scalar_session_value(inner, value),
    }
}

fn fill_descendants_with_null(
    context: &typecheck::Context,
    type_: &ast::ColumnType,
    physical_name: &str,
    flattened: &mut HashMap<String, SessionValue>,
) {
    let Some(type_name) = type_.get_custom_type_name() else {
        return;
    };
    let Some((_, Type::OneOf { variants })) = context.types.get(type_name) else {
        return;
    };
    fill_variant_descendants_with_null(context, variants, physical_name, flattened);
}

fn fill_variant_descendants_with_null(
    context: &typecheck::Context,
    variants: &[ast::Variant],
    physical_name: &str,
    flattened: &mut HashMap<String, SessionValue>,
) {
    for variant in variants {
        if let Some(fields) = &variant.fields {
            for field in fields {
                let ast::Field::Column(column) = field else {
                    continue;
                };
                let nested_name = format!("{}__{}", physical_name, column.name);
                flattened
                    .entry(nested_name.clone())
                    .or_insert(SessionValue::Null);
                fill_descendants_with_null(context, &column.type_, &nested_name, flattened);
            }
        }
    }
}
