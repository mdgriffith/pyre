use pyre::db::introspect::{self, SchemaResult};
use pyre::fixture::{self, ReturnedRow};
use serde::Serialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

struct IngestError(JsValue);

impl From<fixture::Error> for IngestError {
    fn from(error: fixture::Error) -> Self {
        Self(JsValue::from_str(&error.to_string()))
    }
}

/// The callback must execute the insert on the caller's write transaction and
/// resolve to the physical RETURNING row (blob values as byte arrays).
#[wasm_bindgen]
pub async fn ingest_fixture(
    introspection: JsValue,
    entries: JsValue,
    execute: js_sys::Function,
    expected_namespace: Option<String>,
) -> Result<JsValue, JsValue> {
    let raw: introspect::IntrospectionRaw = serde_wasm_bindgen::from_value(introspection)?;
    if raw.schema_source.is_empty() {
        return Err(JsValue::from_str("database does not contain a Pyre schema"));
    }
    let introspection = introspect::from_raw(raw);
    let SchemaResult::Success { schema, context } = &introspection.schema else {
        return Err(JsValue::from_str(
            "database contains an invalid Pyre schema",
        ));
    };
    if let Some(expected) = expected_namespace {
        if expected != schema.namespace {
            return Err(JsValue::from_str(&format!(
                "seed expected database namespace '{expected}', found '{}'",
                schema.namespace
            )));
        }
    }
    let entries: Vec<(String, serde_json::Value)> = serde_wasm_bindgen::from_value(entries)?;
    let response = fixture::ingest_entries(context, &schema.namespace, entries, |insert| {
        let execute = &execute;
        async move {
            let serializer =
                serde_wasm_bindgen::Serializer::new().serialize_large_number_types_as_bigints(true);
            let step = insert
                .serialize(&serializer)
                .map_err(|error| IngestError(error.into()))?;
            let pending = execute
                .call1(&JsValue::UNDEFINED, &step)
                .map_err(IngestError)?;
            let row = JsFuture::from(js_sys::Promise::resolve(&pending))
                .await
                .map_err(IngestError)?;
            serde_wasm_bindgen::from_value::<ReturnedRow>(row)
                .map_err(|error| IngestError(error.into()))
        }
    })
    .await
    .map_err(|error: IngestError| error.0)?;

    // Keep the existing JSON-compatible result shape. Integers used as SQL
    // arguments above travel separately as BigInt, so nested keys stay exact.
    let json =
        serde_json::to_string(&response).map_err(|error| JsValue::from_str(&error.to_string()))?;
    js_sys::JSON::parse(&json)
}
