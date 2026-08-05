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
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tagged_union_variants: HashMap<String, HashMap<String, FieldSchema>>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tagged_union_types: HashMap<String, HashMap<String, HashMap<String, FieldSchema>>>,
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
            let value = object.get(name).unwrap_or(&JsonValue::Null);
            if value.is_null() && !field_schema.nullable && !field_schema.omittable {
                return Err(if object.contains_key(name) {
                    Error::UnexpectedNull(name.clone())
                } else {
                    Error::MissingField(name.clone())
                });
            }
            prepare_field(
                name,
                name,
                value,
                field_schema,
                &field_schema.tagged_union_types,
                &mut logical,
                &mut sql_args,
            )?;
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

fn prepare_field(
    display_name: &str,
    physical_name: &str,
    value: &JsonValue,
    schema: &FieldSchema,
    tagged_union_types: &HashMap<String, HashMap<String, HashMap<String, FieldSchema>>>,
    logical: &mut HashMap<String, sync::SessionValue>,
    sql_args: &mut HashMap<String, JsonValue>,
) -> Result<(), Error> {
    if value.is_null() {
        insert_prepared(
            physical_name,
            sync::SessionValue::Null,
            JsonValue::Null,
            logical,
            sql_args,
        );
        fill_tagged_union_descendants_with_null(
            physical_name,
            schema,
            tagged_union_types,
            logical,
            sql_args,
        );
        return Ok(());
    }

    let tagged_union_variants = if schema.tagged_union_variants.is_empty() {
        tagged_union_types.get(&schema.type_)
    } else {
        Some(&schema.tagged_union_variants)
    };
    if let Some(tagged_union_variants) = tagged_union_variants {
        let tag = value
            .get("_type")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| Error::InvalidFieldType {
                field: display_name.to_string(),
                expected: schema.type_.clone(),
            })?;
        let fields = tagged_union_variants
            .get(tag)
            .ok_or_else(|| Error::InvalidFieldType {
                field: display_name.to_string(),
                expected: schema.type_.clone(),
            })?;
        let object = value.as_object().ok_or_else(|| Error::InvalidFieldType {
            field: display_name.to_string(),
            expected: schema.type_.clone(),
        })?;

        insert_prepared(
            physical_name,
            sync::SessionValue::Text(tag.to_string()),
            JsonValue::String(tag.to_string()),
            logical,
            sql_args,
        );
        fill_tagged_union_descendants_with_null(
            physical_name,
            schema,
            tagged_union_types,
            logical,
            sql_args,
        );
        for (name, field_schema) in fields {
            let nested_display_name = format!("{}.{}.{}", display_name, tag, name);
            let nested_physical_name = format!("{}__{}", physical_name, name);
            let field_value = object.get(name).unwrap_or(&JsonValue::Null);
            if field_value.is_null() && !field_schema.nullable && !field_schema.omittable {
                return Err(if object.contains_key(name) {
                    Error::UnexpectedNull(nested_display_name)
                } else {
                    Error::MissingField(nested_display_name)
                });
            }
            prepare_field(
                &nested_display_name,
                &nested_physical_name,
                field_value,
                field_schema,
                tagged_union_types,
                logical,
                sql_args,
            )?;
        }
        return Ok(());
    }

    validate_value(display_name, value, schema)?;
    let sql_value = normalize_sql_value(value, schema);
    let logical_value = json_to_session_value(&sql_value, schema)?;
    insert_prepared(physical_name, logical_value, sql_value, logical, sql_args);
    Ok(())
}

fn insert_prepared(
    physical_name: &str,
    logical_value: sync::SessionValue,
    sql_value: JsonValue,
    logical: &mut HashMap<String, sync::SessionValue>,
    sql_args: &mut HashMap<String, JsonValue>,
) {
    logical.insert(physical_name.to_string(), logical_value);
    sql_args.insert(format!("session_{}", physical_name), sql_value);
}

fn fill_tagged_union_descendants_with_null(
    physical_name: &str,
    schema: &FieldSchema,
    tagged_union_types: &HashMap<String, HashMap<String, HashMap<String, FieldSchema>>>,
    logical: &mut HashMap<String, sync::SessionValue>,
    sql_args: &mut HashMap<String, JsonValue>,
) {
    fill_tagged_union_descendants_with_null_inner(
        physical_name,
        schema,
        tagged_union_types,
        logical,
        sql_args,
        &mut std::collections::HashSet::new(),
    );
}

fn fill_tagged_union_descendants_with_null_inner(
    physical_name: &str,
    schema: &FieldSchema,
    tagged_union_types: &HashMap<String, HashMap<String, HashMap<String, FieldSchema>>>,
    logical: &mut HashMap<String, sync::SessionValue>,
    sql_args: &mut HashMap<String, JsonValue>,
    visiting: &mut std::collections::HashSet<String>,
) {
    let variants = if schema.tagged_union_variants.is_empty() {
        let Some(variants) = tagged_union_types.get(&schema.type_) else {
            return;
        };
        if !visiting.insert(schema.type_.clone()) {
            return;
        }
        variants
    } else {
        &schema.tagged_union_variants
    };
    for fields in variants.values() {
        for (name, field_schema) in fields {
            let nested_name = format!("{}__{}", physical_name, name);
            logical
                .entry(nested_name.clone())
                .or_insert(sync::SessionValue::Null);
            sql_args
                .entry(format!("session_{}", nested_name))
                .or_insert(JsonValue::Null);
            fill_tagged_union_descendants_with_null_inner(
                &nested_name,
                field_schema,
                tagged_union_types,
                logical,
                sql_args,
                visiting,
            );
        }
    }
    if schema.tagged_union_variants.is_empty() {
        visiting.remove(&schema.type_);
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
            "DateTime" => datetime_to_epoch_seconds(value).is_some(),
            "Int" => value.as_i64().is_some(),
            "Float" => value.is_number(),
            "Bool" => {
                value.is_boolean() || value.as_i64().map(|n| n == 0 || n == 1).unwrap_or(false)
            }
            type_ if type_.starts_with("Id.Int") => value.as_i64().is_some(),
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

    if schema.type_ == "DateTime" {
        if let Some(seconds) = datetime_to_epoch_seconds(value) {
            return JsonValue::from(seconds);
        }
    }

    value.clone()
}

fn datetime_to_epoch_seconds(value: &JsonValue) -> Option<i64> {
    if let Some(seconds) = value.as_i64() {
        return Some(seconds);
    }

    let raw = value.as_str()?.trim();
    if let Ok(seconds) = raw.parse::<i64>() {
        return Some(seconds);
    }

    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|datetime| datetime.timestamp())
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
