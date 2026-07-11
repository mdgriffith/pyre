use crate::ast::{self, WhereArg};
use crate::typecheck;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

// Sync module requires json feature for JSON value handling
#[cfg(feature = "json")]
use serde_json::Value as JsonValue;

// When json feature is not enabled, sync functionality is not available
#[cfg(not(feature = "json"))]
compile_error!("sync module requires the 'json' feature to be enabled");

/// Generic session value type that doesn't depend on libsql
#[derive(Clone, Debug, PartialEq)]
pub enum SessionValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

/// A sync cursor tracks the last seen state for each table
pub type SyncCursor = HashMap<String, TableCursor>;

pub const DEFAULT_SYNC_PAGE_SIZE: usize = 1000;
pub const MAX_SYNC_PAGE_SIZE: usize = 5000;
pub const MAX_SYNC_CURSOR_TABLES: usize = 512;
pub const MAX_SYNC_CURSOR_PERMISSION_HASH_BYTES: usize = 256;
pub const SYNC_ROWS_JSON_COLUMN: &str = "_pyre_rows";

pub fn normalize_page_size(page_size: usize) -> Result<usize, SyncError> {
    if page_size == 0 {
        return Err(SyncError::InvalidPageSize);
    }

    Ok(page_size.min(MAX_SYNC_PAGE_SIZE))
}

/// Cursor state for a single table
#[derive(Clone, Serialize, Deserialize)]
pub struct TableCursor {
    pub last_seen_updated_at: Option<i64>, // Unix timestamp
    pub permission_hash: String,
}

/// Result of a sync page request
#[derive(Serialize, Deserialize)]
pub struct SyncPageResult {
    /// The server-selected source database for this sync page.
    #[serde(rename = "databaseId", skip_serializing_if = "Option::is_none")]
    pub database_id: Option<String>,
    /// The current server-side sync revision for this page.
    #[serde(rename = "serverRevision", skip_serializing_if = "Option::is_none")]
    pub server_revision: Option<i64>,
    /// Data organized by table name
    pub tables: HashMap<String, TableSyncData>,
    /// Whether there is more data to fetch
    pub has_more: bool,
}

/// Data for a single table in a sync page
#[derive(Serialize, Deserialize)]
pub struct TableSyncData {
    /// The rows of data
    pub rows: Vec<JsonValue>,
    /// The permission hash for this table (client should update cursor with this)
    pub permission_hash: String,
    /// The maximum updated_at timestamp from the returned rows (client should update cursor with this)
    pub last_seen_updated_at: Option<i64>,
}

/// SQL statements for syncing a table
#[derive(Clone, Debug)]
pub struct SyncStatement {
    pub sql: String,
    pub params: Vec<SessionValue>,
}

#[derive(Clone, Debug)]
pub struct TableSyncSql {
    pub table_name: String,
    pub permission_hash: String,
    pub sql: Vec<String>,
    pub params: Vec<Vec<SessionValue>>,
    /// Column names in the order they appear in the SQL SELECT
    pub headers: Vec<String>,
    /// Column names that should be decoded as JSON values in the runtime
    pub json_columns: Vec<String>,
}

fn push_storage_column(column_names: &mut Vec<String>, column_name: String) {
    if !column_names.contains(&column_name) {
        column_names.push(column_name);
    }
}

fn table_sync_enabled(context: &typecheck::Context, table: &typecheck::Table) -> bool {
    context
        .namespace_sync_modes
        .get(&table.schema)
        .copied()
        .unwrap_or(ast::SyncMode::Synced)
        == ast::SyncMode::Synced
}

fn synced_table_names(context: &typecheck::Context) -> HashSet<String> {
    context
        .tables
        .values()
        .filter(|table| table_sync_enabled(context, table))
        .map(|table| ast::get_tablename(&table.record.name, &table.record.fields))
        .collect()
}

pub fn validate_sync_cursor(
    sync_cursor: &SyncCursor,
    context: &typecheck::Context,
) -> Result<(), SyncError> {
    if sync_cursor.len() > MAX_SYNC_CURSOR_TABLES {
        return Err(SyncError::InvalidSyncCursor(format!(
            "sync cursor references {} tables; max is {}",
            sync_cursor.len(),
            MAX_SYNC_CURSOR_TABLES
        )));
    }

    let known_tables = synced_table_names(context);
    for (table_name, cursor) in sync_cursor {
        if !known_tables.contains(table_name) {
            return Err(SyncError::InvalidSyncCursor(format!(
                "sync cursor references unknown table '{}'",
                table_name
            )));
        }

        if cursor.permission_hash.len() > MAX_SYNC_CURSOR_PERMISSION_HASH_BYTES {
            return Err(SyncError::InvalidSyncCursor(format!(
                "sync cursor permission_hash for '{}' is too large",
                table_name
            )));
        }
    }

    Ok(())
}

fn collect_sync_storage_columns(
    context: &typecheck::Context,
    column_type: &ast::ColumnType,
    base_name: &str,
    headers: &mut Vec<String>,
    json_columns: &mut Vec<String>,
) {
    push_storage_column(headers, base_name.to_string());

    if column_type.is_json_like() {
        push_storage_column(json_columns, base_name.to_string());
    }

    let Some(type_name) = column_type.get_custom_type_name() else {
        return;
    };

    let Some((_definfo, type_)) = context.types.get(type_name) else {
        return;
    };

    let typecheck::Type::OneOf { variants } = type_ else {
        return;
    };

    for variant in variants {
        if let Some(fields) = &variant.fields {
            for field in fields {
                if let ast::Field::Column(column) = field {
                    collect_sync_storage_columns(
                        context,
                        &column.type_,
                        &format!("{}__{}", base_name, column.name),
                        headers,
                        json_columns,
                    );
                }
            }
        }
    }
}

/// Result containing SQL for all tables that need syncing
#[derive(Debug)]
pub struct SyncSqlResult {
    pub tables: Vec<TableSyncSql>,
}

/// Status information for a single table's sync state
#[derive(Clone, Serialize, Deserialize)]
pub struct TableSyncStatus {
    pub table_name: String,
    pub sync_layer: usize,
    pub needs_sync: bool,
    pub max_updated_at: Option<i64>,
    pub permission_hash: String,
}

/// Result of sync status check
pub struct SyncStatusResult {
    pub server_revision: Option<i64>,
    pub tables: Vec<TableSyncStatus>,
}

/// Extract all session field names referenced in a permission WhereArg
pub fn extract_session_fields_from_permission(where_arg: &WhereArg) -> Vec<String> {
    let mut fields = Vec::new();
    extract_session_fields_recursive(where_arg, &mut fields);
    fields
}

fn extract_session_fields_recursive(where_arg: &WhereArg, fields: &mut Vec<String>) {
    match where_arg {
        WhereArg::Column(is_session_var, fieldname, _, value, _field_name_range) => {
            if *is_session_var {
                fields.push(fieldname.clone());
            }
            extract_session_fields_from_query_value(value, fields);
        }
        WhereArg::And(args) | WhereArg::Or(args) => {
            for arg in args {
                extract_session_fields_recursive(arg, fields);
            }
        }
    }
}

fn extract_session_fields_from_query_value(value: &ast::QueryValue, fields: &mut Vec<String>) {
    match value {
        ast::QueryValue::Variable((_, var)) => {
            if let Some(session_field) = &var.session_field {
                fields.push(session_field.clone());
            }
        }
        ast::QueryValue::Fn(func) => {
            for arg in &func.args {
                extract_session_fields_from_query_value(arg, fields);
            }
        }
        ast::QueryValue::LiteralTypeValue((_, details)) => {
            if let Some(fields_) = &details.fields {
                for (_name, value) in fields_ {
                    extract_session_fields_from_query_value(value, fields);
                }
            }
        }
        ast::QueryValue::String(_)
        | ast::QueryValue::Int(_)
        | ast::QueryValue::Float(_)
        | ast::QueryValue::Bool(_)
        | ast::QueryValue::Null(_) => {}
    }
}

/// Calculate permission hash from permission AST and session values
pub fn calculate_permission_hash(
    permission: &Option<WhereArg>,
    session: &HashMap<String, SessionValue>,
) -> String {
    let mut hasher = Sha256::new();

    // Hash the permission AST structure
    if let Some(perm) = permission {
        hash_permission_ast(&mut hasher, perm);
    } else {
        hasher.update("no_permission");
    }

    // Hash relevant session values
    if let Some(perm) = permission {
        let session_fields = extract_session_fields_from_permission(perm);
        for field in session_fields {
            if let Some(value) = session.get(&field) {
                hasher.update(&field);
                hash_session_value(&mut hasher, value);
            }
        }
    }

    // Convert hash to hex without using format!
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
    let hash_bytes = hasher.finalize();
    let mut hex = String::with_capacity(hash_bytes.len() * 2);
    for byte in hash_bytes.iter() {
        hex.push(HEX_CHARS[(byte >> 4) as usize] as char);
        hex.push(HEX_CHARS[(byte & 0x0f) as usize] as char);
    }
    hex
}

fn hash_permission_ast(hasher: &mut Sha256, where_arg: &WhereArg) {
    match where_arg {
        WhereArg::Column(is_session, fieldname, op, value, _field_name_range) => {
            hasher.update("column");
            hasher.update(if *is_session { "session" } else { "table" });
            hasher.update(fieldname);
            // Convert operator to string without Debug formatting
            let op_str = match op {
                ast::Operator::Equal => "Equal",
                ast::Operator::NotEqual => "NotEqual",
                ast::Operator::GreaterThan => "GreaterThan",
                ast::Operator::LessThan => "LessThan",
                ast::Operator::GreaterThanOrEqual => "GreaterThanOrEqual",
                ast::Operator::LessThanOrEqual => "LessThanOrEqual",
                ast::Operator::In => "In",
                ast::Operator::NotIn => "NotIn",
                ast::Operator::Like => "Like",
                ast::Operator::NotLike => "NotLike",
            };
            hasher.update(op_str);
            hash_query_value(hasher, value);
        }
        WhereArg::And(args) => {
            hasher.update("and");
            for arg in args {
                hash_permission_ast(hasher, arg);
            }
        }
        WhereArg::Or(args) => {
            hasher.update("or");
            for arg in args {
                hash_permission_ast(hasher, arg);
            }
        }
    }
}

fn hash_query_value(hasher: &mut Sha256, value: &ast::QueryValue) {
    match value {
        ast::QueryValue::Fn(func) => {
            hasher.update("fn");
            hasher.update(&func.name);
            for arg in &func.args {
                hash_query_value(hasher, arg);
            }
        }
        ast::QueryValue::Variable((_, var)) => {
            hasher.update("var");
            hasher.update(&var.name);
        }
        ast::QueryValue::String((_, s)) => {
            hasher.update("string");
            hasher.update(s);
        }
        ast::QueryValue::Int((_, i)) => {
            hasher.update("int");
            // Convert integer to string without formatting infrastructure
            let mut num_str = String::new();
            let mut n = *i;
            if n < 0 {
                num_str.push('-');
                n = -n;
            }
            if n == 0 {
                num_str.push('0');
            } else {
                let mut digits = Vec::new();
                while n > 0 {
                    digits.push((b'0' + (n % 10) as u8) as char);
                    n /= 10;
                }
                for d in digits.iter().rev() {
                    num_str.push(*d);
                }
            }
            hasher.update(&num_str);
        }
        ast::QueryValue::Float((_, f)) => {
            hasher.update("float");
            // For floats, hash the bits directly to avoid formatting
            // Convert f32 bits to bytes for hashing
            let bits = f.to_bits();
            let bytes = bits.to_le_bytes();
            hasher.update(&bytes);
        }
        ast::QueryValue::Bool((_, b)) => {
            hasher.update("bool");
            hasher.update(if *b { "true" } else { "false" });
        }
        ast::QueryValue::Null(_) => {
            hasher.update("null");
        }
        ast::QueryValue::LiteralTypeValue((_, details)) => {
            hasher.update("literal");
            hasher.update(&details.name);
        }
    }
}

fn hash_session_value(hasher: &mut Sha256, value: &SessionValue) {
    match value {
        SessionValue::Null => hasher.update("null"),
        SessionValue::Integer(i) => {
            hasher.update("int");
            // Convert integer to string without formatting infrastructure
            let mut num_str = String::new();
            let mut n = *i;
            if n < 0 {
                num_str.push('-');
                n = -n;
            }
            if n == 0 {
                num_str.push('0');
            } else {
                let mut digits = Vec::new();
                while n > 0 {
                    digits.push((b'0' + (n % 10) as u8) as char);
                    n /= 10;
                }
                for d in digits.iter().rev() {
                    num_str.push(*d);
                }
            }
            hasher.update(&num_str);
        }
        SessionValue::Real(f) => {
            hasher.update("real");
            // For floats, hash the bits directly to avoid formatting
            let bits = f.to_bits();
            let bytes = bits.to_le_bytes();
            hasher.update(&bytes);
        }
        SessionValue::Text(s) => {
            hasher.update("text");
            hasher.update(s);
        }
        SessionValue::Blob(b) => {
            hasher.update("blob");
            // Hash blob bytes directly instead of Debug formatting
            hasher.update(b);
        }
    }
}

fn render_session_param(value: &SessionValue, params: &mut Vec<SessionValue>) -> String {
    params.push(value.clone());
    "?".to_string()
}

fn render_permission_value(
    value: &ast::QueryValue,
    session: &HashMap<String, SessionValue>,
    params: &mut Vec<SessionValue>,
) -> String {
    match value {
        ast::QueryValue::Variable((_, var)) => {
            if let Some(session_key) = &var.session_field {
                let session_value = session
                    .get(session_key)
                    .expect("Session variable should exist after typechecking");
                render_session_param(session_value, params)
            } else {
                crate::generate::sql::to_sql::render_value(value)
            }
        }
        _ => crate::generate::sql::to_sql::render_value(value),
    }
}

/// Render a permission WHERE clause to SQL
/// This is a custom renderer for sync operations that doesn't require QueryField or QueryInfo
/// Handles session variable replacement internally
fn render_permission_where(
    where_arg: &WhereArg,
    table: &typecheck::Table,
    session: &HashMap<String, SessionValue>,
    params: &mut Vec<SessionValue>,
) -> String {
    match where_arg {
        WhereArg::Column(is_session_var, fieldname, op, value, _field_name_range) => {
            let qualified_column_name = if *is_session_var {
                let session_value = session
                    .get(fieldname)
                    .expect("Session variable should exist after typechecking");
                render_session_param(session_value, params)
            } else {
                let table_name = crate::ext::string::quote(&ast::get_tablename(
                    &table.record.name,
                    &table.record.fields,
                ));
                format!("{}.{}", table_name, crate::ext::string::quote(fieldname))
            };

            let value_str = render_permission_value(value, session, params);
            let value_str = if matches!(op, ast::Operator::In | ast::Operator::NotIn)
                && matches!(value, ast::QueryValue::Variable((_, var)) if var.session_field.is_some())
            {
                format!("(select value from json_each({}))", value_str)
            } else {
                value_str
            };
            let is_null = match value {
                ast::QueryValue::Variable((_, var)) => var
                    .session_field
                    .as_ref()
                    .and_then(|session_key| session.get(session_key))
                    .map(|value| matches!(value, SessionValue::Null))
                    .unwrap_or(false),
                _ => matches!(value, ast::QueryValue::Null(_)),
            };
            let operator_str = if is_null {
                match op {
                    ast::Operator::Equal => "is".to_string(),
                    ast::Operator::NotEqual => "is not".to_string(),
                    _ => crate::generate::sql::to_sql::operator(op),
                }
            } else {
                crate::generate::sql::to_sql::operator(op)
            };
            format!("{} {} {}", qualified_column_name, operator_str, value_str)
        }
        WhereArg::And(args) => {
            let inner_list: Vec<String> = args
                .iter()
                .map(|arg| render_permission_where(arg, table, session, params))
                .collect();
            format!("({})", inner_list.join(" and "))
        }
        WhereArg::Or(args) => {
            let inner_list: Vec<String> = args
                .iter()
                .map(|arg| render_permission_where(arg, table, session, params))
                .collect();
            format!("({})", inner_list.join(" or "))
        }
    }
}

pub fn get_sync_status_statement(
    sync_cursor: &SyncCursor,
    context: &typecheck::Context,
    session: &HashMap<String, SessionValue>,
) -> Result<SyncStatement, SyncError> {
    validate_sync_cursor(sync_cursor, context)?;
    let mut params = Vec::new();
    let sql = get_sync_status_sql_with_params(sync_cursor, context, session, &mut params)?;
    Ok(SyncStatement { sql, params })
}

/// Get sync status SQL - generates a single SQL query that checks which tables need syncing
/// Returns SQL that can be executed to get sync status for all tables
pub fn get_sync_status_sql(
    sync_cursor: &SyncCursor,
    context: &typecheck::Context,
    session: &HashMap<String, SessionValue>,
) -> Result<String, SyncError> {
    let statement = get_sync_status_statement(sync_cursor, context, session)?;
    if !statement.params.is_empty() {
        return Err(SyncError::SqlGenerationError(
            "sync status SQL requires bind params; use get_sync_status_statement".to_string(),
        ));
    }

    Ok(statement.sql)
}

fn get_sync_status_sql_with_params(
    sync_cursor: &SyncCursor,
    context: &typecheck::Context,
    session: &HashMap<String, SessionValue>,
    params: &mut Vec<SessionValue>,
) -> Result<String, SyncError> {
    use crate::ext::string;

    let mut union_parts = Vec::new();

    // Iterate through all tables in the context
    for (_record_name, table) in &context.tables {
        if !table_sync_enabled(context, table) {
            continue;
        }

        // Get the actual table name from @tablename directive
        let actual_table_name = ast::get_tablename(&table.record.name, &table.record.fields);
        let quoted_table_name = string::quote(&actual_table_name);

        // Get permission for select operation
        let permission = ast::get_permissions(&table.record, &ast::QueryOperation::Query);

        // Calculate current permission hash
        let current_permission_hash = calculate_permission_hash(&permission, session);

        // Get cursor state for this table
        let table_cursor = sync_cursor.get(&actual_table_name);
        let last_seen_updated_at = table_cursor.and_then(|c| c.last_seen_updated_at);

        // Build WHERE clause for permissions. Session values are emitted as bind parameters.
        let permission_where = if let Some(perm) = &permission {
            format!(
                " WHERE {}",
                render_permission_where(perm, table, session, params)
            )
        } else {
            String::new()
        };

        // Build the subquery for this table
        // We compute MAX(updatedAt) with permissions applied
        // Also include the sync_layer, table_name, permission_hash, and last_seen_updated_at
        let sync_layer_value = table.sync_layer;
        let table_name_literal = string::single_quote(&actual_table_name);
        let permission_hash_literal = string::single_quote(&current_permission_hash);
        let last_seen_literal = match last_seen_updated_at {
            Some(ts) => ts.to_string(),
            None => "NULL".to_string(),
        };

        let subquery = format!(
            "SELECT {} AS table_name, {} AS sync_layer, {} AS permission_hash, {} AS last_seen_updated_at, MAX({}.updatedAt) AS max_updated_at, (SELECT value FROM _pyre_sync WHERE key = 'server_revision') AS server_revision FROM {}{}",
            table_name_literal,
            sync_layer_value,
            permission_hash_literal,
            last_seen_literal,
            quoted_table_name,
            quoted_table_name,
            permission_where
        );

        union_parts.push(subquery);
    }

    if union_parts.is_empty() {
        return Ok(
            "SELECT NULL AS table_name, NULL AS sync_layer, NULL AS permission_hash, NULL AS last_seen_updated_at, NULL AS max_updated_at, (SELECT value FROM _pyre_sync WHERE key = 'server_revision') AS server_revision"
                .to_string(),
        );
    }

    // Combine all subqueries with UNION ALL
    let sql = union_parts.join(" UNION ALL ");
    Ok(sql)
}

/// Parse sync status results from SQL query execution
/// The SQL should return rows with: table_name, sync_layer, permission_hash, last_seen_updated_at, max_updated_at
pub fn parse_sync_status(
    sync_cursor: &SyncCursor,
    _context: &typecheck::Context,
    _session: &HashMap<String, SessionValue>,
    rows: &[std::collections::HashMap<String, serde_json::Value>],
) -> Result<SyncStatusResult, SyncError> {
    let mut result = SyncStatusResult {
        server_revision: None,
        tables: Vec::new(),
    };

    for row in rows {
        if result.server_revision.is_none() {
            result.server_revision = row.get("server_revision").and_then(|v| {
                if v.is_null() {
                    None
                } else {
                    v.as_i64().or_else(|| v.as_u64().map(|u| u as i64))
                }
            });
        }

        let Some(table_name) = row
            .get("table_name")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string())
        else {
            continue;
        };

        let sync_layer = row
            .get("sync_layer")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                SyncError::SqlGenerationError("Missing sync_layer in sync status row".to_string())
            })? as usize;

        let permission_hash = row
            .get("permission_hash")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                SyncError::SqlGenerationError(
                    "Missing permission_hash in sync status row".to_string(),
                )
            })?
            .to_string();

        let max_updated_at = row.get("max_updated_at").and_then(|v| {
            if v.is_null() {
                None
            } else {
                v.as_i64().or_else(|| v.as_u64().map(|u| u as i64))
            }
        });

        let last_seen_updated_at = row.get("last_seen_updated_at").and_then(|v| {
            if v.is_null() {
                None
            } else {
                v.as_i64().or_else(|| v.as_u64().map(|u| u as i64))
            }
        });

        // Check if permission hash changed
        let table_cursor = sync_cursor.get(&table_name);
        let permission_hash_changed = match table_cursor {
            Some(cursor) => cursor.permission_hash != permission_hash,
            None => true, // No cursor means first sync
        };

        // Check if max_updated_at > last_seen_updated_at
        let has_new_data = match (max_updated_at, last_seen_updated_at) {
            (Some(max), Some(last)) => max > last,
            (Some(_), None) => true, // Has data but no cursor
            (None, _) => false,      // No data
        };

        let needs_sync = permission_hash_changed || has_new_data;

        result.tables.push(TableSyncStatus {
            table_name,
            sync_layer,
            needs_sync,
            max_updated_at,
            permission_hash,
        });
    }

    // Sort by sync_layer (lower numbers first)
    result.tables.sort_by_key(|t| t.sync_layer);

    Ok(result)
}

/// Get sync SQL for all tables that need syncing
/// Generates SQL directly with permission filters and bind parameters for session values.
/// Only syncs tables that need syncing, ordered by sync_layer
pub fn get_sync_sql(
    sync_status: &SyncStatusResult,
    sync_cursor: &SyncCursor,
    context: &typecheck::Context,
    session: &HashMap<String, SessionValue>,
    page_size: usize,
) -> Result<SyncSqlResult, SyncError> {
    use crate::ext::string;
    validate_sync_cursor(sync_cursor, context)?;
    let effective_page_size = normalize_page_size(page_size)?;

    let mut result = SyncSqlResult { tables: Vec::new() };

    // Iterate through tables that need syncing, ordered by sync_layer
    // sync_status.tables is already sorted by sync_layer
    for status in &sync_status.tables {
        if !status.needs_sync {
            continue;
        }

        // Find the table in context by table name
        let table = context
            .tables
            .values()
            .find(|t| {
                let actual_table_name = ast::get_tablename(&t.record.name, &t.record.fields);
                actual_table_name == status.table_name
            })
            .ok_or_else(|| {
                SyncError::SqlGenerationError(
                    "Table ".to_string() + &status.table_name + " not found in context",
                )
            })?;
        if !table_sync_enabled(context, table) {
            continue;
        }

        let actual_table_name = &status.table_name;

        // Get permission for select operation
        let permission = ast::get_permissions(&table.record, &ast::QueryOperation::Query);

        // Use permission hash from status (already calculated)
        let current_permission_hash = &status.permission_hash;

        // Check if permission hash changed to determine if we need full resync
        let table_cursor = sync_cursor.get(actual_table_name);
        let needs_full_resync = match table_cursor {
            Some(cursor) => cursor.permission_hash != *current_permission_hash,
            None => true, // No cursor means first sync
        };

        // Determine the last_seen_updated_at to use
        let last_seen_updated_at = if needs_full_resync {
            None // Full resync - start from beginning
        } else {
            // Use the last_seen_updated_at from cursor (not max_updated_at from status)
            table_cursor.and_then(|c| c.last_seen_updated_at)
        };

        // Build WHERE clause combining permissions and updatedAt filter
        let mut where_parts = Vec::new();
        let mut params = Vec::new();

        // Add permission WHERE clause. Session values are emitted as bind parameters.
        if let Some(perm) = &permission {
            where_parts.push(render_permission_where(perm, table, session, &mut params));
        }

        // Add updatedAt filter if provided
        if let Some(updated_at) = last_seen_updated_at {
            let table_name = crate::ext::string::quote(&ast::get_tablename(
                &table.record.name,
                &table.record.fields,
            ));
            params.push(SessionValue::Integer(updated_at));
            let updated_at_where = format!(
                "{}.{} > ?",
                table_name,
                crate::ext::string::quote("updatedAt")
            );
            where_parts.push(updated_at_where);
        }

        // Build WHERE clause SQL
        let where_clause = if where_parts.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", where_parts.join(" AND "))
        };

        // Build column list and headers
        let mut columns = Vec::new();
        let mut headers = Vec::new();
        let mut json_columns = Vec::new();
        for field in &table.record.fields {
            if let ast::Field::Column(col) = field {
                collect_sync_storage_columns(
                    context,
                    &col.type_,
                    &col.name,
                    &mut headers,
                    &mut json_columns,
                );
            }
        }

        let quoted_table_name = string::quote(&actual_table_name);
        let json_column_set = json_columns.iter().cloned().collect::<HashSet<_>>();
        for header in &headers {
            let quoted_col_name = string::quote(header);
            if json_column_set.contains(header) {
                columns.push(format!(
                    "json({}.{}) as {}",
                    quoted_table_name, quoted_col_name, quoted_col_name
                ));
            } else {
                columns.push(format!("{}.{}", quoted_table_name, quoted_col_name));
            }
        }

        if columns.is_empty() {
            return Err(SyncError::SqlGenerationError(format!(
                "Table {} has no columns",
                actual_table_name
            )));
        }

        let row_values = headers
            .iter()
            .map(|header| {
                let quoted_header = string::quote(header);
                if json_column_set.contains(header) {
                    format!("json({})", quoted_header)
                } else {
                    quoted_header
                }
            })
            .collect::<Vec<_>>();

        let sql = format!(
            "SELECT coalesce(json_group_array(json_array({})), json('[]')) AS {} FROM (SELECT {} FROM {}{} ORDER BY {}.updatedAt ASC LIMIT {})",
            row_values.join(", "),
            string::quote(SYNC_ROWS_JSON_COLUMN),
            columns.join(", "),
            quoted_table_name,
            where_clause,
            quoted_table_name,
            effective_page_size + 1 // +1 to check if there's more
        );

        result.tables.push(TableSyncSql {
            table_name: actual_table_name.clone(),
            permission_hash: current_permission_hash.clone(),
            sql: vec![sql], // Single SQL statement
            params: vec![params],
            headers,
            json_columns,
        });
    }

    Ok(result)
}

/// Get sync page info - calculates permission hashes and determines what needs syncing
/// The actual query execution should be done separately using the generated queries
pub fn get_sync_page_info(
    sync_cursor: &SyncCursor,
    context: &typecheck::Context,
    session: &HashMap<String, SessionValue>,
    _page_size: usize,
) -> SyncPageResult {
    let mut result = SyncPageResult {
        database_id: None,
        server_revision: None,
        tables: HashMap::new(),
        has_more: false,
    };

    // Iterate through all tables in the context
    for (_record_name, table) in &context.tables {
        if !table_sync_enabled(context, table) {
            continue;
        }

        // Get the actual table name from @tablename directive
        let actual_table_name = ast::get_tablename(&table.record.name, &table.record.fields);

        // Get permission for select operation
        let permission = ast::get_permissions(&table.record, &ast::QueryOperation::Query);

        // Calculate current permission hash
        let current_permission_hash = calculate_permission_hash(&permission, session);

        // Get cursor state for this table (use actual table name)
        let table_cursor = sync_cursor.get(&actual_table_name);

        // Check if permission hash matches
        let needs_full_resync = match table_cursor {
            Some(cursor) => cursor.permission_hash != current_permission_hash,
            None => true, // No cursor means first sync
        };

        // Determine the last_seen_updated_at to use
        let last_seen_updated_at = if needs_full_resync {
            None // Full resync - start from beginning
        } else {
            table_cursor.and_then(|c| c.last_seen_updated_at)
        };

        // Return sync info - actual query execution happens separately
        // Use actual table name as the key
        result.tables.insert(
            actual_table_name,
            TableSyncData {
                rows: Vec::new(), // Will be populated by query execution
                permission_hash: current_permission_hash,
                last_seen_updated_at,
            },
        );
    }

    result
}

#[derive(Debug)]
pub enum SyncError {
    DatabaseError(String),
    SqlGenerationError(String),
    PermissionError(String),
    InvalidPageSize,
    InvalidSyncCursor(String),
}

// Display and Error traits removed to avoid formatting infrastructure
// Errors are converted to strings manually in WASM code
