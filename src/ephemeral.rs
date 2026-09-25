//! Canonical executable contract for complete ephemeral values and top-level patches.

use crate::{ast, error::DefInfo, typecheck};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contract {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<StateContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<StateContract>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub types: BTreeMap<String, TaggedUnion>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StateContract {
    pub fields: BTreeMap<String, FieldContract>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldContract {
    pub schema: ValueSchema,
    pub writable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<InitialValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValueSchema {
    #[serde(flatten)]
    pub type_: ValueType,
    #[serde(default)]
    pub nullable: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ValueType {
    String,
    Int,
    Float,
    Bool,
    DateTime,
    Date,
    Json,
    TypedJson { value: Box<ValueSchema> },
    List { item: Box<ValueSchema> },
    Dict { value: Box<ValueSchema> },
    IdInt,
    IdUuid,
    Custom { name: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaggedUnion {
    pub variants: BTreeMap<String, BTreeMap<String, ValueSchema>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum InitialValue {
    Now,
    Value(Value),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    ExpectedObject,
    UnknownField,
    MissingField,
    UnexpectedNull,
    InvalidType,
    UnknownVariant,
    DerivedField,
    MissingSessionField,
    UnknownState,
    InvalidContract,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationError {
    pub code: ErrorCode,
    pub path: Vec<String>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatchResult {
    pub value: Value,
    pub changed: bool,
}

impl Contract {
    pub fn from_context(context: &typecheck::Context) -> Result<Self, ValidationError> {
        let mut contract = Self {
            connection: None,
            shared: None,
            types: BTreeMap::new(),
        };

        for (name, (definition, type_)) in &context.types {
            let (DefInfo::Def(_), typecheck::Type::OneOf { variants }) = (definition, type_) else {
                continue;
            };
            let variants = variants
                .iter()
                .map(|variant| {
                    let fields = variant
                        .fields
                        .as_ref()
                        .map(|fields| {
                            fields
                                .iter()
                                .filter_map(|field| match field {
                                    ast::Field::Column(column) => Some((
                                        column.name.clone(),
                                        value_schema(&column.type_, column.nullable),
                                    )),
                                    _ => None,
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    (variant.name.clone(), fields)
                })
                .collect();
            contract
                .types
                .insert(name.clone(), TaggedUnion { variants });
        }

        contract.connection = context
            .states
            .get("Connection")
            .map(resolve_state)
            .transpose()?;
        contract.shared = context
            .states
            .get("Shared")
            .map(resolve_state)
            .transpose()?;
        Ok(contract)
    }

    pub fn initialize_shared(&self, now_seconds: i64) -> Result<Value, Vec<ValidationError>> {
        self.initialize(self.shared.as_ref(), None, now_seconds, "Shared")
    }

    pub fn initialize_connection(
        &self,
        trusted_session: &Value,
        now_seconds: i64,
    ) -> Result<Value, Vec<ValidationError>> {
        self.initialize(
            self.connection.as_ref(),
            Some(trusted_session),
            now_seconds,
            "Connection",
        )
    }

    pub fn validate_complete(
        &self,
        state_name: &str,
        value: &Value,
    ) -> Result<Value, Vec<ValidationError>> {
        let state = self.state(state_name)?;
        normalize_state(self, state, value)
    }

    pub fn apply_patch(
        &self,
        state_name: &str,
        current: &Value,
        patch: &Value,
    ) -> Result<PatchResult, Vec<ValidationError>> {
        let state = self.state(state_name)?;
        let normalized_current = normalize_state(self, state, current)?;
        let Value::Object(patch) = patch else {
            return Err(vec![error(
                ErrorCode::ExpectedObject,
                vec![],
                "patch must be a JSON object",
            )]);
        };

        let mut errors = Vec::new();
        let mut normalized_patch = BTreeMap::new();
        for (name, value) in patch {
            let Some(field) = state.fields.get(name) else {
                errors.push(error(
                    ErrorCode::UnknownField,
                    vec![name.clone()],
                    format!("unknown patch field '{name}'"),
                ));
                continue;
            };
            if !field.writable {
                errors.push(error(
                    ErrorCode::DerivedField,
                    vec![name.clone()],
                    format!("derived field '{name}' is not writable"),
                ));
                continue;
            }
            match normalize_value(self, &field.schema, value, vec![name.clone()]) {
                Ok(value) => {
                    normalized_patch.insert(name.clone(), value);
                }
                Err(mut field_errors) => errors.append(&mut field_errors),
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        let mut candidate = normalized_current.clone();
        let Value::Object(candidate_object) = &mut candidate else {
            unreachable!("normalized states are objects")
        };
        for (name, value) in normalized_patch {
            candidate_object.insert(name, value);
        }
        let value = normalize_state(self, state, &candidate)?;
        Ok(PatchResult {
            changed: value != normalized_current,
            value,
        })
    }

    /// Recomputes trusted derived fields while retaining every writable field.
    pub fn refresh_connection(
        &self,
        current: &Value,
        trusted_session: &Value,
        now_seconds: i64,
    ) -> Result<PatchResult, Vec<ValidationError>> {
        let state = self.state("Connection")?;
        let normalized_current = normalize_state(self, state, current)?;
        let mut value = self.initialize_connection(trusted_session, now_seconds)?;
        let (Value::Object(current), Value::Object(ref mut refreshed)) =
            (&normalized_current, &mut value)
        else {
            unreachable!("normalized states are objects")
        };
        for (name, field) in &state.fields {
            if field.writable {
                refreshed.insert(name.clone(), current[name].clone());
            }
        }
        Ok(PatchResult {
            changed: value != normalized_current,
            value,
        })
    }

    fn state(&self, name: &str) -> Result<&StateContract, Vec<ValidationError>> {
        let state = match name {
            "Connection" => self.connection.as_ref(),
            "Shared" => self.shared.as_ref(),
            _ => None,
        };
        state.ok_or_else(|| {
            vec![error(
                ErrorCode::UnknownState,
                vec![],
                format!("ephemeral state '{name}' is not declared"),
            )]
        })
    }

    fn initialize(
        &self,
        state: Option<&StateContract>,
        session: Option<&Value>,
        now_seconds: i64,
        state_name: &str,
    ) -> Result<Value, Vec<ValidationError>> {
        let state = state.ok_or_else(|| {
            vec![error(
                ErrorCode::UnknownState,
                vec![],
                format!("ephemeral state '{state_name}' is not declared"),
            )]
        })?;
        let session_object = match session {
            Some(Value::Object(object)) => Some(object),
            Some(_) => {
                return Err(vec![error(
                    ErrorCode::ExpectedObject,
                    vec!["Session".to_string()],
                    "trusted session must be a JSON object",
                )])
            }
            None => None,
        };

        let mut object = Map::new();
        let mut errors = Vec::new();
        for (name, field) in &state.fields {
            let raw = if let Some(source) = &field.derived_from {
                match session_object.and_then(|session| session.get(source)) {
                    Some(value) => value.clone(),
                    None => {
                        errors.push(error(
                            ErrorCode::MissingSessionField,
                            vec!["Session".to_string(), source.clone()],
                            format!("trusted session is missing field '{source}'"),
                        ));
                        continue;
                    }
                }
            } else if let Some(default) = &field.default {
                match default {
                    InitialValue::Now => Value::from(now_seconds),
                    InitialValue::Value(value) => value.clone(),
                }
            } else {
                Value::Null
            };
            match normalize_value(self, &field.schema, &raw, vec![name.clone()]) {
                Ok(value) => {
                    object.insert(name.clone(), value);
                }
                Err(mut field_errors) => errors.append(&mut field_errors),
            }
        }
        if errors.is_empty() {
            Ok(Value::Object(object))
        } else {
            Err(errors)
        }
    }
}

fn resolve_state(state: &typecheck::State) -> Result<StateContract, ValidationError> {
    let mut fields = BTreeMap::new();
    for field in &state.fields {
        let (column, writable, derived_from) = match field {
            ast::StateField::Writable(column) => (column, true, None),
            ast::StateField::Derived { column, source } => {
                (column, false, source.path.first().cloned())
            }
            _ => continue,
        };
        let default = column
            .directives
            .iter()
            .find_map(|directive| match directive {
                ast::ColumnDirective::Default { value, .. } => Some(value),
                _ => None,
            })
            .map(initial_value)
            .transpose()?;
        fields.insert(
            column.name.clone(),
            FieldContract {
                schema: value_schema(&column.type_, column.nullable),
                writable,
                default,
                derived_from,
            },
        );
    }
    Ok(StateContract { fields })
}

fn value_schema(type_: &ast::ColumnType, nullable: bool) -> ValueSchema {
    if let ast::ColumnType::Nullable(inner) = type_ {
        return value_schema(inner, true);
    }
    let type_ = match type_ {
        ast::ColumnType::String => ValueType::String,
        ast::ColumnType::Int => ValueType::Int,
        ast::ColumnType::Float => ValueType::Float,
        ast::ColumnType::Bool => ValueType::Bool,
        ast::ColumnType::DateTime => ValueType::DateTime,
        ast::ColumnType::Date => ValueType::Date,
        ast::ColumnType::Json => ValueType::Json,
        ast::ColumnType::JsonTyped(inner) => ValueType::TypedJson {
            value: Box::new(value_schema(inner, false)),
        },
        ast::ColumnType::List(inner) => ValueType::List {
            item: Box::new(value_schema(inner, false)),
        },
        ast::ColumnType::Dict(inner) => ValueType::Dict {
            value: Box::new(value_schema(inner, false)),
        },
        ast::ColumnType::IdInt { .. } => ValueType::IdInt,
        ast::ColumnType::IdUuid { .. } => ValueType::IdUuid,
        ast::ColumnType::ForeignKey {
            serialization_type, ..
        } => match serialization_type {
            Some(ast::ConcreteSerializationType::IdUuid | ast::ConcreteSerializationType::Text) => {
                ValueType::IdUuid
            }
            Some(ast::ConcreteSerializationType::Real) => ValueType::Float,
            Some(ast::ConcreteSerializationType::Date) => ValueType::Date,
            Some(ast::ConcreteSerializationType::DateTime) => ValueType::DateTime,
            _ => ValueType::IdInt,
        },
        ast::ColumnType::Custom(name) => ValueType::Custom { name: name.clone() },
        ast::ColumnType::Nullable(_) => unreachable!(),
    };
    ValueSchema { type_, nullable }
}

fn initial_value(value: &ast::DefaultValue) -> Result<InitialValue, ValidationError> {
    match value {
        ast::DefaultValue::Now => Ok(InitialValue::Now),
        ast::DefaultValue::Value(value) => query_value(value).map(InitialValue::Value),
    }
}

fn query_value(value: &ast::QueryValue) -> Result<Value, ValidationError> {
    match value {
        ast::QueryValue::String((_, value)) => Ok(Value::String(value.clone())),
        ast::QueryValue::Int((_, value)) => Ok(Value::from(*value)),
        ast::QueryValue::Float((_, value)) => Number::from_f64(f64::from(*value))
            .map(Value::Number)
            .ok_or_else(invalid_default),
        ast::QueryValue::Bool((_, value)) => Ok(Value::Bool(*value)),
        ast::QueryValue::Null(_) => Ok(Value::Null),
        ast::QueryValue::LiteralTypeValue((_, literal)) => {
            let mut object = Map::new();
            object.insert("_type".to_string(), Value::String(literal.name.clone()));
            for (name, value) in literal.fields.as_deref().unwrap_or_default() {
                object.insert(name.clone(), query_value(value)?);
            }
            Ok(Value::Object(object))
        }
        ast::QueryValue::Fn(_) | ast::QueryValue::Variable(_) => Err(invalid_default()),
    }
}

fn invalid_default() -> ValidationError {
    error(
        ErrorCode::InvalidContract,
        vec![],
        "state default is not executable",
    )
}

fn normalize_state(
    contract: &Contract,
    state: &StateContract,
    value: &Value,
) -> Result<Value, Vec<ValidationError>> {
    let Value::Object(object) = value else {
        return Err(vec![error(
            ErrorCode::ExpectedObject,
            vec![],
            "complete state must be a JSON object",
        )]);
    };
    let mut errors = Vec::new();
    for name in object.keys() {
        if !state.fields.contains_key(name) {
            errors.push(error(
                ErrorCode::UnknownField,
                vec![name.clone()],
                format!("unknown state field '{name}'"),
            ));
        }
    }
    let mut normalized = Map::new();
    for (name, field) in &state.fields {
        let Some(value) = object.get(name) else {
            errors.push(error(
                ErrorCode::MissingField,
                vec![name.clone()],
                format!("complete state is missing field '{name}'"),
            ));
            continue;
        };
        match normalize_value(contract, &field.schema, value, vec![name.clone()]) {
            Ok(value) => {
                normalized.insert(name.clone(), value);
            }
            Err(mut field_errors) => errors.append(&mut field_errors),
        }
    }
    if errors.is_empty() {
        Ok(Value::Object(normalized))
    } else {
        Err(errors)
    }
}

fn normalize_value(
    contract: &Contract,
    schema: &ValueSchema,
    value: &Value,
    path: Vec<String>,
) -> Result<Value, Vec<ValidationError>> {
    if value.is_null() {
        return if schema.nullable {
            Ok(Value::Null)
        } else {
            Err(vec![error(
                ErrorCode::UnexpectedNull,
                path,
                "value cannot be null",
            )])
        };
    }
    let invalid = || {
        vec![error(
            ErrorCode::InvalidType,
            path.clone(),
            format!("expected {}", type_name(&schema.type_)),
        )]
    };
    match &schema.type_ {
        ValueType::String | ValueType::IdUuid => value
            .as_str()
            .map(|value| Value::String(value.to_string()))
            .ok_or_else(invalid),
        ValueType::Int | ValueType::IdInt => value.as_i64().map(Value::from).ok_or_else(invalid),
        ValueType::Float => value
            .as_f64()
            .filter(|value| value.is_finite())
            .and_then(Number::from_f64)
            .map(Value::Number)
            .ok_or_else(invalid),
        ValueType::Bool => value.as_bool().map(Value::Bool).ok_or_else(invalid),
        ValueType::DateTime => value.as_i64().map(Value::from).ok_or_else(invalid),
        ValueType::Date => value
            .as_str()
            .filter(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok())
            .map(|value| Value::String(value.to_string()))
            .ok_or_else(invalid),
        ValueType::Json => Ok(value.clone()),
        ValueType::TypedJson { value: inner } => normalize_value(contract, inner, value, path),
        ValueType::List { item } => {
            let Value::Array(values) = value else {
                return Err(invalid());
            };
            let mut result = Vec::with_capacity(values.len());
            let mut errors = Vec::new();
            for (index, value) in values.iter().enumerate() {
                let mut item_path = path.clone();
                item_path.push(index.to_string());
                match normalize_value(contract, item, value, item_path) {
                    Ok(value) => result.push(value),
                    Err(mut item_errors) => errors.append(&mut item_errors),
                }
            }
            if errors.is_empty() {
                Ok(Value::Array(result))
            } else {
                Err(errors)
            }
        }
        ValueType::Dict { value: inner } => {
            let Value::Object(values) = value else {
                return Err(invalid());
            };
            let mut result = Map::new();
            let mut errors = Vec::new();
            for (name, value) in values {
                let mut value_path = path.clone();
                value_path.push(name.clone());
                match normalize_value(contract, inner, value, value_path) {
                    Ok(value) => {
                        result.insert(name.clone(), value);
                    }
                    Err(mut value_errors) => errors.append(&mut value_errors),
                }
            }
            if errors.is_empty() {
                Ok(Value::Object(result))
            } else {
                Err(errors)
            }
        }
        ValueType::Custom { name } => normalize_custom(contract, name, value, path),
    }
}

fn normalize_custom(
    contract: &Contract,
    name: &str,
    value: &Value,
    path: Vec<String>,
) -> Result<Value, Vec<ValidationError>> {
    let Some(definition) = contract.types.get(name) else {
        return Err(vec![error(
            ErrorCode::InvalidContract,
            path,
            format!("contract has no definition for type '{name}'"),
        )]);
    };
    let Value::Object(object) = value else {
        return Err(vec![error(
            ErrorCode::InvalidType,
            path,
            format!("expected tagged union '{name}'"),
        )]);
    };
    let Some(tag) = object.get("_type").and_then(Value::as_str) else {
        let mut tag_path = path;
        tag_path.push("_type".to_string());
        return Err(vec![error(
            ErrorCode::MissingField,
            tag_path,
            "tagged union requires a string '_type' discriminator",
        )]);
    };
    let Some(fields) = definition.variants.get(tag) else {
        let mut tag_path = path;
        tag_path.push("_type".to_string());
        return Err(vec![error(
            ErrorCode::UnknownVariant,
            tag_path,
            format!("unknown variant '{tag}' for '{name}'"),
        )]);
    };
    let mut errors = Vec::new();
    for field_name in object.keys() {
        if field_name != "_type" && !fields.contains_key(field_name) {
            let mut field_path = path.clone();
            field_path.push(field_name.clone());
            errors.push(error(
                ErrorCode::UnknownField,
                field_path,
                format!("variant '{tag}' has no field '{field_name}'"),
            ));
        }
    }
    let mut normalized = Map::new();
    normalized.insert("_type".to_string(), Value::String(tag.to_string()));
    for (field_name, schema) in fields {
        let mut field_path = path.clone();
        field_path.push(field_name.clone());
        let raw = match object.get(field_name) {
            Some(value) => value,
            None => {
                errors.push(error(
                    ErrorCode::MissingField,
                    field_path,
                    format!("variant '{tag}' is missing field '{field_name}'"),
                ));
                continue;
            }
        };
        match normalize_value(contract, schema, raw, field_path) {
            Ok(value) => {
                normalized.insert(field_name.clone(), value);
            }
            Err(mut field_errors) => errors.append(&mut field_errors),
        }
    }
    if errors.is_empty() {
        Ok(Value::Object(normalized))
    } else {
        Err(errors)
    }
}

fn type_name(type_: &ValueType) -> String {
    match type_ {
        ValueType::String => "String".to_string(),
        ValueType::Int => "Int".to_string(),
        ValueType::Float => "Float".to_string(),
        ValueType::Bool => "Bool".to_string(),
        ValueType::DateTime => "DateTime".to_string(),
        ValueType::Date => "Date".to_string(),
        ValueType::Json => "Json".to_string(),
        ValueType::TypedJson { .. } => "typed Json".to_string(),
        ValueType::List { .. } => "List".to_string(),
        ValueType::Dict { .. } => "Dict".to_string(),
        ValueType::IdInt => "integer ID".to_string(),
        ValueType::IdUuid => "UUID ID".to_string(),
        ValueType::Custom { name } => name.clone(),
    }
}

fn error(code: ErrorCode, path: Vec<String>, message: impl Into<String>) -> ValidationError {
    ValidationError {
        code,
        path,
        message: message.into(),
    }
}
