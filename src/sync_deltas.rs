use crate::ast::{self, WhereArg};
use crate::sync::SessionValue;
use crate::typecheck;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

// Sync deltas module requires json feature for JSON value handling
#[cfg(feature = "json")]
use serde_json::{Map, Value as JsonValue};

// When json feature is not enabled, sync deltas functionality is not available
#[cfg(not(feature = "json"))]
compile_error!("sync_deltas module requires the 'json' feature to be enabled");

/// Grouped format from SQL: one entry per table with multiple rows
/// This matches the format returned by SQL generation
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AffectedRowTableGroup {
    pub table_name: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<JsonValue>>, // Array of row arrays, each row array has values matching headers order
}

/// A group of sessions that share the same affected table groups
#[derive(Serialize, Deserialize)]
pub struct SessionDeltaGroup {
    pub session_ids: HashSet<String>,
    /// Table groups with their rows (keeps grouped format for efficiency)
    pub table_groups: Vec<AffectedRowTableGroup>,
}

/// Result containing deltas grouped by session permissions
#[derive(Serialize, Deserialize)]
pub struct SyncDeltasResult {
    /// Groups of sessions, each with their filtered table groups
    pub groups: Vec<SessionDeltaGroup>,
}

/// Evaluate a permission WhereArg against row data and session values
/// Returns true if the permission condition is satisfied
///
/// Optimized to work directly with JsonValue::Object references, avoiding HashMap conversion
fn evaluate_permission(
    where_arg: &WhereArg,
    row_data: &Map<String, JsonValue>,
    session: &HashMap<String, SessionValue>,
) -> bool {
    match where_arg {
        WhereArg::Exists(..) => false,
        WhereArg::Column(is_session_var, fieldname, op, value, _field_name_range) => {
            // Get the right-hand side value first (needed for both paths)
            let rhs_value = query_value_to_json(value, session);

            // Get the left-hand side value
            // Optimized: for row columns, use reference directly (no clone!)
            // For session variables, conversion to JsonValue is unavoidable but less frequent
            if *is_session_var {
                // Session variable - convert to JsonValue (unavoidable conversion)
                let lhs_value = session
                    .get(fieldname)
                    .map_or(JsonValue::Null, |v| session_value_to_json(v));
                evaluate_operator(op, &lhs_value, &rhs_value)
            } else {
                // Table column - use reference directly from Map (no clone!)
                // This is the hot path - most permission checks are on row columns
                let lhs_value_ref = row_data.get(fieldname).unwrap_or(&JsonValue::Null);
                evaluate_operator(op, lhs_value_ref, &rhs_value)
            }
        }
        WhereArg::And(args) => {
            // All conditions must be true - short-circuit on first false
            args.iter()
                .all(|arg| evaluate_permission(arg, row_data, session))
        }
        WhereArg::Or(args) => {
            // At least one condition must be true - short-circuit on first true
            args.iter()
                .any(|arg| evaluate_permission(arg, row_data, session))
        }
    }
}

/// Evaluate an operator between two JSON values
fn evaluate_operator(op: &ast::Operator, lhs: &JsonValue, rhs: &JsonValue) -> bool {
    match op {
        ast::Operator::Equal => json_values_equal(lhs, rhs),
        ast::Operator::NotEqual => !json_values_equal(lhs, rhs),
        ast::Operator::GreaterThan => json_compare(lhs, rhs) == Some(std::cmp::Ordering::Greater),
        ast::Operator::LessThan => json_compare(lhs, rhs) == Some(std::cmp::Ordering::Less),
        ast::Operator::GreaterThanOrEqual => {
            matches!(
                json_compare(lhs, rhs),
                Some(std::cmp::Ordering::Greater) | Some(std::cmp::Ordering::Equal)
            )
        }
        ast::Operator::LessThanOrEqual => {
            matches!(
                json_compare(lhs, rhs),
                Some(std::cmp::Ordering::Less) | Some(std::cmp::Ordering::Equal)
            )
        }
        ast::Operator::In => {
            // rhs should be an array, check if lhs is in it
            if let JsonValue::Array(arr) = rhs {
                arr.iter().any(|item| json_values_equal(lhs, item))
            } else {
                false
            }
        }
        ast::Operator::NotIn => {
            // rhs should be an array, check if lhs is NOT in it
            if let JsonValue::Array(arr) = rhs {
                !arr.iter().any(|item| json_values_equal(lhs, item))
            } else {
                true
            }
        }
        ast::Operator::Like => {
            // Simple LIKE pattern matching (SQL LIKE semantics)
            if let (Some(lhs_str), Some(rhs_str)) = (lhs.as_str(), rhs.as_str()) {
                like_pattern_match(lhs_str, rhs_str)
            } else {
                false
            }
        }
        ast::Operator::NotLike => {
            // Simple NOT LIKE pattern matching
            if let (Some(lhs_str), Some(rhs_str)) = (lhs.as_str(), rhs.as_str()) {
                !like_pattern_match(lhs_str, rhs_str)
            } else {
                true
            }
        }
    }
}

/// Compare two JSON values for equality
/// Handles SQLite boolean/integer equivalence: true == 1, false == 0
fn json_values_equal(a: &JsonValue, b: &JsonValue) -> bool {
    match (a, b) {
        (JsonValue::Null, JsonValue::Null) => true,
        (JsonValue::Bool(a), JsonValue::Bool(b)) => a == b,
        (JsonValue::Number(a), JsonValue::Number(b)) => {
            // Try to compare as integers first, then floats
            if let (Some(a_i), Some(b_i)) = (a.as_i64(), b.as_i64()) {
                a_i == b_i
            } else if let (Some(a_f), Some(b_f)) = (a.as_f64(), b.as_f64()) {
                (a_f - b_f).abs() < f64::EPSILON
            } else {
                false
            }
        }
        // Handle SQLite boolean/integer equivalence: true == 1, false == 0
        (JsonValue::Bool(a_bool), JsonValue::Number(b_num)) => {
            if let Some(b_i) = b_num.as_i64() {
                (*a_bool && b_i == 1) || (!*a_bool && b_i == 0)
            } else {
                false
            }
        }
        (JsonValue::Number(a_num), JsonValue::Bool(b_bool)) => {
            if let Some(a_i) = a_num.as_i64() {
                (*b_bool && a_i == 1) || (!*b_bool && a_i == 0)
            } else {
                false
            }
        }
        (JsonValue::String(a), JsonValue::String(b)) => a == b,
        (JsonValue::Array(a), JsonValue::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| json_values_equal(x, y))
        }
        (JsonValue::Object(a), JsonValue::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).map_or(false, |w| json_values_equal(v, w)))
        }
        _ => false,
    }
}

/// Compare two JSON values (returns None if types are incomparable)
fn json_compare(a: &JsonValue, b: &JsonValue) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (JsonValue::Null, JsonValue::Null) => Some(std::cmp::Ordering::Equal),
        (JsonValue::Bool(a), JsonValue::Bool(b)) => Some(a.cmp(b)),
        (JsonValue::Number(a), JsonValue::Number(b)) => {
            // Try to compare as integers first, then floats
            if let (Some(a_i), Some(b_i)) = (a.as_i64(), b.as_i64()) {
                Some(a_i.cmp(&b_i))
            } else if let (Some(a_f), Some(b_f)) = (a.as_f64(), b.as_f64()) {
                a_f.partial_cmp(&b_f)
            } else {
                None
            }
        }
        (JsonValue::String(a), JsonValue::String(b)) => Some(a.cmp(b)),
        _ => None,
    }
}

/// Optimized LIKE pattern matching using byte slices (no allocations)
/// SQL LIKE semantics: % matches any sequence, _ matches any single character
fn like_pattern_match(text: &str, pattern: &str) -> bool {
    like_pattern_match_bytes(text.as_bytes(), pattern.as_bytes(), 0, 0)
}

/// Iterative LIKE pattern matching using byte indices (no allocations)
fn like_pattern_match_bytes(
    text: &[u8],
    pattern: &[u8],
    mut text_idx: usize,
    mut pattern_idx: usize,
) -> bool {
    // Use a stack to handle backtracking for % wildcards
    // Stack stores (text_idx, pattern_idx) pairs
    let mut stack: Vec<(usize, usize)> = Vec::new();

    loop {
        // Check if we've exhausted both text and pattern
        if pattern_idx >= pattern.len() {
            if text_idx >= text.len() {
                return true; // Both exhausted - match
            }
            // Pattern exhausted but text remains - try backtracking
            if let Some((t_idx, p_idx)) = stack.pop() {
                text_idx = t_idx;
                pattern_idx = p_idx;
                continue;
            }
            return false; // No more backtracking options
        }

        if text_idx >= text.len() {
            // Text exhausted
            if pattern[pattern_idx] == b'%' {
                // % can match zero characters
                pattern_idx += 1;
                continue;
            }
            // Try backtracking
            if let Some((t_idx, p_idx)) = stack.pop() {
                text_idx = t_idx;
                pattern_idx = p_idx;
                continue;
            }
            return false;
        }

        match pattern[pattern_idx] {
            b'%' => {
                // % matches zero or more characters
                // Try matching zero characters first (greedy)
                pattern_idx += 1;
                // Also push backtrack point: try matching one+ characters
                stack.push((text_idx + 1, pattern_idx - 1));
            }
            b'_' => {
                // _ matches any single character
                text_idx += 1;
                pattern_idx += 1;
            }
            c => {
                if text[text_idx] == c {
                    // Characters match
                    text_idx += 1;
                    pattern_idx += 1;
                } else {
                    // Characters don't match - try backtracking
                    if let Some((t_idx, p_idx)) = stack.pop() {
                        text_idx = t_idx;
                        pattern_idx = p_idx;
                        continue;
                    }
                    return false; // No match and no backtracking
                }
            }
        }
    }
}

/// Convert a QueryValue to JSON, resolving session variables
fn query_value_to_json(
    value: &ast::QueryValue,
    session: &HashMap<String, SessionValue>,
) -> JsonValue {
    match value {
        ast::QueryValue::String((_, s)) => JsonValue::String(s.clone()),
        ast::QueryValue::Int((_, i)) => JsonValue::Number((*i as i64).into()),
        ast::QueryValue::Float((_, f)) => {
            JsonValue::Number(serde_json::Number::from_f64(*f as f64).unwrap_or(0.into()))
        }
        ast::QueryValue::Bool((_, b)) => JsonValue::Bool(*b),
        ast::QueryValue::Null(_) => JsonValue::Null,
        ast::QueryValue::Variable((_, var)) => {
            // Check if this is a session variable
            if let Some(session_field) = &var.session_field {
                session
                    .get(session_field)
                    .map_or(JsonValue::Null, |v| session_value_to_json(v))
            } else {
                // Regular variable - not supported in permission evaluation
                JsonValue::Null
            }
        }
        ast::QueryValue::LiteralTypeValue((_, details)) => {
            // For literal type values, we'll represent them as strings for now
            // This might need to be more sophisticated depending on use cases
            JsonValue::String(details.name.clone())
        }
        ast::QueryValue::Fn(_) => {
            // Function calls not supported in permission evaluation
            JsonValue::Null
        }
    }
}

/// Convert a SessionValue to JSON
fn session_value_to_json(value: &SessionValue) -> JsonValue {
    match value {
        SessionValue::Null => JsonValue::Null,
        SessionValue::Integer(i) => JsonValue::Number((*i).into()),
        SessionValue::Real(f) => {
            JsonValue::Number(serde_json::Number::from_f64(*f).unwrap_or(0.into()))
        }
        SessionValue::Text(s) => JsonValue::String(s.clone()),
        SessionValue::Blob(b) => {
            // Convert blob to hex string for JSON without using format!
            // This avoids pulling in std::fmt infrastructure
            const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
            let mut hex = String::with_capacity(b.len() * 2);
            for byte in b {
                hex.push(HEX_CHARS[(byte >> 4) as usize] as char);
                hex.push(HEX_CHARS[(byte & 0x0f) as usize] as char);
            }
            JsonValue::String(hex)
        }
    }
}

/// Convert a row array to a JSON object using headers
/// This is done lazily during iteration to avoid pre-processing
fn row_array_to_object(headers: &[String], row_array: &[JsonValue]) -> Map<String, JsonValue> {
    let mut obj = Map::with_capacity(headers.len());
    for (i, header) in headers.iter().enumerate() {
        if i < row_array.len() {
            obj.insert(header.clone(), row_array[i].clone());
        }
    }
    obj
}

/// Calculate which sessions should receive which affected rows based on permissions
/// Keeps grouped format for efficiency (no explosion into individual rows)
/// Optimized with:
/// - Table lookup map (O(1) instead of O(n) per lookup)
/// - Lazy conversion of row arrays to objects (only when needed for permission checking)
/// - Groups sessions by shared affected row sets (deduplication)
/// - Returns grouped format to minimize bandwidth and memory
pub fn calculate_sync_deltas(
    affected_row_groups: &[AffectedRowTableGroup],
    connected_sessions: &HashMap<String, HashMap<String, SessionValue>>,
    context: &typecheck::Context,
) -> Result<SyncDeltasResult, SyncDeltasError> {
    // OPTIMIZATION 1: Build table lookup map once (O(k) instead of O(n*m*k))
    let mut table_map: HashMap<String, Option<WhereArg>> = HashMap::new();
    for table in context.tables.values() {
        let actual_table_name = ast::get_tablename(&table.record.name, &table.record.fields);
        let permission = ast::get_permissions(&table.record, &ast::QueryOperation::Query);
        table_map.insert(actual_table_name, permission);
    }

    // Build a flat index for permission checking while preserving group structure
    // Structure: Vec<(table_group_idx, row_idx_in_group, row_obj)>
    let mut flat_rows: Vec<(usize, usize, Map<String, JsonValue>)> = Vec::new();

    for (group_idx, table_group) in affected_row_groups.iter().enumerate() {
        let table_name = &table_group.table_name;
        let headers = &table_group.headers;

        // Verify table exists
        table_map
            .get(table_name)
            .ok_or_else(|| SyncDeltasError::TableNotFound(table_name.clone()))?;

        // Convert each row array to an object for permission checking
        for (row_idx, row_array) in table_group.rows.iter().enumerate() {
            let row_obj = row_array_to_object(headers, row_array);
            flat_rows.push((group_idx, row_idx, row_obj));
        }
    }

    // OPTIMIZATION 2: Group sessions by which rows they can see
    // Key: Vec<(group_idx, row_idx)> sorted, Value: session IDs
    let mut row_visibility_to_sessions: HashMap<Vec<(usize, usize)>, HashSet<String>> =
        HashMap::new();

    for (session_id, session_data) in connected_sessions {
        let mut visible_rows = Vec::new();

        for (_flat_idx, (group_idx, row_idx, row_obj)) in flat_rows.iter().enumerate() {
            let table_name = &affected_row_groups[*group_idx].table_name;

            let permission = table_map
                .get(table_name)
                .ok_or_else(|| SyncDeltasError::TableNotFound(table_name.clone()))?
                .as_ref();

            // If no permission (public), all sessions can see it
            let should_receive = if let Some(perm) = permission {
                evaluate_permission(perm, row_obj, session_data)
            } else {
                true // Public - all sessions can see it
            };

            if should_receive {
                visible_rows.push((*group_idx, *row_idx));
            }
        }

        if !visible_rows.is_empty() {
            // Sort to ensure consistent key for grouping
            visible_rows.sort_unstable();
            row_visibility_to_sessions
                .entry(visible_rows)
                .or_insert_with(HashSet::new)
                .insert(session_id.clone());
        }
    }

    // OPTIMIZATION 3: Convert back to grouped format for each session group
    // This keeps the bandwidth-efficient grouped structure
    let groups: Vec<SessionDeltaGroup> = row_visibility_to_sessions
        .into_iter()
        .map(|(visible_rows, session_ids)| {
            // Group visible rows by table
            let mut table_to_row_indices: HashMap<usize, Vec<usize>> = HashMap::new();
            for (group_idx, row_idx) in visible_rows {
                table_to_row_indices
                    .entry(group_idx)
                    .or_insert_with(Vec::new)
                    .push(row_idx);
            }

            // Build filtered table groups
            let mut table_groups = Vec::new();
            for (group_idx, row_indices) in table_to_row_indices {
                let original_group = &affected_row_groups[group_idx];
                let filtered_rows: Vec<Vec<JsonValue>> = row_indices
                    .iter()
                    .map(|&idx| original_group.rows[idx].clone())
                    .collect();

                table_groups.push(AffectedRowTableGroup {
                    table_name: original_group.table_name.clone(),
                    headers: original_group.headers.clone(),
                    rows: filtered_rows,
                });
            }

            SessionDeltaGroup {
                session_ids,
                table_groups,
            }
        })
        .collect();

    Ok(SyncDeltasResult { groups })
}

#[derive(Debug)]
pub enum SyncDeltasError {
    TableNotFound(String),
    InvalidRowData(String),
}

// Display and Error traits removed to avoid formatting infrastructure
// Errors are converted to strings manually in WASM code
