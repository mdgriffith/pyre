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
    cache::set_schema(introspection);
    Ok(())
}

#[wasm_bindgen]
pub fn process_introspection(introspection: JsValue) -> Result<JsValue, JsValue> {
    cache::process_introspection(introspection)
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
pub fn calculate_sync_deltas(affected_rows: JsValue, connected_sessions: JsValue) -> JsValue {
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
