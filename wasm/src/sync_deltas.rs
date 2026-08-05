use crate::cache;
use pyre::db::introspect;
use pyre::sync_deltas;
use serde::{Deserialize, Serialize};
use serde_wasm_bindgen;
use std::collections::{HashMap, HashSet};
use wasm_bindgen::prelude::*;

// WASM-compatible types for sync deltas
// Grouped format matching SQL output: one entry per table with multiple rows
#[derive(Serialize, Deserialize, Clone)]
pub struct AffectedRowTableGroupWasm {
    pub table_name: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>, // Array of row arrays, each row array has values matching headers order
}

#[derive(Serialize, Deserialize)]
pub struct SessionDataWasm {
    pub session: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
pub struct SessionDeltaGroupWasm {
    pub session_ids: HashSet<String>,
    pub table_groups: Vec<AffectedRowTableGroupWasm>,
}

#[derive(Serialize, Deserialize)]
pub struct SyncDeltasResultWasm {
    pub groups: Vec<SessionDeltaGroupWasm>,
}

pub fn convert_table_group_wasm_to_rust(
    group: &AffectedRowTableGroupWasm,
) -> sync_deltas::AffectedRowTableGroup {
    sync_deltas::AffectedRowTableGroup {
        table_name: group.table_name.clone(),
        headers: group.headers.clone(),
        rows: group.rows.clone(),
    }
}

pub fn convert_table_group_rust_to_wasm(
    group: sync_deltas::AffectedRowTableGroup,
) -> AffectedRowTableGroupWasm {
    AffectedRowTableGroupWasm {
        table_name: group.table_name,
        headers: group.headers,
        rows: group.rows,
    }
}

fn convert_result_rust_to_wasm(result: sync_deltas::SyncDeltasResult) -> SyncDeltasResultWasm {
    SyncDeltasResultWasm {
        groups: result
            .groups
            .into_iter()
            .map(|group| SessionDeltaGroupWasm {
                session_ids: group.session_ids,
                table_groups: group
                    .table_groups
                    .into_iter()
                    .map(convert_table_group_rust_to_wasm)
                    .collect(),
            })
            .collect(),
    }
}

/// Calculate sync deltas for connected sessions based on affected rows from a mutation
/// Accepts grouped format directly from SQL (no transformation needed)
pub fn calculate_sync_deltas_wasm(
    affected_row_groups: JsValue,
    connected_sessions: JsValue,
) -> Result<SyncDeltasResultWasm, String> {
    let introspection = match cache::get() {
        Some(introspection) => introspection,
        None => return Err("No schema found".to_string()),
    };

    // Parse grouped format directly from JavaScript
    let affected_row_groups_wasm: Vec<AffectedRowTableGroupWasm> =
        serde_wasm_bindgen::from_value(affected_row_groups)
            .map_err(|_e| "Failed to parse affected row groups".to_string())?;

    // Accept Map as HashMap<String, SessionDataWasm> where session_id comes from the map key
    let connected_sessions_map: HashMap<String, SessionDataWasm> =
        serde_wasm_bindgen::from_value(connected_sessions)
            .map_err(|_e| "Failed to parse connected sessions".to_string())?;

    match &introspection.schema {
        introspect::SchemaResult::Success { context, .. } => {
            // Convert WASM types to Rust types (grouped format)
            let affected_row_groups_rust: Vec<sync_deltas::AffectedRowTableGroup> =
                affected_row_groups_wasm
                    .iter()
                    .map(convert_table_group_wasm_to_rust)
                    .collect();

            let connected_sessions_rust = connected_sessions_map
                .into_iter()
                .map(|(session_id, data)| {
                    let session = pyre::session::prepare_session(context, &data.session)
                        .map_err(|error| {
                            format!("Failed to prepare session '{}': {}", session_id, error)
                        })?;
                    Ok::<_, String>((session_id, session))
                })
                .collect::<Result<HashMap<_, _>, _>>()?;

            // Calculate deltas (now accepts grouped format directly)
            let result = sync_deltas::calculate_sync_deltas(
                &affected_row_groups_rust,
                &connected_sessions_rust,
                context,
            )
            .map_err(|e| match e {
                sync_deltas::SyncDeltasError::TableNotFound(table) => {
                    "Table not found: ".to_string() + &table
                }
                sync_deltas::SyncDeltasError::InvalidRowData(msg) => {
                    "Invalid row data: ".to_string() + &msg
                }
            })?;

            Ok(convert_result_rust_to_wasm(result))
        }
        _ => Err("No schema found".to_string()),
    }
}
