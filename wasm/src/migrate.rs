use crate::cache;
use pyre::db::introspect;
use pyre::db::migrate;
use pyre::error;

const FILEPATH: &str = "schema.pyre";

/// Re-export MigrationSql from the centralized migrate module
pub use migrate::MigrationSql;

/**
 * Dynamic migration approach - generates SQL from schema source without file operations.
 * This is used in WASM environments where file system access is not available.
 *
 * Delegates to the centralized migrate_dynamic function in pyre::db::migrate.
 */
pub fn migrate(
    name: String,
    introspection: &introspect::Introspection,
    new_schema_source: &str,
) -> Result<MigrationSql, Vec<error::Error>> {
    migrate::migrate_dynamic(name, introspection, new_schema_source, FILEPATH)
}

pub fn migrate_wasm(
    name: String,
    schema_source: String,
) -> Result<MigrationSql, Vec<error::Error>> {
    let introspection = match cache::get() {
        Some(introspection) => introspection,
        None => {
            return Err(vec![error::Error {
                error_type: error::ErrorType::MigrationMissingSchema,
                filepath: "".to_string(),
                locations: vec![],
            }]);
        }
    };

    migrate(name, &introspection, &schema_source)
}

pub fn migrate_with_introspection_wasm(
    name: String,
    schema_source: String,
    introspection: wasm_bindgen::JsValue,
) -> Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue> {
    #[derive(serde::Deserialize)]
    struct Input {
        #[serde(flatten)]
        raw: introspect::IntrospectionRaw,
        #[serde(default)]
        target_schema_namespace: Option<String>,
        #[serde(default)]
        target_external_identities: Vec<pyre::ast::ExternalIdentity>,
    }
    let input = serde_wasm_bindgen::from_value::<Input>(introspection).map_err(|error| {
        wasm_bindgen::JsValue::from_str(&format!("Invalid database introspection: {error}"))
    })?;
    let namespace = input
        .target_schema_namespace
        .as_deref()
        .unwrap_or(pyre::ast::DEFAULT_SCHEMANAME);
    let introspection =
        introspect::from_raw_with_project(input.raw, namespace, &input.target_external_identities);
    let result = migrate::migrate_dynamic_with_external_identities(
        name,
        &introspection,
        &schema_source,
        FILEPATH,
        input
            .target_schema_namespace
            .as_deref()
            .unwrap_or(pyre::ast::DEFAULT_SCHEMANAME),
        &input.target_external_identities,
    );

    serde_wasm_bindgen::to_value(&result).map_err(|error| {
        wasm_bindgen::JsValue::from_str(&format!("Failed to serialize migration plan: {error}"))
    })
}
