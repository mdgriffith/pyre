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
    #[serde(default)]
    pub last_seen_primary_key: Option<JsonValue>,
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
    /// Opaque identity for the current lifetime of the source database.
    #[serde(rename = "databaseEpoch")]
    pub database_epoch: String,
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
    /// The primary key of the last returned row, used to disambiguate equal timestamps.
    pub last_seen_primary_key: Option<JsonValue>,
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
    pub primary_key: String,
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

        if cursor.last_seen_updated_at.is_none() && cursor.last_seen_primary_key.is_some() {
            return Err(SyncError::InvalidSyncCursor(format!(
                "sync cursor primary key for '{}' requires last_seen_updated_at",
                table_name
            )));
        }

        if let Some(primary_key) = &cursor.last_seen_primary_key {
            if primary_key.as_i64().is_none() && primary_key.as_str().is_none() {
                return Err(SyncError::InvalidSyncCursor(format!(
                    "sync cursor primary key for '{}' must be an integer or string",
                    table_name
                )));
            }
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
    pub max_primary_key: Option<JsonValue>,
    pub permission_hash: String,
}

/// Result of sync status check
pub struct SyncStatusResult {
    pub server_revision: Option<i64>,
    pub database_epoch: String,
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
        WhereArg::Constant(_) => {}
        WhereArg::Exists(_, body) => extract_session_fields_recursive(body, fields),
        WhereArg::Column(is_session_var, path, _, value, _field_name_range) => {
            if *is_session_var {
                extract_session_path_fields(path, fields);
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
            if let Some(path) = var.session_path() {
                extract_session_path_fields(&path, fields);
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

fn extract_session_path_fields(path: &ast::PredicatePath, fields: &mut Vec<String>) {
    for (index, segment) in path.segments.iter().enumerate() {
        if matches!(segment, ast::PredicatePathSegment::Variant(_)) {
            let discriminator = ast::PredicatePath {
                segments: path.segments[..index].to_vec(),
            }
            .flattened();
            if !fields.contains(&discriminator) {
                fields.push(discriminator);
            }
        }
    }
    let terminal = path.flattened();
    if !fields.contains(&terminal) {
        fields.push(terminal);
    }
}

/// Calculate permission hash from permission AST and session values
pub fn calculate_permission_hash(
    permission: &Option<WhereArg>,
    session: &HashMap<String, SessionValue>,
) -> String {
    let mut hasher = Sha256::new();
    // v3 forces timestamp-only cursors to perform one full sync and acquire
    // the primary-key component required for lossless pagination.
    hasher.update("permission_hash_v3");

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
                hasher.update((field.len() as u64).to_le_bytes());
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
        WhereArg::Constant(value) => {
            hasher.update(if *value {
                "constant_true"
            } else {
                "constant_false"
            });
        }
        WhereArg::Exists(path, body) => {
            hasher.update("exists");
            hasher.update((path.len() as u64).to_le_bytes());
            for (segment, _) in path {
                hasher.update((segment.len() as u64).to_le_bytes());
                hasher.update(segment);
            }
            hash_permission_ast(hasher, body);
        }
        WhereArg::Column(is_session, path, op, value, _field_name_range) => {
            hasher.update("column");
            hasher.update(if *is_session { "session" } else { "table" });
            if path.is_simple() {
                // Preserve existing cursors for permissions that predate predicate paths.
                hasher.update(path.root());
            } else {
                hasher.update("predicate_path_v1");
                hasher.update((path.segments.len() as u64).to_le_bytes());
                for segment in &path.segments {
                    let (kind, name) = match segment {
                        ast::PredicatePathSegment::Field(name) => (0_u8, name),
                        ast::PredicatePathSegment::Variant(name) => (1_u8, name),
                    };
                    hasher.update([kind]);
                    hasher.update((name.len() as u64).to_le_bytes());
                    hasher.update(name);
                }
            }
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
            if let Some(path) = var.session_path().filter(|path| !path.is_simple()) {
                hasher.update("session_predicate_path_v1");
                hasher.update((path.segments.len() as u64).to_le_bytes());
                for segment in path.segments {
                    let (kind, name) = match segment {
                        ast::PredicatePathSegment::Field(name) => (0_u8, name),
                        ast::PredicatePathSegment::Variant(name) => (1_u8, name),
                    };
                    hasher.update([kind]);
                    hasher.update((name.len() as u64).to_le_bytes());
                    hasher.update(name);
                }
            } else {
                hasher.update(&var.name);
            }
        }
        ast::QueryValue::String((_, s)) => {
            hasher.update("string");
            hasher.update(s);
        }
        ast::QueryValue::Int((_, i)) => {
            hasher.update("int");
            hasher.update(i.to_le_bytes());
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
            hasher.update(i.to_le_bytes());
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
            hasher.update((s.len() as u64).to_le_bytes());
            hasher.update(s);
        }
        SessionValue::Blob(b) => {
            hasher.update("blob");
            hasher.update((b.len() as u64).to_le_bytes());
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
            if let Some(path) = var.session_path() {
                let session_key = path.flattened();
                let session_value = session.get(&session_key).unwrap_or(&SessionValue::Null);
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
    context: &typecheck::Context,
    where_arg: &WhereArg,
    table: &typecheck::Table,
    session: &HashMap<String, SessionValue>,
    params: &mut Vec<SessionValue>,
) -> String {
    match where_arg {
        WhereArg::Constant(value) => if *value { "1" } else { "0" }.to_string(),
        WhereArg::Exists(..) => "0".to_string(),
        WhereArg::Column(is_session_var, path, op, value, _field_name_range) => {
            let fieldname = path.root();
            let resolved = if *is_session_var {
                context.session.as_ref().and_then(|session| {
                    typecheck::resolve_predicate_path(context, &session.fields, path).ok()
                })
            } else {
                typecheck::resolve_predicate_path(context, &table.record.fields, path).ok()
            };
            let qualified_column_name = if *is_session_var {
                let physical = resolved
                    .as_ref()
                    .map(|path| path.physical_column.as_str())
                    .unwrap_or(fieldname);
                let session_value = session.get(physical).unwrap_or(&SessionValue::Null);
                render_session_param(session_value, params)
            } else {
                let table_name = crate::ext::string::quote(&ast::get_tablename(
                    &table.record.name,
                    &table.record.fields,
                ));
                let physical = resolved
                    .as_ref()
                    .map(|path| path.physical_column.as_str())
                    .unwrap_or(fieldname);
                format!("{}.{}", table_name, crate::ext::string::quote(physical))
            };

            let value_str = render_permission_value(value, session, params);
            let value_str = if matches!(op, ast::Operator::In | ast::Operator::NotIn)
                && matches!(value, ast::QueryValue::Variable((_, var)) if var.session_field.is_some())
            {
                format!("(select value from json_each({}))", value_str)
            } else {
                value_str
            };
            let runtime_value_is_null = match value {
                ast::QueryValue::Variable((_, var)) => var
                    .session_field
                    .as_ref()
                    .and_then(|_| var.session_path())
                    .and_then(|path| session.get(&path.flattened()))
                    .map(|value| matches!(value, SessionValue::Null))
                    .unwrap_or(false),
                _ => matches!(value, ast::QueryValue::Null(_)),
            };
            let null_safe = runtime_value_is_null
                || typecheck::predicate_operand_is_nullable(
                    context,
                    &table.record.fields,
                    *is_session_var,
                    path,
                )
                || typecheck::query_value_is_nullable(context, value);
            let operator_str = if null_safe {
                match op {
                    ast::Operator::Equal => "is".to_string(),
                    ast::Operator::NotEqual => "is not".to_string(),
                    _ => crate::generate::sql::to_sql::operator(op),
                }
            } else {
                crate::generate::sql::to_sql::operator(op)
            };
            let comparison = format!("{} {} {}", qualified_column_name, operator_str, value_str);
            let mut guards = Vec::new();
            if !*is_session_var {
                if let Some(resolved) = &resolved {
                    let table_name = crate::ext::string::quote(&ast::get_tablename(
                        &table.record.name,
                        &table.record.fields,
                    ));
                    guards.extend(resolved.discriminators.iter().map(|(column, variant)| {
                        format!(
                            "{}.{} = '{}'",
                            table_name,
                            crate::ext::string::quote(column),
                            variant.replace("'", "''")
                        )
                    }));
                }
            }
            guards.push(comparison);
            if *is_session_var {
                if let Some(resolved) = &resolved {
                    guards.extend(resolved.discriminators.iter().map(|(column, variant)| {
                        let value = session.get(column).unwrap_or(&SessionValue::Null);
                        format!(
                            "{} = '{}'",
                            render_session_param(value, params),
                            variant.replace("'", "''")
                        )
                    }));
                }
            }
            if let ast::QueryValue::Variable((_, variable)) = value {
                if let Some(session_path) = variable.session_path() {
                    if let Some(session_schema) = &context.session {
                        if let Ok(resolved) = typecheck::resolve_predicate_path(
                            context,
                            &session_schema.fields,
                            &session_path,
                        ) {
                            guards.extend(resolved.discriminators.iter().map(
                                |(column, variant)| {
                                    let value = session.get(column).unwrap_or(&SessionValue::Null);
                                    format!(
                                        "{} = '{}'",
                                        render_session_param(value, params),
                                        variant.replace("'", "''")
                                    )
                                },
                            ));
                        }
                    }
                }
            }
            if guards.len() == 1 {
                return guards.pop().unwrap_or_default();
            }
            format!("({})", guards.join(" and "))
        }
        WhereArg::And(args) => {
            let inner_list: Vec<String> = args
                .iter()
                .map(|arg| render_permission_where(context, arg, table, session, params))
                .collect();
            format!("({})", inner_list.join(" and "))
        }
        WhereArg::Or(args) => {
            let inner_list: Vec<String> = args
                .iter()
                .map(|arg| render_permission_where(context, arg, table, session, params))
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
        let primary_key =
            ast::get_primary_id_field_name(&table.record.fields).ok_or_else(|| {
                SyncError::SqlGenerationError(format!(
                    "Table {} has no primary key",
                    actual_table_name
                ))
            })?;
        let quoted_primary_key = string::quote(&primary_key);

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
                render_permission_where(context, perm, table, session, params)
            )
        } else {
            String::new()
        };

        // Select the final tuple in the same ordering used by catch-up pagination.
        let sync_layer_value = table.sync_layer;
        let table_name_literal = string::single_quote(&actual_table_name);
        let permission_hash_literal = string::single_quote(&current_permission_hash);
        let last_seen_literal = match last_seen_updated_at {
            Some(ts) => ts.to_string(),
            None => "NULL".to_string(),
        };

        let subquery = format!(
            "SELECT {} AS table_name, {} AS sync_layer, {} AS permission_hash, {} AS last_seen_updated_at, _pyre_latest.updatedAt AS max_updated_at, _pyre_latest.{} AS max_primary_key, (SELECT server_revision FROM _pyre_sync WHERE id = 1) AS server_revision, (SELECT database_epoch FROM _pyre_sync WHERE id = 1) AS database_epoch FROM (SELECT 1) AS _pyre_status LEFT JOIN (SELECT updatedAt, {} FROM {}{} ORDER BY updatedAt DESC, {} DESC LIMIT 1) AS _pyre_latest ON 1 = 1",
            table_name_literal,
            sync_layer_value,
            permission_hash_literal,
            last_seen_literal,
            quoted_primary_key,
            quoted_primary_key,
            quoted_table_name,
            permission_where,
            quoted_primary_key
        );

        union_parts.push(subquery);
    }

    if union_parts.is_empty() {
        return Ok(
            "SELECT NULL AS table_name, NULL AS sync_layer, NULL AS permission_hash, NULL AS last_seen_updated_at, NULL AS max_updated_at, NULL AS max_primary_key, (SELECT server_revision FROM _pyre_sync WHERE id = 1) AS server_revision, (SELECT database_epoch FROM _pyre_sync WHERE id = 1) AS database_epoch"
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
        database_epoch: String::new(),
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
        if result.database_epoch.is_empty() {
            result.database_epoch = row
                .get("database_epoch")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    SyncError::DatabaseError("missing database_epoch in _pyre_sync".to_string())
                })?
                .to_string();
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

        let max_primary_key = row
            .get("max_primary_key")
            .filter(|value| !value.is_null())
            .cloned();

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

        let last_seen_primary_key =
            table_cursor.and_then(|cursor| cursor.last_seen_primary_key.as_ref());

        // Timestamp-only cursors retain their legacy behavior until the v3
        // permission hash forces a full sync and supplies the key component.
        let has_new_data = match (max_updated_at, last_seen_updated_at) {
            (Some(max), Some(last)) if max > last => true,
            (Some(max), Some(last)) if max == last => {
                match (max_primary_key.as_ref(), last_seen_primary_key) {
                    (Some(max_key), Some(last_key)) => primary_key_is_greater(max_key, last_key)?,
                    _ => false,
                }
            }
            (Some(_), Some(_)) => false,
            (Some(_), None) => true, // Has data but no cursor
            (None, _) => false,      // No data
        };

        let needs_sync = permission_hash_changed || has_new_data;

        result.tables.push(TableSyncStatus {
            table_name,
            sync_layer,
            needs_sync,
            max_updated_at,
            max_primary_key,
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
        let primary_key =
            ast::get_primary_id_field_name(&table.record.fields).ok_or_else(|| {
                SyncError::SqlGenerationError(format!(
                    "Table {} has no primary key",
                    actual_table_name
                ))
            })?;

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
        let last_seen_primary_key = if needs_full_resync {
            None
        } else {
            table_cursor.and_then(|cursor| cursor.last_seen_primary_key.as_ref())
        };

        // Build WHERE clause combining permissions and updatedAt filter
        let mut where_parts = Vec::new();
        let mut params = Vec::new();

        // Add permission WHERE clause. Session values are emitted as bind parameters.
        if let Some(perm) = &permission {
            where_parts.push(render_permission_where(
                context,
                perm,
                table,
                session,
                &mut params,
            ));
        }

        // Add the keyset pagination filter. Timestamp-only cursors remain valid
        // for persisted pre-v3 client state.
        if let Some(updated_at) = last_seen_updated_at {
            let table_name = crate::ext::string::quote(&ast::get_tablename(
                &table.record.name,
                &table.record.fields,
            ));
            let updated_at_column = crate::ext::string::quote("updatedAt");
            let primary_key_column = crate::ext::string::quote(&primary_key);
            let updated_at_where = if let Some(primary_key_value) = last_seen_primary_key {
                params.push(SessionValue::Integer(updated_at));
                params.push(SessionValue::Integer(updated_at));
                params.push(primary_key_session_value(primary_key_value)?);
                format!(
                    "({table_name}.{updated_at_column} > ? OR ({table_name}.{updated_at_column} = ? AND {table_name}.{primary_key_column} > ?))"
                )
            } else {
                params.push(SessionValue::Integer(updated_at));
                format!("{table_name}.{updated_at_column} > ?")
            };
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
            "SELECT coalesce(json_group_array(json_array({})), json('[]')) AS {} FROM (SELECT {} FROM {}{} ORDER BY {}.updatedAt ASC, {}.{} ASC LIMIT {})",
            row_values.join(", "),
            string::quote(SYNC_ROWS_JSON_COLUMN),
            columns.join(", "),
            quoted_table_name,
            where_clause,
            quoted_table_name,
            quoted_table_name,
            string::quote(&primary_key),
            effective_page_size + 1 // +1 to check if there's more
        );

        result.tables.push(TableSyncSql {
            table_name: actual_table_name.clone(),
            primary_key,
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
        database_epoch: String::new(),
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
                last_seen_primary_key: if needs_full_resync {
                    None
                } else {
                    table_cursor.and_then(|cursor| cursor.last_seen_primary_key.clone())
                },
            },
        );
    }

    result
}

fn primary_key_session_value(value: &JsonValue) -> Result<SessionValue, SyncError> {
    if let Some(value) = value.as_i64() {
        Ok(SessionValue::Integer(value))
    } else if let Some(value) = value.as_str() {
        Ok(SessionValue::Text(value.to_string()))
    } else {
        Err(SyncError::InvalidSyncCursor(
            "sync cursor primary key must be an integer or string".to_string(),
        ))
    }
}

fn primary_key_is_greater(max: &JsonValue, last: &JsonValue) -> Result<bool, SyncError> {
    match (max.as_i64(), last.as_i64(), max.as_str(), last.as_str()) {
        (Some(max), Some(last), _, _) => Ok(max > last),
        (_, _, Some(max), Some(last)) => Ok(max > last),
        _ => Err(SyncError::InvalidSyncCursor(
            "sync cursor primary key does not match the database primary key type".to_string(),
        )),
    }
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
