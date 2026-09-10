use console_log;
use js_sys;
use log::Level;
use wasm_bindgen::prelude::*;
mod cache;
mod migrate;
mod query;
mod seed;
mod sync;
mod sync_deltas;
mod sync_shape;

#[wasm_bindgen(start)]
pub fn start() {
    console_log::init_with_level(Level::Info).expect("error initializing log");
}

#[wasm_bindgen]
pub fn set_schema(introspection: JsValue) -> Result<(), JsValue> {
    // cache::set_schema otherwise silently retains the previous schema on bad input.
    serde_wasm_bindgen::from_value::<pyre::db::introspect::IntrospectionRaw>(introspection.clone())
        .map_err(|_| JsValue::from_str("InvalidSchema"))?;
    cache::set_schema(introspection);
    Ok(())
}

#[wasm_bindgen]
pub fn process_introspection(introspection: JsValue) -> Result<JsValue, JsValue> {
    cache::process_introspection(introspection)
}

/// Compare this with the trusted manifest.compiledContract after restoring the
/// request's captured introspection, without an await before SQL/codec use.
#[wasm_bindgen]
pub fn get_schema_compiled_contract() -> Result<String, JsValue> {
    let introspection = cache::get().ok_or_else(|| JsValue::from_str("InvalidSchema"))?;
    match &introspection.schema {
        pyre::db::introspect::SchemaResult::Success { context, .. } => {
            Ok(pyre::generate::manifest::compiled_schema_contract(context))
        }
        _ => Err(JsValue::from_str("InvalidSchema")),
    }
}

/// Validate raw storage groups before ordinary reshaping. This validates rows,
/// not snapshot coverage: the caller still checks the plan, revision and fence.
#[wasm_bindgen]
pub fn validate_replacement_table_groups(table_groups: JsValue) -> bool {
    let Ok(groups) = serde_wasm_bindgen::from_value::<Vec<pyre::sync_deltas::AffectedRowTableGroup>>(
        table_groups,
    ) else {
        return false;
    };
    let Some(introspection) = cache::get() else {
        return false;
    };
    let pyre::db::introspect::SchemaResult::Success { context, .. } = &introspection.schema else {
        return false;
    };
    let mut names = std::collections::HashSet::new();
    groups.iter().all(|group| {
        if !names.insert(&group.table_name) {
            return false;
        }
        let Some(table) = context.tables.values().find(|table| {
            pyre::ast::get_tablename(&table.record.name, &table.record.fields) == group.table_name
        }) else {
            return false;
        };
        pyre::sync::reshape_replacement_table(context, &table.schema, group).is_ok()
    })
}

#[wasm_bindgen]
pub fn migrate(name: String, schema_source: String) -> JsValue {
    let result = migrate::migrate_wasm(name, schema_source);
    serde_wasm_bindgen::to_value(&result).unwrap()
}

#[wasm_bindgen]
pub fn migrate_with_introspection(
    name: String,
    schema_source: String,
    introspection: JsValue,
) -> Result<JsValue, JsValue> {
    migrate::migrate_with_introspection_wasm(name, schema_source, introspection)
}

#[wasm_bindgen]
pub fn query_to_sql(query_source: String) -> JsValue {
    let result = query::query_to_sql_wasm(query_source);
    serde_wasm_bindgen::to_value(&result).unwrap()
}

#[wasm_bindgen]
pub fn sql_is_initialized() -> String {
    pyre::db::introspect::IS_INITIALIZED.to_string()
}

#[wasm_bindgen]
pub fn sql_introspect() -> String {
    pyre::db::introspect::INTROSPECT_SQL.to_string()
}

#[wasm_bindgen]
pub fn sql_introspect_uninitialized() -> String {
    pyre::db::introspect::INTROSPECT_UNINITIALIZED_SQL.to_string()
}

#[wasm_bindgen]
pub fn calculate_permission_hash(table_name: String, session: JsValue) -> JsValue {
    let result = sync::calculate_permission_hash_wasm(table_name, session);
    match result {
        Ok(hash) => serde_wasm_bindgen::to_value(&hash).unwrap(),
        Err(e) => {
            // e is already a String, no need for .to_string()
            serde_wasm_bindgen::to_value(&("Error: ".to_string() + &e)).unwrap()
        }
    }
}

#[wasm_bindgen]
pub fn get_sync_page_info(sync_cursor: JsValue, session: JsValue, page_size: usize) -> JsValue {
    let result = sync::get_sync_page_info_wasm(sync_cursor, session, page_size);
    match result {
        Ok(info) => {
            // Serialize to JSON string first, then parse it back to JsValue
            // This works around serde_wasm_bindgen HashMap serialization issues
            let json_str = serde_json::to_string(&info).unwrap();
            js_sys::JSON::parse(&json_str).unwrap()
        }
        Err(e) => {
            // e is already a String, no need for .to_string()
            serde_wasm_bindgen::to_value(&("Error: ".to_string() + &e)).unwrap()
        }
    }
}

#[wasm_bindgen]
pub fn get_sync_status_sql(sync_cursor: JsValue, session: JsValue) -> JsValue {
    let result = sync::get_sync_status_sql_wasm(sync_cursor, session);
    match result {
        Ok(statement) => {
            let json_str = serde_json::to_string(&statement).unwrap();
            js_sys::JSON::parse(&json_str).unwrap()
        }
        Err(e) => {
            // e is already a String, no need for .to_string()
            serde_wasm_bindgen::to_value(&("Error: ".to_string() + &e)).unwrap()
        }
    }
}

#[wasm_bindgen]
pub fn get_sync_sql(
    status_rows: JsValue,
    sync_cursor: JsValue,
    session: JsValue,
    page_size: usize,
) -> JsValue {
    let result = sync::get_sync_sql_wasm(status_rows, sync_cursor, session, page_size);
    match result {
        Ok(sql_result) => {
            // Serialize to JSON string first, then parse it back to JsValue
            // This works around serde_wasm_bindgen HashMap serialization issues
            let json_str = serde_json::to_string(&sql_result).unwrap();
            js_sys::JSON::parse(&json_str).unwrap()
        }
        Err(e) => {
            // e is already a String, no need for .to_string()
            serde_wasm_bindgen::to_value(&("Error: ".to_string() + &e)).unwrap()
        }
    }
}

#[wasm_bindgen]
pub fn get_replacement_sql(session: JsValue, namespace: String) -> JsValue {
    match sync::get_replacement_sql_wasm(session, namespace) {
        Ok(sql_result) => {
            let json_str = serde_json::to_string(&sql_result).unwrap();
            js_sys::JSON::parse(&json_str).unwrap()
        }
        Err(e) => serde_wasm_bindgen::to_value(&("Error: ".to_string() + &e)).unwrap(),
    }
}

#[wasm_bindgen]
pub fn calculate_sync_deltas(affected_rows: JsValue, connected_sessions: JsValue) -> JsValue {
    if let Some(introspection) = cache::get() {
        if let pyre::db::introspect::SchemaResult::Success { context, .. } = &introspection.schema {
            if pyre::sync::requires_replacement(context) {
                return JsValue::from_str("Error: ReplacementRequired");
            }
        }
    }
    let result = sync_deltas::calculate_sync_deltas_wasm(affected_rows, connected_sessions);
    match result {
        Ok(deltas_result) => {
            // Serialize to JSON string first, then parse it back to JsValue
            let json_str = serde_json::to_string(&deltas_result).unwrap();
            js_sys::JSON::parse(&json_str).unwrap()
        }
        Err(e) => {
            // e is already a String, no need for .to_string()
            serde_wasm_bindgen::to_value(&("Error: ".to_string() + &e)).unwrap()
        }
    }
}

#[wasm_bindgen]
pub fn reshape_sync_table_groups(table_groups: JsValue) -> JsValue {
    let result = sync_shape::reshape_sync_table_groups_wasm(table_groups);
    match result {
        Ok(table_groups) => {
            let json_str = serde_json::to_string(&table_groups).unwrap();
            js_sys::JSON::parse(&json_str).unwrap()
        }
        Err(e) => serde_wasm_bindgen::to_value(&("Error: ".to_string() + &e)).unwrap(),
    }
}

#[wasm_bindgen]
pub fn seed_database(schema_source: String, options: JsValue) -> JsValue {
    let options: Option<seed::SeedOptions> = if options.is_undefined() || options.is_null() {
        None
    } else {
        match serde_wasm_bindgen::from_value(options) {
            Ok(opts) => Some(opts),
            Err(_e) => {
                // Avoid calling .to_string() on JsValue error
                return serde_wasm_bindgen::to_value(&"Error: Failed to parse options".to_string())
                    .unwrap();
            }
        }
    };

    let result = seed::seed_wasm(schema_source, options);
    match result {
        Ok(seed_sql) => {
            // Serialize to JSON string first, then parse it back to JsValue
            let json_str = serde_json::to_string(&seed_sql).unwrap();
            js_sys::JSON::parse(&json_str).unwrap()
        }
        Err(e) => {
            // e is already a String, no need for .to_string()
            serde_wasm_bindgen::to_value(&("Error: ".to_string() + &e)).unwrap()
        }
    }
}
