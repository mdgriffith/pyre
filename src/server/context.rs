//! Version 1 database-context wire shapes, not an authentication or runtime API.
//!
//! Deserialization checks envelope shape only. Receiving a context does not prove
//! its provenance, authenticate a caller, or authorize database access. A trusted
//! server must derive authority and session values; caller-supplied values are not
//! authority. Session-schema validation and expiration checks against the current
//! time are separate responsibilities. Public codecs manage no lifecycle or cache;
//! the crate-private runtime is a context-manager prototype for integration with
//! existing application session authority. It owns no routes, live connections,
//! publication queues, or transport delivery. Applications must coordinate their
//! delivery checks with invalidation; the manager cannot revoke queued bytes.
//! TypeScript currently has the context codec only, not lifecycle parity, and
//! client context installation remains future work.
//! Identifiers are preserved verbatim rather than normalized.
//! Public fields allow direct construction; serialization does not validate
//! manually constructed values. Use serde deserialization to validate wire input.

use serde::{de::Error, Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

#[cfg(feature = "database")]
pub(crate) mod runtime;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextRequest {
    #[serde(deserialize_with = "version")]
    pub protocol_version: u8,
    #[serde(deserialize_with = "identifier")]
    pub database_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ContextMessage {
    Context {
        #[serde(deserialize_with = "version")]
        protocol_version: u8,
        #[serde(deserialize_with = "identifier")]
        database_id: String,
        #[serde(deserialize_with = "context_id")]
        context_id: String,
        #[serde(deserialize_with = "identifier")]
        schema_id: String,
        #[serde(deserialize_with = "identifier")]
        cache_scope: String,
        #[serde(deserialize_with = "identifier")]
        authority_revision: String,
        #[serde(deserialize_with = "identifier")]
        database_epoch: String,
        #[serde(deserialize_with = "session")]
        session: Map<String, Value>,
        #[serde(deserialize_with = "timestamp")]
        expires_at: u64,
    },
    ContextInvalidated {
        #[serde(deserialize_with = "version")]
        protocol_version: u8,
        #[serde(deserialize_with = "identifier")]
        database_id: String,
        #[serde(deserialize_with = "context_id")]
        context_id: String,
        reason: InvalidationReason,
    },
    ContextError {
        #[serde(deserialize_with = "version")]
        protocol_version: u8,
        #[serde(deserialize_with = "identifier")]
        database_id: String,
        code: ContextErrorCode,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvalidationReason {
    AuthorityChanged,
    Expired,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextErrorCode {
    Unauthenticated,
    Denied,
    Unavailable,
    ContextMismatch,
}

fn version<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    if f64::deserialize(deserializer)? == 1.0 {
        Ok(1)
    } else {
        Err(D::Error::custom("protocolVersion must be 1"))
    }
}

fn identifier<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value
        .trim_matches(|character: char| character.is_whitespace() || character == '\u{feff}')
        .is_empty()
    {
        Err(D::Error::custom("identifier must be a nonblank string"))
    } else {
        Ok(value)
    }
}

fn timestamp<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    // JSON numbers such as 1, 1.0, and 1e0 have the same meaning in JavaScript.
    let value = f64::deserialize(deserializer)?;
    if (0.0..=9_007_199_254_740_991.0).contains(&value) && value.fract() == 0.0 {
        Ok(value as u64)
    } else {
        Err(D::Error::custom(
            "expiresAt must be a safe nonnegative integer",
        ))
    }
}

fn context_id<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        Ok(value)
    } else {
        Err(D::Error::custom(
            "contextId must be a header-safe identifier",
        ))
    }
}

fn session<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Map<String, Value>, D::Error> {
    fn interoperable(value: &Value) -> bool {
        match value {
            Value::Number(number) => number.as_f64().is_some_and(|number| {
                number.is_finite()
                    && (number.fract() != 0.0 || number.abs() <= 9_007_199_254_740_991.0)
            }),
            Value::Array(values) => values.iter().all(interoperable),
            Value::Object(values) => values.values().all(interoperable),
            _ => true,
        }
    }
    let value = Map::<String, Value>::deserialize(deserializer)?;
    if value.values().all(interoperable) {
        Ok(value)
    } else {
        Err(D::Error::custom(
            "session numbers must be finite and integers must be JavaScript-safe",
        ))
    }
}
