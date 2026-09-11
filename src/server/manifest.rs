use crate::sync;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;

impl Manifest {
    /// Authenticate the context used for permission SQL against this compiled manifest.
    pub fn matches_context(&self, context: &crate::typecheck::Context) -> bool {
        self.version == 1
            && self.compiled_contract
                == crate::generate::manifest::compiled_schema_contract(context)
    }

    /// Content identity of this compiled allowlist, independent of HashMap insertion order.
    /// `version` is the manifest format version; this fingerprint is the request fence.
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        fn hash_value(value: &JsonValue, hasher: &mut Sha256) {
            match value {
                JsonValue::Object(object) => {
                    hasher.update(b"{");
                    for (index, (key, value)) in object
                        .iter()
                        .collect::<std::collections::BTreeMap<_, _>>()
                        .into_iter()
                        .enumerate()
                    {
                        if index != 0 {
                            hasher.update(b",");
                        }
                        hasher.update(serde_json::to_vec(key).expect("JSON key"));
                        hasher.update(b":");
                        hash_value(value, hasher);
                    }
                    hasher.update(b"}");
                }
                JsonValue::Array(values) => {
                    hasher.update(b"[");
                    for (index, value) in values.iter().enumerate() {
                        if index != 0 {
                            hasher.update(b",");
                        }
                        hash_value(value, hasher);
                    }
                    hasher.update(b"]");
                }
                _ => hasher.update(serde_json::to_vec(value).expect("JSON value")),
            }
        }
        let mut hasher = Sha256::new();
        hash_value(
            &serde_json::to_value(self).expect("manifest is JSON serializable"),
            &mut hasher,
        );
        format!("sha256:{:x}", hasher.finalize())
    }
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
    #[serde(default, rename = "replacementContracts")]
    pub replacement_contracts: HashMap<String, String>,
    /// Compiler-owned schema/session contract, including permissions outside the query projections.
    #[serde(default, rename = "compiledContract")]
    pub compiled_contract: String,
    pub version: u32,
    pub session_schema: HashMap<String, FieldSchema>,
    pub queries: HashMap<String, QueryManifest>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QueryManifest {
    /// Compiler digest of schema permissions/defaults and input/result/session codecs.
    #[serde(default, rename = "compiledContract")]
    pub compiled_contract: String,
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
    #[serde(
        default,
        rename = "generatedEdit",
        skip_serializing_if = "Option::is_none"
    )]
    pub generated_edit: Option<GeneratedEdit>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedEdit {
    pub kind: String,
    /// Zero-based indices in both `sql` and `syncSql`. Each is a direct target DML
    /// statement returning the raw authorized identity as `_pyreEditId`.
    pub write_statement_indices: Vec<usize>,
    /// Writable input names; updates exclude identity and managed/immutable fields.
    pub writable_inputs: Vec<String>,
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
    value: JsonValue,
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
        let JsonValue::Object(ref object) = value else {
            return Err(Error::ExpectedObject);
        };

        let mut logical = HashMap::new();
        let mut sql_args = HashMap::new();

        for (name, field_schema) in schema {
            let value = object.get(name).unwrap_or(&JsonValue::Null);
            if value.is_null()
                && !field_schema.nullable
                && !field_schema.omittable
                && (!object.contains_key(name)
                    || validate_field_inner(name, value, field_schema, true).is_err())
            {
                return Err(if object.contains_key(name) {
                    Error::UnexpectedNull(name.clone())
                } else {
                    Error::MissingField(name.clone())
                });
            }
            if object.contains_key(name) {
                validate_field_inner(name, value, field_schema, true)?;
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

        Ok(Self {
            value,
            logical,
            sql_args,
        })
    }

    pub fn revalidate(&self, schema: &HashMap<String, FieldSchema>) -> Result<Self, Error> {
        Self::new(self.value.clone(), schema)
    }

    pub fn logical(&self) -> &HashMap<String, sync::SessionValue> {
        &self.logical
    }

    pub fn sql_args(&self) -> &HashMap<String, JsonValue> {
        &self.sql_args
    }
}

/// Validate structured values before either SQL serialization or session flattening.
pub(crate) fn validate_field(
    name: &str,
    value: &JsonValue,
    schema: &FieldSchema,
) -> Result<(), Error> {
    validate_field_inner(name, value, schema, false)
}

fn validate_field_inner(
    name: &str,
    value: &JsonValue,
    schema: &FieldSchema,
    session: bool,
) -> Result<(), Error> {
    validate_field_mode(name, value, schema, session, false)
}

/// Validate a reshaped SQLite field, allowing only SQLite's 0/1 Boolean storage
/// representation in flattened columns (not inside typed JSON documents).
pub(crate) fn validate_storage_field(
    name: &str,
    value: &JsonValue,
    schema: &FieldSchema,
) -> Result<(), Error> {
    validate_field_mode(name, value, schema, false, true)
}

fn validate_field_mode(
    name: &str,
    value: &JsonValue,
    schema: &FieldSchema,
    session: bool,
    storage: bool,
) -> Result<(), Error> {
    fn check(
        value: &JsonValue,
        type_: &crate::ast::ColumnType,
        definitions: &HashMap<String, HashMap<String, HashMap<String, FieldSchema>>>,
        depth: usize,
        session: bool,
        storage: bool,
    ) -> bool {
        use crate::ast::ColumnType as T;
        if depth > 64 {
            return false;
        }
        match type_ {
            T::Nullable(inner) => {
                value.is_null() || check(value, inner, definitions, depth + 1, session, storage)
            }
            T::Json => !value.is_null(),
            T::JsonTyped(inner) => check(value, inner, definitions, depth + 1, session, false),
            T::List(inner) => value.as_array().is_some_and(|items| {
                items
                    .iter()
                    .all(|v| check(v, inner, definitions, depth + 1, session, storage))
            }),
            T::Dict(inner) => value.as_object().is_some_and(|items| {
                items
                    .values()
                    .all(|v| check(v, inner, definitions, depth + 1, session, storage))
            }),
            T::String => value.is_string(),
            T::IdUuid { .. } => value.as_str().is_some_and(is_uuid),
            T::Int | T::IdInt { .. } => integer_value(value).is_some(),
            T::Float => value.is_number(),
            T::Bool => {
                value.is_boolean()
                    || ((session || storage) && matches!(integer_value(value), Some(0 | 1)))
            }
            T::DateTime => datetime_to_epoch_seconds(value).is_some(),
            T::Date => value.is_string(),
            T::Custom(name) => {
                let Some(variants) = definitions.get(name) else {
                    return false;
                };
                let tag = value
                    .as_str()
                    .or_else(|| value.get("_type").and_then(JsonValue::as_str));
                let Some(fields) = tag.and_then(|tag| variants.get(tag)) else {
                    return false;
                };
                if value.is_string() {
                    return variants.values().all(HashMap::is_empty);
                }
                let Some(object) = value.as_object() else {
                    return false;
                };
                (session
                    || object
                        .keys()
                        .all(|key| key == "_type" || fields.contains_key(key)))
                    && fields.iter().all(|(name, field)| match object.get(name) {
                        None => field.omittable || (session && field.nullable),
                        Some(v) if v.is_null() => {
                            field.nullable
                                || check(
                                    v,
                                    &T::from_str(&field.type_),
                                    definitions,
                                    depth + 1,
                                    session,
                                    storage,
                                )
                        }
                        Some(v) => check(
                            v,
                            &crate::ast::ColumnType::from_str(&field.type_),
                            definitions,
                            depth + 1,
                            session,
                            storage,
                        ),
                    })
            }
            T::ForeignKey { .. } => false,
        }
    }
    let valid = if value.is_null() {
        schema.nullable
            || check(
                value,
                &crate::ast::ColumnType::from_str(&schema.type_),
                &HashMap::new(),
                0,
                session,
                storage,
            )
    } else if schema.is_enum {
        let tag = value
            .as_str()
            .or_else(|| value.get("_type").and_then(JsonValue::as_str));
        tag.is_some_and(|tag| schema.enum_variants.iter().any(|v| v == tag))
            && (session || value.as_object().is_none_or(|object| object.len() == 1))
    } else {
        let mut definitions = schema.tagged_union_types.clone();
        if !schema.tagged_union_variants.is_empty() {
            definitions.insert(schema.type_.clone(), schema.tagged_union_variants.clone());
        }
        check(
            value,
            &crate::ast::ColumnType::from_str(&schema.type_),
            &definitions,
            0,
            session,
            storage,
        )
    };
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidFieldType {
            field: name.into(),
            expected: schema.type_.clone(),
        })
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
    if let Some(tagged_union_variants) = tagged_union_variants.filter(|_| !schema.is_enum) {
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
            if field_value.is_null()
                && !field_schema.nullable
                && !field_schema.omittable
                && (!object.contains_key(name)
                    || validate_field_inner(name, field_value, field_schema, true).is_err())
            {
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
    let sql_value = if schema.type_.starts_with("Json") {
        JsonValue::String(normalize_json_value_inner(value, schema, false).to_string())
    } else {
        normalize_sql_value(value, schema)
    };
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

// Match client identity ingestion without rewriting case, imposing a UUID
// version, or coercing legacy stored keys.
pub(crate) fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
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
            "Int" => integer_value(value).is_some(),
            "Float" => value.is_number(),
            "Bool" => value.is_boolean() || matches!(integer_value(value), Some(0 | 1)),
            type_ if type_.starts_with("Id.Int") => integer_value(value).is_some(),
            type_ if type_.starts_with("Id.Uuid") => value.as_str().is_some_and(is_uuid),
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

/// Normalize typed JSON recursively without treating it as a partial read projection.
pub(crate) fn normalize_json_value(value: &JsonValue, schema: &FieldSchema) -> JsonValue {
    normalize_json_value_inner(value, schema, true)
}

fn normalize_json_value_inner(
    value: &JsonValue,
    schema: &FieldSchema,
    write_input: bool,
) -> JsonValue {
    fn normalize(
        value: &JsonValue,
        type_: &crate::ast::ColumnType,
        definitions: &HashMap<String, HashMap<String, HashMap<String, FieldSchema>>>,
        write_input: bool,
    ) -> JsonValue {
        use crate::ast::ColumnType as T;
        if value.is_null() {
            return JsonValue::Null;
        }
        match type_ {
            T::Nullable(inner) | T::JsonTyped(inner) => {
                normalize(value, inner, definitions, write_input)
            }
            T::Int | T::IdInt { .. } => integer_value(value)
                .map(JsonValue::from)
                .unwrap_or_else(|| value.clone()),
            T::DateTime => datetime_to_epoch_seconds(value)
                .map(JsonValue::from)
                .unwrap_or_else(|| value.clone()),
            T::List(inner) => JsonValue::Array(
                value
                    .as_array()
                    .expect("validated list")
                    .iter()
                    .map(|v| normalize(v, inner, definitions, write_input))
                    .collect(),
            ),
            T::Dict(inner) => JsonValue::Object(
                value
                    .as_object()
                    .expect("validated dict")
                    .iter()
                    .map(|(key, v)| (key.clone(), normalize(v, inner, definitions, write_input)))
                    .collect(),
            ),
            T::Bool if !write_input => {
                JsonValue::Bool(value == &JsonValue::Bool(true) || integer_value(value) == Some(1))
            }
            T::Custom(name) => {
                let Some(variants) = definitions.get(name) else {
                    return value.clone();
                };
                let tag = value
                    .as_str()
                    .or_else(|| value.get("_type").and_then(JsonValue::as_str))
                    .expect("validated tag");
                if variants.values().all(HashMap::is_empty) {
                    // Typed JSON uses the stored tag object in both write and session bindings.
                    // Scalar enum bindings are handled separately by normalize_sql_value.
                    return serde_json::json!({"_type": tag});
                }
                let fields = &variants[tag];
                JsonValue::Object(
                    value
                        .as_object()
                        .expect("validated variant")
                        .iter()
                        .filter(|(key, _)| {
                            write_input || key.as_str() == "_type" || fields.contains_key(*key)
                        })
                        .map(|(key, v)| {
                            (
                                key.clone(),
                                fields
                                    .get(key)
                                    .map(|field| {
                                        normalize(
                                            v,
                                            &T::from_str(&field.type_),
                                            definitions,
                                            write_input,
                                        )
                                    })
                                    .unwrap_or_else(|| v.clone()),
                            )
                        })
                        .collect(),
                )
            }
            _ => value.clone(),
        }
    }
    let mut definitions = schema.tagged_union_types.clone();
    if !schema.tagged_union_variants.is_empty() {
        definitions.insert(schema.type_.clone(), schema.tagged_union_variants.clone());
    }
    if schema.is_enum {
        return value.get("_type").cloned().unwrap_or_else(|| value.clone());
    }
    normalize(
        value,
        &crate::ast::ColumnType::from_str(&schema.type_),
        &definitions,
        write_input,
    )
}

pub(crate) fn normalize_sql_value(value: &JsonValue, schema: &FieldSchema) -> JsonValue {
    if value.is_null() {
        return JsonValue::Null;
    }
    if schema.type_ == "Int" || schema.type_.starts_with("Id.Int") {
        if let Some(integer) = integer_value(value) {
            return JsonValue::from(integer);
        }
    }
    if schema.is_enum {
        if let Some(tag) = value.get("_type").and_then(JsonValue::as_str) {
            return JsonValue::String(tag.to_string());
        }
    }

    if schema.type_ == "Bool" {
        return JsonValue::from(
            if value == &JsonValue::Bool(true) || integer_value(value) == Some(1) {
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

    normalize_json_value(value, schema)
}

fn integer_value(value: &JsonValue) -> Option<i64> {
    let number = value.as_f64()?;
    (number.fract() == 0.0 && number.abs() <= 9_007_199_254_740_991.0).then_some(number as i64)
}

fn datetime_to_epoch_seconds(value: &JsonValue) -> Option<i64> {
    // Match generated CoercedDate's whole seconds and JavaScript Date range.
    const MAX_SECONDS: i64 = 8_640_000_000_000;
    if let Some(seconds) = integer_value(value) {
        return (seconds.abs() <= MAX_SECONDS).then_some(seconds);
    }

    let raw = value.as_str()?.trim();
    if let Ok(seconds) = raw.parse::<i64>() {
        return (seconds >= -MAX_SECONDS && seconds <= MAX_SECONDS).then_some(seconds);
    }

    if raw.as_bytes().get(17..19) == Some(b"60") {
        return None;
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
