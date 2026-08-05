use crate::ast;
use crate::sync::SessionValue;
use crate::typecheck::{self, Type};
use chrono::DateTime;
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};

pub fn prepare_session(
    context: &typecheck::Context,
    value: &JsonValue,
) -> Result<HashMap<String, SessionValue>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "Session must be an object".to_string())?;
    let Some(session) = context.session.as_ref() else {
        return Ok(object
            .iter()
            .map(|(name, value)| (name.clone(), untyped_session_value(value)))
            .collect());
    };
    if session.fields.is_empty() && session.start.is_none() {
        return Ok(object
            .iter()
            .map(|(name, value)| (name.clone(), untyped_session_value(value)))
            .collect());
    }
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

fn untyped_session_value(value: &JsonValue) -> SessionValue {
    match value {
        JsonValue::Null => SessionValue::Null,
        JsonValue::Bool(value) => SessionValue::Integer(if *value { 1 } else { 0 }),
        JsonValue::Number(value) => value
            .as_i64()
            .map(SessionValue::Integer)
            .or_else(|| value.as_f64().map(SessionValue::Real))
            .unwrap_or(SessionValue::Null),
        JsonValue::String(value) => SessionValue::Text(value.clone()),
        JsonValue::Array(_) | JsonValue::Object(_) => SessionValue::Text(value.to_string()),
    }
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
            let accepts_string_tag = variants.iter().all(|variant| variant.fields.is_none());
            let tag = value
                .as_str()
                .filter(|_| accepts_string_tag)
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
                let object = value
                    .as_object()
                    .ok_or_else(|| format!("Session field '{}' must be an object", column.name))?;
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
        ast::ColumnType::String | ast::ColumnType::IdUuid { .. } => value
            .as_str()
            .map(|value| SessionValue::Text(value.to_string())),
        ast::ColumnType::Int | ast::ColumnType::IdInt { .. } => {
            value.as_i64().map(SessionValue::Integer)
        }
        ast::ColumnType::Float => value.as_f64().map(SessionValue::Real),
        ast::ColumnType::Bool => value
            .as_bool()
            .map(|value| SessionValue::Integer(if value { 1 } else { 0 }))
            .or_else(|| {
                value
                    .as_i64()
                    .filter(|value| matches!(value, 0 | 1))
                    .map(SessionValue::Integer)
            }),
        ast::ColumnType::DateTime => value.as_i64().map(SessionValue::Integer).or_else(|| {
            value
                .as_str()
                .and_then(|value| {
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
        ast::ColumnType::ForeignKey { .. } => type_
            .to_serialization_type()
            .into_concrete()
            .and_then(|serialization_type| concrete_session_value(&serialization_type, value)),
        ast::ColumnType::Json
        | ast::ColumnType::JsonTyped(_)
        | ast::ColumnType::List(_)
        | ast::ColumnType::Dict(_)
        | ast::ColumnType::Custom(_) => Some(SessionValue::Text(value.to_string())),
        ast::ColumnType::Nullable(inner) => scalar_session_value(inner, value),
    }
}

fn concrete_session_value(
    type_: &ast::ConcreteSerializationType,
    value: &JsonValue,
) -> Option<SessionValue> {
    match type_ {
        ast::ConcreteSerializationType::Integer | ast::ConcreteSerializationType::IdInt => {
            value.as_i64().map(SessionValue::Integer)
        }
        ast::ConcreteSerializationType::Real => value.as_f64().map(SessionValue::Real),
        ast::ConcreteSerializationType::Text
        | ast::ConcreteSerializationType::Date
        | ast::ConcreteSerializationType::IdUuid => value
            .as_str()
            .map(|value| SessionValue::Text(value.to_string())),
        ast::ConcreteSerializationType::DateTime => {
            value.as_i64().map(SessionValue::Integer).or_else(|| {
                value
                    .as_str()
                    .and_then(|value| {
                        value.parse::<i64>().ok().or_else(|| {
                            DateTime::parse_from_rfc3339(value)
                                .ok()
                                .map(|date| date.timestamp())
                        })
                    })
                    .map(SessionValue::Integer)
            })
        }
        ast::ConcreteSerializationType::Blob
        | ast::ConcreteSerializationType::VectorBlob { .. } => {
            value.as_array().and_then(|values| {
                values
                    .iter()
                    .map(|value| value.as_u64().and_then(|value| u8::try_from(value).ok()))
                    .collect::<Option<Vec<_>>>()
                    .map(SessionValue::Blob)
            })
        }
        ast::ConcreteSerializationType::JsonB => Some(SessionValue::Text(value.to_string())),
    }
}

fn fill_descendants_with_null(
    context: &typecheck::Context,
    type_: &ast::ColumnType,
    physical_name: &str,
    flattened: &mut HashMap<String, SessionValue>,
) {
    fill_descendants_with_null_inner(
        context,
        type_,
        physical_name,
        flattened,
        &mut HashSet::new(),
    );
}

fn fill_descendants_with_null_inner(
    context: &typecheck::Context,
    type_: &ast::ColumnType,
    physical_name: &str,
    flattened: &mut HashMap<String, SessionValue>,
    visiting: &mut HashSet<String>,
) {
    let Some(type_name) = type_.get_custom_type_name() else {
        return;
    };
    if !visiting.insert(type_name.to_string()) {
        return;
    }
    let Some((_, Type::OneOf { variants })) = context.types.get(type_name) else {
        visiting.remove(type_name);
        return;
    };
    fill_variant_descendants_with_null_inner(context, variants, physical_name, flattened, visiting);
    visiting.remove(type_name);
}

fn fill_variant_descendants_with_null(
    context: &typecheck::Context,
    variants: &[ast::Variant],
    physical_name: &str,
    flattened: &mut HashMap<String, SessionValue>,
) {
    fill_variant_descendants_with_null_inner(
        context,
        variants,
        physical_name,
        flattened,
        &mut HashSet::new(),
    );
}

fn fill_variant_descendants_with_null_inner(
    context: &typecheck::Context,
    variants: &[ast::Variant],
    physical_name: &str,
    flattened: &mut HashMap<String, SessionValue>,
    visiting: &mut HashSet<String>,
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
                fill_descendants_with_null_inner(
                    context,
                    &column.type_,
                    &nested_name,
                    flattened,
                    visiting,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_session_is_valid_without_a_session_schema() {
        let mut schema = ast::Schema::default();
        crate::parser::run(
            "schema.pyre",
            "record Item {\n    @public\n    id Int @id\n}",
            &mut schema,
        )
        .unwrap();
        let context = typecheck::check_schema(&ast::Database {
            schemas: vec![schema],
        })
        .unwrap();

        assert_eq!(
            prepare_session(&context, &serde_json::json!({})),
            Ok(HashMap::new())
        );
    }

    #[test]
    fn legacy_flat_session_is_preserved_without_a_session_schema() {
        let mut schema = ast::Schema::default();
        crate::parser::run(
            "schema.pyre",
            "record Item {\n    id Int @id\n    ownerId Int\n    @allow(query) { ownerId == Session.userId }\n    @allow(insert, update, delete) { False }\n}",
            &mut schema,
        )
        .unwrap();
        let context = typecheck::check_schema(&ast::Database {
            schemas: vec![schema],
        })
        .unwrap();

        assert_eq!(
            prepare_session(&context, &serde_json::json!({ "userId": 7 })),
            Ok(HashMap::from([(
                "userId".to_string(),
                SessionValue::Integer(7)
            )]))
        );
    }

    #[test]
    fn boolean_session_numbers_are_limited_to_sql_boolean_values() {
        assert_eq!(
            scalar_session_value(&ast::ColumnType::Bool, &serde_json::json!(1)),
            Some(SessionValue::Integer(1))
        );
        assert_eq!(
            scalar_session_value(&ast::ColumnType::Bool, &serde_json::json!(2)),
            None
        );
    }

    #[test]
    fn foreign_key_serialization_uses_its_physical_scalar_type() {
        assert_eq!(
            concrete_session_value(
                &ast::ConcreteSerializationType::Text,
                &JsonValue::String("key".to_string()),
            ),
            Some(SessionValue::Text("key".to_string()))
        );
        assert_eq!(
            concrete_session_value(
                &ast::ConcreteSerializationType::Real,
                &serde_json::json!(1.5),
            ),
            Some(SessionValue::Real(1.5))
        );
        assert_eq!(
            concrete_session_value(
                &ast::ConcreteSerializationType::DateTime,
                &JsonValue::String("1970-01-01T00:00:05Z".to_string()),
            ),
            Some(SessionValue::Integer(5))
        );
        assert_eq!(
            concrete_session_value(
                &ast::ConcreteSerializationType::IdUuid,
                &JsonValue::String("uuid".to_string()),
            ),
            Some(SessionValue::Text("uuid".to_string()))
        );
    }
}
