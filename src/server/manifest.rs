use crate::sync;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;

impl Manifest {
    /// Load a generated `manifest.json` from disk.
    ///
    /// This is the manifest produced by `pyre generate` and consumed by the
    /// native Rust query runtime.
    #[cfg(feature = "filesystem")]
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, LoadError> {
        let contents = std::fs::read_to_string(path).map_err(LoadError::Io)?;
        serde_json::from_str(&contents).map_err(LoadError::Json)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Manifest {
    pub version: u32,
    pub session_schema: HashMap<String, FieldSchema>,
    pub queries: HashMap<String, QueryManifest>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QueryManifest {
    pub id: String,
    pub operation: String,
    #[serde(default)]
    pub primary_db: String,
    #[serde(default)]
    pub attached_dbs: Vec<String>,
    pub input_schema: HashMap<String, FieldSchema>,
    pub session_args: Vec<String>,
    pub optional_input_args: Vec<String>,
    pub json_input_args: Vec<String>,
    pub sql: Vec<SqlInfo>,
    #[serde(default, rename = "syncSql")]
    pub sync_sql: Option<Vec<SqlInfo>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FieldSchema {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub is_enum: bool,
    #[serde(default)]
    pub enum_variants: Vec<String>,
    pub nullable: bool,
    pub omittable: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SqlInfo {
    pub include: bool,
    pub params: Vec<String>,
    pub sql: String,
}

#[derive(Clone, Debug)]
pub struct PyreSession {
    logical: HashMap<String, sync::SessionValue>,
    sql_args: HashMap<String, JsonValue>,
}

impl PyreSession {
    /// Validate an application session record and build Pyre runtime views.
    ///
    /// The input should have the same logical shape as the `session { ... }`
    /// block in the Pyre schema. The resulting session exposes unprefixed
    /// logical values for sync permission checks and `session_<name>` SQL args
    /// for query execution.
    pub fn new(value: JsonValue, schema: &HashMap<String, FieldSchema>) -> Result<Self, Error> {
        let JsonValue::Object(object) = value else {
            return Err(Error::ExpectedObject);
        };

        let mut logical = HashMap::new();
        let mut sql_args = HashMap::new();

        for (name, field_schema) in schema {
            let Some(value) = object.get(name) else {
                if field_schema.nullable || field_schema.omittable {
                    continue;
                }

                return Err(Error::MissingField(name.clone()));
            };

            if value.is_null() {
                if !field_schema.nullable {
                    return Err(Error::UnexpectedNull(name.clone()));
                }

                logical.insert(name.clone(), sync::SessionValue::Null);
                sql_args.insert(format!("session_{}", name), JsonValue::Null);
                continue;
            }

            validate_value(name, value, field_schema)?;
            let sql_value = normalize_sql_value(value, field_schema);
            logical.insert(
                name.clone(),
                json_to_session_value(&sql_value, field_schema)?,
            );
            sql_args.insert(format!("session_{}", name), sql_value);
        }

        Ok(Self { logical, sql_args })
    }

    pub fn logical(&self) -> &HashMap<String, sync::SessionValue> {
        &self.logical
    }

    pub fn sql_args(&self) -> &HashMap<String, JsonValue> {
        &self.sql_args
    }
}

fn validate_value(name: &str, value: &JsonValue, schema: &FieldSchema) -> Result<(), Error> {
    let valid = if schema.is_enum {
        let tag = match value {
            JsonValue::String(value) => Some(value.as_str()),
            JsonValue::Object(_) => value.get("_type").and_then(JsonValue::as_str),
            _ => None,
        };
        tag.is_some_and(|tag| schema.enum_variants.iter().any(|variant| variant == tag))
    } else {
        match schema.type_.as_str() {
            "String" => value.is_string(),
            "DateTime" => value.is_string() || value.is_number(),
            "Int" | "Float" => value.is_number(),
            "Bool" => {
                value.is_boolean() || value.as_i64().map(|n| n == 0 || n == 1).unwrap_or(false)
            }
            type_ if type_.starts_with("Id.Int") => value.is_number(),
            type_ if type_.starts_with("Id.Uuid") => value.is_string(),
            type_ if type_.starts_with("Json") => true,
            _ => true,
        }
    };

    if valid {
        Ok(())
    } else {
        Err(Error::InvalidFieldType {
            field: name.to_string(),
            expected: schema.type_.clone(),
        })
    }
}

fn json_to_session_value(
    value: &JsonValue,
    schema: &FieldSchema,
) -> Result<sync::SessionValue, Error> {
    match schema.type_.as_str() {
        "String" => value
            .as_str()
            .map(|value| sync::SessionValue::Text(value.to_string()))
            .ok_or_else(|| Error::InvalidFieldType {
                field: String::new(),
                expected: schema.type_.clone(),
            }),
        "DateTime" => {
            match value {
                JsonValue::String(value) => Ok(sync::SessionValue::Text(value.clone())),
                JsonValue::Number(value) => value
                    .as_i64()
                    .map(sync::SessionValue::Integer)
                    .ok_or_else(|| Error::InvalidFieldType {
                        field: String::new(),
                        expected: schema.type_.clone(),
                    }),
                _ => Err(Error::InvalidFieldType {
                    field: String::new(),
                    expected: schema.type_.clone(),
                }),
            }
        }
        "Int" => value
            .as_i64()
            .map(sync::SessionValue::Integer)
            .ok_or_else(|| Error::InvalidFieldType {
                field: String::new(),
                expected: schema.type_.clone(),
            }),
        "Float" => {
            value
                .as_f64()
                .map(sync::SessionValue::Real)
                .ok_or_else(|| Error::InvalidFieldType {
                    field: String::new(),
                    expected: schema.type_.clone(),
                })
        }
        "Bool" => Ok(sync::SessionValue::Integer(
            if value == &JsonValue::Bool(true) || value.as_i64() == Some(1) {
                1
            } else {
                0
            },
        )),
        type_ if type_.starts_with("Id.Int") => value
            .as_i64()
            .map(sync::SessionValue::Integer)
            .ok_or_else(|| Error::InvalidFieldType {
                field: String::new(),
                expected: schema.type_.clone(),
            }),
        type_ if type_.starts_with("Id.Uuid") => value
            .as_str()
            .map(|value| sync::SessionValue::Text(value.to_string()))
            .ok_or_else(|| Error::InvalidFieldType {
                field: String::new(),
                expected: schema.type_.clone(),
            }),
        _ => Ok(match value {
            JsonValue::String(value) => sync::SessionValue::Text(value.clone()),
            JsonValue::Number(value) => value
                .as_i64()
                .map(sync::SessionValue::Integer)
                .or_else(|| value.as_f64().map(sync::SessionValue::Real))
                .unwrap_or(sync::SessionValue::Null),
            JsonValue::Bool(value) => sync::SessionValue::Integer(if *value { 1 } else { 0 }),
            JsonValue::Null => sync::SessionValue::Null,
            JsonValue::Array(_) | JsonValue::Object(_) => {
                sync::SessionValue::Text(value.to_string())
            }
        }),
    }
}

fn normalize_sql_value(value: &JsonValue, schema: &FieldSchema) -> JsonValue {
    if schema.is_enum {
        if let Some(tag) = value.get("_type").and_then(JsonValue::as_str) {
            return JsonValue::String(tag.to_string());
        }
    }

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

#[derive(Debug)]
pub enum Error {
    ExpectedObject,
    InvalidFieldType { field: String, expected: String },
    MissingField(String),
    UnexpectedNull(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::ExpectedObject => write!(f, "session must be a JSON object"),
            Error::InvalidFieldType { field, expected } => {
                write!(f, "session field '{}' must be {}", field, expected)
            }
            Error::MissingField(field) => write!(f, "missing session field '{}'", field),
            Error::UnexpectedNull(field) => write!(f, "session field '{}' cannot be null", field),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(error) => write!(f, "failed to read manifest: {}", error),
            LoadError::Json(error) => write!(f, "failed to parse manifest: {}", error),
        }
    }
}

impl std::error::Error for LoadError {}
