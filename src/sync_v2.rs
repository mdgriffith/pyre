//! Timestamp/key catchup plus a durable deletion cursor. Each page is one
//! SQLite snapshot; its revision certifies only the keys returned by that page.
use crate::{ast, sync, sync_deltas::AffectedRowTableGroup, sync_shape, typecheck};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

pub const VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
pub struct Page {
    #[serde(rename = "syncVersion")]
    pub sync_version: u32,
    #[serde(rename = "databaseId")]
    pub database_id: String,
    #[serde(rename = "databaseEpoch")]
    pub database_epoch: String,
    #[serde(rename = "serverRevision")]
    pub server_revision: i64,
    #[serde(rename = "snapshotTimestamp")]
    pub snapshot_timestamp: i64,
    pub tables: HashMap<String, Table>,
    pub has_more: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Table {
    pub changes: Vec<Change>,
    pub permission_hash: String,
    pub last_seen_delete_sequence: i64,
    pub last_seen_updated_at: Option<i64>,
    pub last_seen_primary_key: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Change {
    pub op: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<Value>,
}

pub fn statement(
    context: &typecheck::Context,
    cursor: &sync::SyncCursor,
    session: &HashMap<String, sync::SessionValue>,
    page_size: usize,
) -> Result<sync::SyncStatement, String> {
    use crate::ext::string::{quote, single_quote};
    sync::validate_sync_cursor(cursor, context).map_err(|e| format!("{e:?}"))?;
    let size = sync::normalize_page_size(page_size).map_err(|e| format!("{e:?}"))?;
    let mut parts = Vec::new();
    let mut params = Vec::new();
    let mut tables = context.tables.values().collect::<Vec<_>>();
    if let Some(first) = tables.first() {
        if tables.iter().any(|table| table.schema != first.schema) {
            return Err("durable sync requires the context of one physical database, not a multi-namespace project".into());
        }
    }
    tables.sort_by_key(|table| (&table.schema, &table.record.name));
    for table in tables {
        if context
            .namespace_sync_modes
            .get(&table.schema)
            .copied()
            .unwrap_or(ast::SyncMode::Synced)
            != ast::SyncMode::Synced
        {
            continue;
        }
        let name = ast::get_tablename(&table.record.name, &table.record.fields);
        let key =
            ast::get_primary_id_field_name(&table.record.fields).ok_or("missing primary key")?;
        let permission = ast::get_permissions(&table.record, &ast::QueryOperation::Query);
        let hash = format!(
            "timestamp-v2:{}",
            sync::calculate_permission_hash(&permission, session)
        );
        let mut headers = Vec::new();
        let mut json_columns = Vec::new();
        for field in &table.record.fields {
            if let ast::Field::Column(column) = field {
                sync::collect_sync_storage_columns(
                    context,
                    &column.type_,
                    &column.name,
                    &mut headers,
                    &mut json_columns,
                );
            }
        }
        let columns = headers
            .iter()
            .map(|header| {
                let column = format!("{}.{}", quote(&name), quote(header));
                if json_columns.contains(header) {
                    format!("json({column})")
                } else {
                    column
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        // Bind permission params in SQL text order (the row subquery precedes
        // the event cursor). Deletions deliberately never evaluate permission.
        let permission_sql = permission
            .as_ref()
            .map(|permission| {
                sync::render_permission_where(context, permission, table, session, &mut params)
            })
            .unwrap_or_else(|| "1".into());
        let scan_permission_sql = permission
            .as_ref()
            .map(|permission| {
                sync::render_permission_where(context, permission, table, session, &mut params)
            })
            .unwrap_or_else(|| "1".into());
        let last = cursor
            .get(&name)
            .filter(|entry| entry.permission_hash == hash);
        let mut filter = format!("where ({scan_permission_sql})");
        if let Some(sequence) = last.and_then(|entry| entry.last_seen_updated_at) {
            params.push(sync::SessionValue::Integer(sequence));
            if let Some(cursor_key) = last.and_then(|entry| entry.last_seen_primary_key.as_ref()) {
                params.push(sync::SessionValue::Integer(sequence));
                params.push(if let Some(value) = cursor_key.as_i64() {
                    sync::SessionValue::Integer(value)
                } else {
                    sync::SessionValue::Text(
                        cursor_key.as_str().ok_or("invalid cursor key")?.into(),
                    )
                });
                filter.push_str(&format!(
                    " and ({table}.updatedAt > ? or ({table}.updatedAt = ? and {table}.{key} > ?))",
                    table = quote(&name),
                    key = quote(&key)
                ));
            } else {
                filter.push_str(&format!(" and {}.updatedAt >= ?", quote(&name)));
            }
        }
        let literal = single_quote(&name);
        let row_sql = format!("(select json_array({columns}) from {} where {}.{} = e.primary_key and ({permission_sql}))", quote(&name), quote(&name), quote(&key));
        let deleted = last
            .map(|entry| entry.last_seen_delete_sequence)
            .unwrap_or(0);
        if deleted < 0 {
            return Err("negative deletion cursor".into());
        }
        parts.push(format!(
            "select {literal} as table_name, {} as headers_json, {} as permission_hash, database_epoch, server_revision, (select coalesce(json_group_array(json_array(e.sequence, e.primary_key, e.op, json({row_sql}))), json('[]')) from (select * from (select updatedAt as sequence, {key} as primary_key, 'row' as op from {name} {filter} order by sequence, primary_key limit {limit}) union all select * from (select sequence, primary_key, 'delete' as op from _pyre_sync_tombstones where table_name = {literal} and sequence > {deleted} order by sequence limit {limit})) e) as events_json from _pyre_sync where id = 1",
            single_quote(&serde_json::to_string(&headers).unwrap()), single_quote(&hash), key = quote(&key), name = quote(&name), limit = size + 1
        ));
    }
    if parts.is_empty() {
        parts.push("select null as table_name, '[]' as headers_json, '' as permission_hash, database_epoch, server_revision, '[]' as events_json from _pyre_sync where id = 1".into());
    }
    Ok(sync::SyncStatement {
        sql: parts.join(" union all "),
        params,
    })
}

pub fn page(
    context: &typecheck::Context,
    cursor: &sync::SyncCursor,
    rows: &[HashMap<String, Value>],
    page_size: usize,
    database_id: String,
    snapshot_timestamp: i64,
) -> Result<Page, String> {
    let size = sync::normalize_page_size(page_size).map_err(|e| format!("{e:?}"))?;
    let first = rows
        .first()
        .ok_or("missing sync metadata; migrate database first")?;
    let mut page = Page {
        sync_version: VERSION,
        snapshot_timestamp,
        database_id,
        database_epoch: first
            .get("database_epoch")
            .and_then(Value::as_str)
            .ok_or("missing epoch")?
            .into(),
        server_revision: first
            .get("server_revision")
            .and_then(Value::as_i64)
            .ok_or("missing revision")?,
        tables: HashMap::new(),
        has_more: false,
    };
    const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
    if !(0..=MAX_SAFE_INTEGER).contains(&page.server_revision) {
        return Err("sync revision exceeds the client safe integer range".into());
    }
    for raw in rows {
        let Some(name) = raw.get("table_name").and_then(Value::as_str) else {
            continue;
        };
        let headers: Vec<String> =
            serde_json::from_str(raw["headers_json"].as_str().ok_or("invalid headers")?)
                .map_err(|e| e.to_string())?;
        let mut events: Vec<Vec<Value>> =
            serde_json::from_str(raw["events_json"].as_str().ok_or("invalid events")?)
                .map_err(|e| e.to_string())?;
        let mut row_count = 0;
        let mut delete_count = 0;
        events.retain(|event| {
            let count = if event.get(2).is_some_and(|op| op == "delete") {
                &mut delete_count
            } else {
                &mut row_count
            };
            *count += 1;
            *count <= size
        });
        page.has_more |= row_count > size || delete_count > size;
        let hash = raw["permission_hash"]
            .as_str()
            .ok_or("invalid permission hash")?
            .to_string();
        let previous = cursor
            .get(name)
            .filter(|entry| entry.permission_hash == hash);
        let mut table = Table {
            last_seen_delete_sequence: previous
                .map(|entry| entry.last_seen_delete_sequence)
                .unwrap_or(0),
            changes: Vec::new(),
            permission_hash: hash,
            last_seen_updated_at: previous.and_then(|entry| entry.last_seen_updated_at),
            last_seen_primary_key: previous.and_then(|entry| entry.last_seen_primary_key.clone()),
        };
        for event in events {
            if event.len() != 4 {
                return Err("invalid sync event".into());
            }
            if event[1].as_str().is_none()
                && !event[1]
                    .as_i64()
                    .is_some_and(|key| (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&key))
            {
                return Err("sync primary key must be text or a safe integer".into());
            }
            if event[2] == "delete" {
                table.last_seen_delete_sequence =
                    event[0].as_i64().ok_or("invalid deletion sequence")?;
                table.changes.push(Change {
                    op: "delete".into(),
                    id: event[1].clone(),
                    row: None,
                });
            } else {
                table.last_seen_updated_at = event[0].as_i64();
                table.last_seen_primary_key = Some(event[1].clone());
            }
            // Reinserted keys are restored from the same snapshot, never from
            // an old tombstone payload. Deletes themselves remain unfiltered.
            if let Some(row) = event[3].as_array() {
                let group = AffectedRowTableGroup {
                    table_name: name.into(),
                    headers: headers.clone(),
                    rows: vec![row.clone()],
                };
                let normalized = sync_shape::normalize_json_columns(&[group], context)
                    .map_err(|e| e.to_string())?;
                let reshaped = sync_shape::reshape_table_groups(&normalized, context);
                let group = &reshaped[0];
                let object = group
                    .headers
                    .iter()
                    .cloned()
                    .zip(group.rows[0].iter().cloned())
                    .collect::<serde_json::Map<_, _>>();
                table.changes.push(Change {
                    op: "row".into(),
                    id: event[1].clone(),
                    row: Some(json!(object)),
                });
            }
        }
        page.tables.insert(name.into(), table);
    }
    Ok(page)
}
