use pyre::db::{
    bundle::{self, plan, structure, Bundle, History},
    migrate, policy,
};
use serde::Deserialize;
use wasm_bindgen::prelude::*;

fn to_js(value: impl serde::Serialize) -> Result<JsValue, JsValue> {
    let json = serde_json::to_string(&value).map_err(|e| e.to_string())?;
    js_sys::JSON::parse(&json)
}

#[derive(Deserialize)]
struct Artifact {
    manifest: String,
    // Retain exact manifest text and decode the file map in Rust for both catalogs.
    files: String,
}

impl Artifact {
    fn verify(self, namespace: &str) -> Result<Bundle, String> {
        let files: std::collections::BTreeMap<String, String> =
            serde_json::from_str(&self.files).map_err(|e| e.to_string())?;
        Bundle::verify(
            namespace,
            self.manifest.as_bytes(),
            files
                .into_iter()
                .map(|(name, text)| (name, text.into_bytes())),
        )
        .map_err(|e| e.to_string())
    }
}

#[wasm_bindgen]
pub fn finalized_migration_sql() -> Result<JsValue, JsValue> {
    to_js(serde_json::json!({
        "createState": plan::CREATE_STATE,
        "createReceipts": plan::CREATE_RECEIPTS,
        "createBundles": plan::CREATE_BUNDLES,
        "readState": plan::READ_STATE,
        "readBundles": plan::READ_BUNDLES,
        "readReceipt": "select predecessor, cast(applied_at as text) as applied_at, api_version, cast(update_by as text) as update_by from main._pyre_migration_receipts where hash = ?",
        "insertReceipt": plan::INSERT_RECEIPT,
        "tableExists": plan::TABLE_EXISTS,
        "nonempty": plan::NONEMPTY,
        "unsafeConnection": plan::UNSAFE_CONNECTION,
        "unsafeWrites": plan::UNSAFE_WRITES,
        "createMetadata": migrate::CREATE_MIGRATION_TABLE,
        "hasMetadataNamespace": plan::HAS_METADATA_NAMESPACE,
        "addMetadataNamespace": plan::ADD_METADATA_NAMESPACE,
        "readStructure": structure::READ_STRUCTURE,
        "createPolicy": policy::CREATE,
        "preserve": plan::preserved_policy(),
    }))
}

#[wasm_bindgen]
pub fn plan_finalized_history(
    namespace: &str,
    binding: &str,
    artifacts: JsValue,
    initialize: bool,
    baseline: Option<String>,
) -> Result<JsValue, JsValue> {
    let artifacts: Vec<Artifact> =
        serde_wasm_bindgen::from_value(artifacts).map_err(|e| e.to_string())?;
    let bundles = artifacts
        .into_iter()
        .map(|artifact| artifact.verify(namespace))
        .collect::<Result<Vec<_>, _>>()?;
    let history = History::new(namespace, bundles).map_err(|e| e.to_string())?;
    let binding: plan::Binding = binding.parse()?;
    let steps = history
        .bundles()
        .iter()
        .enumerate()
        .map(|(i, bundle)| {
            plan::step(
                bundle,
                i.checked_sub(1)
                    .map(|previous| &history.bundles()[previous]),
                binding,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let store = if initialize && binding == plan::Binding::Database {
        history
            .bundles()
            .iter()
            .enumerate()
            .map(|(i, bundle)| {
                let name = if i == 0 {
                    "root".into()
                } else {
                    bundle.hash().to_string()
                };
                plan::store_bundle(&name, bundle)
            })
            .collect::<Vec<_>>()
    } else {
        vec![]
    };
    let baseline = baseline
        .map(|hash| {
            let hash: bundle::BundleHash = hash.parse()?;
            let i = history
                .bundles()
                .iter()
                .position(|bundle| bundle.hash() == &hash)
                .ok_or("baseline hash is absent from the catalog")?;
            Ok::<_, &str>(plan::install_runtime(
                &history.bundles()[i],
                binding,
                &steps[i].fingerprint,
                true,
                false,
            ))
        })
        .transpose()?;
    to_js(serde_json::json!({ "steps": steps, "store": store, "baseline": baseline }))
}

#[wasm_bindgen]
pub fn verify_finalized_bundle(namespace: &str, artifact: JsValue) -> Result<JsValue, JsValue> {
    let artifact: Artifact = serde_wasm_bindgen::from_value(artifact).map_err(|e| e.to_string())?;
    let bundle = artifact.verify(namespace)?;
    to_js(
        serde_json::json!({ "hash": bundle.hash(), "predecessor": bundle.predecessor(), "apiVersion": bundle.target_api_version() }),
    )
}

#[wasm_bindgen]
pub fn audit_finalized_structure(
    namespace: &str,
    source: &str,
    identities: JsValue,
    rows: JsValue,
) -> Result<(), JsValue> {
    let identities =
        serde_wasm_bindgen::from_value::<Vec<_>>(identities).map_err(|e| e.to_string())?;
    let rows = serde_wasm_bindgen::from_value(rows).map_err(|e| e.to_string())?;
    let context = bundle::load_standalone_schema(namespace, source, &identities)
        .map_err(|e| e.to_string())?;
    Ok(structure::audit(&context, rows)?)
}
