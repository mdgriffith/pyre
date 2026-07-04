use crate::server::database_id::{self, DatabaseId};
use crate::server::query::QueryResult;
use crate::sync::{self, SyncCursor, SyncPageResult, TableSyncData};
use crate::sync_deltas::{self, AffectedRowTableGroup};
use crate::sync_shape;
use crate::typecheck;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;

pub type SyncSession = HashMap<String, sync::SessionValue>;
pub type ConnectedSessions = HashMap<String, SyncSession>;

pub const MAX_LIVE_SYNC_DELTA_ROWS: usize = 5000;
pub const MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES: usize = 1024 * 1024;
pub const MAX_LIVE_SYNC_FANOUT_RECIPIENTS: usize = 1000;

pub struct SyncServer<'a> {
    context: &'a typecheck::Context,
}

impl<'a> SyncServer<'a> {
    pub fn new(context: &'a typecheck::Context) -> Self {
        Self { context }
    }

    pub async fn catchup(
        &self,
        conn: &libsql::Connection,
        sync_cursor: &SyncCursor,
        session: &SyncSession,
        page_size: usize,
        database_id: impl AsRef<str>,
    ) -> Result<SyncPageResult, Error> {
        let result = catchup(conn, self.context, sync_cursor, session, page_size).await?;
        database_id::with_database_id(database_id, result).map_err(Error::DatabaseId)
    }

    pub async fn calculate_deltas(
        &self,
        conn: &libsql::Connection,
        query_result: &mut QueryResult,
        connected_sessions: &ConnectedSessions,
        database_id: impl AsRef<str>,
        origin_session_id: Option<&str>,
    ) -> Result<Vec<SessionDeltaMessage>, Error> {
        let database_id = database_id.as_ref();
        let broadcast_sessions = sessions_without_origin(connected_sessions, origin_session_id);
        let messages = build_delta_messages_for_database(
            self.context,
            &query_result.affected_rows,
            &broadcast_sessions,
            database_id,
        )?;
        let origin_message = build_origin_delta_message(
            self.context,
            &query_result.affected_rows,
            connected_sessions,
            database_id,
            origin_session_id,
        )?;
        stamp_messages_and_response_with_next_server_revision(
            conn,
            messages,
            query_result,
            origin_message,
        )
        .await
    }
}

fn sessions_without_origin(
    connected_sessions: &ConnectedSessions,
    origin_session_id: Option<&str>,
) -> ConnectedSessions {
    let Some(origin_session_id) = origin_session_id else {
        return connected_sessions.clone();
    };

    let mut sessions = connected_sessions.clone();
    sessions.remove(origin_session_id);
    sessions
}

fn build_origin_delta_message(
    context: &typecheck::Context,
    affected_row_groups: &[AffectedRowTableGroup],
    connected_sessions: &ConnectedSessions,
    database_id: &str,
    origin_session_id: Option<&str>,
) -> Result<Option<DeltaMessage>, Error> {
    let Some(origin_session_id) = origin_session_id else {
        return Ok(None);
    };
    let Some(origin_session) = connected_sessions.get(origin_session_id) else {
        return Ok(None);
    };

    let origin_sessions = HashMap::from([(origin_session_id.to_string(), origin_session.clone())]);
    let mut messages = build_delta_messages_for_database(
        context,
        affected_row_groups,
        &origin_sessions,
        database_id,
    )?;

    Ok(messages.pop().map(|message| message.message))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DeltaMessage {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(rename = "serverRevision", skip_serializing_if = "Option::is_none")]
    pub server_revision: Option<i64>,
    #[serde(rename = "databaseId", skip_serializing_if = "Option::is_none")]
    pub database_id: Option<DatabaseId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<AffectedRowTableGroup>,
}

impl DeltaMessage {
    pub fn delta(data: Vec<AffectedRowTableGroup>) -> Self {
        Self {
            type_: "delta".to_string(),
            server_revision: None,
            database_id: None,
            data,
        }
    }

    pub fn delta_for_database(
        database_id: impl AsRef<str>,
        data: Vec<AffectedRowTableGroup>,
    ) -> Result<Self, Error> {
        Ok(Self {
            type_: "delta".to_string(),
            server_revision: None,
            database_id: Some(
                database_id::require_database_id(database_id).map_err(Error::DatabaseId)?,
            ),
            data,
        })
    }

    pub fn sync_required() -> Self {
        Self {
            type_: "syncRequired".to_string(),
            server_revision: None,
            database_id: None,
            data: Vec::new(),
        }
    }

    pub fn sync_required_for_database(database_id: impl AsRef<str>) -> Result<Self, Error> {
        Ok(Self {
            type_: "syncRequired".to_string(),
            server_revision: None,
            database_id: Some(
                database_id::require_database_id(database_id).map_err(Error::DatabaseId)?,
            ),
            data: Vec::new(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SessionDeltaMessage {
    pub session_id: String,
    pub message: DeltaMessage,
}

/// Run a catchup sync request using a client cursor and logical session values.
pub async fn catchup(
    conn: &libsql::Connection,
    context: &typecheck::Context,
    sync_cursor: &SyncCursor,
    session: &SyncSession,
    page_size: usize,
) -> Result<SyncPageResult, Error> {
    let page_size = sync::normalize_page_size(page_size).map_err(Error::Sync)?;

    let status_statement =
        sync::get_sync_status_statement(sync_cursor, context, session).map_err(Error::Sync)?;
    let status_rows = query_objects(conn, &status_statement.sql, &status_statement.params).await?;
    let sync_status = sync::parse_sync_status(sync_cursor, context, session, &status_rows)
        .map_err(Error::Sync)?;

    let sync_sql = sync::get_sync_sql(&sync_status, sync_cursor, context, session, page_size)
        .map_err(Error::Sync)?;

    let mut result = SyncPageResult {
        database_id: None,
        server_revision: sync_status.server_revision,
        tables: HashMap::new(),
        has_more: false,
    };

    for table_sql in sync_sql.tables {
        let updated_at_index = table_sql
            .headers
            .iter()
            .position(|header| header == "updatedAt");
        let mut table_rows = Vec::new();
        let mut max_updated_at = None;

        for (statement_index, statement) in table_sql.sql.iter().enumerate() {
            let params = table_sql
                .params
                .get(statement_index)
                .cloned()
                .unwrap_or_default();
            let rows = expand_sync_rows(
                query_objects(conn, statement, &params).await?,
                &table_sql.headers,
            )?;

            for mut row in rows {
                decode_json_columns(&mut row, &table_sql.json_columns)?;

                if updated_at_index.is_some() {
                    if let Some(updated_at) = row.get("updatedAt").and_then(json_to_i64) {
                        max_updated_at = Some(match max_updated_at {
                            Some(current) if current >= updated_at => current,
                            _ => updated_at,
                        });
                    }
                }

                table_rows.push(row);
            }
        }

        let has_more_for_table = table_rows.len() > page_size;
        if has_more_for_table {
            table_rows.truncate(page_size);
            result.has_more = true;

            if updated_at_index.is_some() {
                max_updated_at = table_rows
                    .last()
                    .and_then(|row| row.get("updatedAt"))
                    .and_then(json_to_i64);
            }
        }

        let raw_group = AffectedRowTableGroup {
            table_name: table_sql.table_name.clone(),
            headers: table_sql.headers.clone(),
            rows: table_rows
                .iter()
                .map(|row| {
                    table_sql
                        .headers
                        .iter()
                        .map(|header| row.get(header).cloned().unwrap_or(JsonValue::Null))
                        .collect()
                })
                .collect(),
        };

        let reshaped_group = sync_shape::reshape_table_groups(&[raw_group], context)
            .into_iter()
            .next();
        let rows = reshaped_group
            .map(|group| {
                group
                    .rows
                    .into_iter()
                    .map(|row| row_array_to_object(&group.headers, row))
                    .collect()
            })
            .unwrap_or_default();

        result.tables.insert(
            table_sql.table_name,
            TableSyncData {
                rows,
                permission_hash: table_sql.permission_hash,
                last_seen_updated_at: max_updated_at,
            },
        );
    }

    Ok(result)
}

async fn next_server_revision(conn: &libsql::Connection) -> Result<i64, Error> {
    let mut rows = conn
        .query(
            "UPDATE _pyre_sync SET value = value + 1 WHERE key = 'server_revision' RETURNING value",
            (),
        )
        .await
        .map_err(Error::Database)?;

    let Some(row) = rows.next().await.map_err(Error::Database)? else {
        return Err(Error::Sync(sync::SyncError::DatabaseError(
            "failed to allocate Pyre sync server revision".to_string(),
        )));
    };

    row.get::<i64>(0).map_err(Error::Database)
}

async fn stamp_messages_and_response_with_next_server_revision(
    conn: &libsql::Connection,
    mut messages: Vec<SessionDeltaMessage>,
    query_result: &mut QueryResult,
    mut origin_message: Option<DeltaMessage>,
) -> Result<Vec<SessionDeltaMessage>, Error> {
    if query_result.affected_rows.is_empty() {
        return Ok(messages);
    }

    let server_revision = next_server_revision(conn).await?;
    for message in &mut messages {
        message.message.server_revision = Some(server_revision);
    }
    if let Some(origin_message) = &mut origin_message {
        origin_message.server_revision = Some(server_revision);
    }

    let mut envelope = serde_json::Map::new();
    envelope.insert(
        "serverRevision".to_string(),
        JsonValue::from(server_revision),
    );
    if let Some(origin_message) = origin_message {
        envelope.insert(
            "sync".to_string(),
            serde_json::to_value(origin_message).map_err(Error::Json)?,
        );
    }
    envelope.insert("result".to_string(), query_result.response.clone());
    query_result.response = JsonValue::Object(envelope);

    Ok(messages)
}

fn build_delta_messages_for_database(
    context: &typecheck::Context,
    affected_row_groups: &[AffectedRowTableGroup],
    connected_sessions: &ConnectedSessions,
    database_id: impl AsRef<str>,
) -> Result<Vec<SessionDeltaMessage>, Error> {
    let database_id = database_id::require_database_id(database_id).map_err(Error::DatabaseId)?;
    build_delta_messages(
        context,
        affected_row_groups,
        connected_sessions,
        Some(database_id),
    )
}

fn build_delta_messages(
    context: &typecheck::Context,
    affected_row_groups: &[AffectedRowTableGroup],
    connected_sessions: &ConnectedSessions,
    database_id: Option<DatabaseId>,
) -> Result<Vec<SessionDeltaMessage>, Error> {
    if affected_row_groups.is_empty() || connected_sessions.is_empty() {
        return Ok(Vec::new());
    }

    let result =
        sync_deltas::calculate_sync_deltas(affected_row_groups, connected_sessions, context)
            .map_err(Error::SyncDeltas)?;
    let mut messages = Vec::new();

    for group in result.groups {
        let reshaped_table_groups = sync_shape::reshape_table_groups(&group.table_groups, context);
        let delta_message = match &database_id {
            Some(database_id) => {
                DeltaMessage::delta_for_database(database_id, reshaped_table_groups)?
            }
            None => DeltaMessage::delta(reshaped_table_groups),
        };
        let message = if live_sync_requires_catchup(&delta_message, group.session_ids.len())? {
            match &database_id {
                Some(database_id) => DeltaMessage::sync_required_for_database(database_id)?,
                None => DeltaMessage::sync_required(),
            }
        } else {
            delta_message
        };

        for session_id in group.session_ids {
            messages.push(SessionDeltaMessage {
                session_id,
                message: message.clone(),
            });
        }
    }

    messages.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    Ok(messages)
}

fn live_sync_requires_catchup(
    message: &DeltaMessage,
    recipient_count: usize,
) -> Result<bool, Error> {
    if count_rows(&message.data) > MAX_LIVE_SYNC_DELTA_ROWS {
        return Ok(true);
    }

    if recipient_count > MAX_LIVE_SYNC_FANOUT_RECIPIENTS {
        return Ok(true);
    }

    Ok(serde_json::to_vec(message).map_err(Error::Json)?.len() > MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES)
}

fn count_rows(table_groups: &[AffectedRowTableGroup]) -> usize {
    table_groups.iter().map(|group| group.rows.len()).sum()
}

async fn query_objects(
    conn: &libsql::Connection,
    sql: &str,
    params: &[sync::SessionValue],
) -> Result<Vec<HashMap<String, JsonValue>>, Error> {
    let values = params
        .iter()
        .cloned()
        .map(session_value_to_libsql)
        .collect::<Vec<_>>();
    let mut rows = if values.is_empty() {
        conn.query(sql, ()).await.map_err(Error::Database)?
    } else {
        conn.query(sql, libsql::params_from_iter(values))
            .await
            .map_err(Error::Database)?
    };
    let column_names = (0..rows.column_count())
        .map(|index| rows.column_name(index).unwrap_or("").to_string())
        .collect::<Vec<_>>();
    let mut result = Vec::new();

    while let Some(row) = rows.next().await.map_err(Error::Database)? {
        let mut object = HashMap::with_capacity(column_names.len());

        for (index, column_name) in column_names.iter().enumerate() {
            let value = row
                .get::<libsql::Value>(index as i32)
                .map_err(Error::Database)?;
            object.insert(column_name.clone(), libsql_value_to_json(value));
        }

        result.push(object);
    }

    Ok(result)
}

fn expand_sync_rows(
    rows: Vec<HashMap<String, JsonValue>>,
    headers: &[String],
) -> Result<Vec<HashMap<String, JsonValue>>, Error> {
    if rows.len() != 1 || !rows[0].contains_key(sync::SYNC_ROWS_JSON_COLUMN) {
        return Ok(rows);
    }

    let raw_rows = rows[0]
        .get(sync::SYNC_ROWS_JSON_COLUMN)
        .cloned()
        .unwrap_or(JsonValue::Null);
    let row_arrays = match raw_rows {
        JsonValue::String(raw) => serde_json::from_str::<JsonValue>(&raw).map_err(Error::Json)?,
        value => value,
    };

    let JsonValue::Array(row_arrays) = row_arrays else {
        return Ok(Vec::new());
    };

    Ok(row_arrays
        .into_iter()
        .filter_map(|row| match row {
            JsonValue::Array(values) => Some(
                headers
                    .iter()
                    .enumerate()
                    .map(|(index, header)| {
                        (
                            header.clone(),
                            values.get(index).cloned().unwrap_or(JsonValue::Null),
                        )
                    })
                    .collect(),
            ),
            _ => None,
        })
        .collect())
}

fn session_value_to_libsql(value: sync::SessionValue) -> libsql::Value {
    match value {
        sync::SessionValue::Null => libsql::Value::Null,
        sync::SessionValue::Integer(value) => libsql::Value::Integer(value),
        sync::SessionValue::Real(value) => libsql::Value::Real(value),
        sync::SessionValue::Text(value) => libsql::Value::Text(value),
        sync::SessionValue::Blob(value) => libsql::Value::Blob(value),
    }
}

fn libsql_value_to_json(value: libsql::Value) -> JsonValue {
    match value {
        libsql::Value::Null => JsonValue::Null,
        libsql::Value::Integer(value) => JsonValue::from(value),
        libsql::Value::Real(value) => JsonValue::from(value),
        libsql::Value::Text(value) => JsonValue::String(value),
        libsql::Value::Blob(value) => {
            JsonValue::Array(value.into_iter().map(JsonValue::from).collect())
        }
    }
}

fn decode_json_columns(
    row: &mut HashMap<String, JsonValue>,
    json_columns: &[String],
) -> Result<(), Error> {
    for column in json_columns {
        if let Some(value) = row.get(column).cloned() {
            row.insert(column.clone(), parse_json_column_value(value)?);
        }
    }

    Ok(())
}

fn parse_json_column_value(value: JsonValue) -> Result<JsonValue, Error> {
    match value {
        JsonValue::String(raw) => {
            let parsed = serde_json::from_str::<JsonValue>(&raw).map_err(Error::Json)?;
            Ok(try_parse_nested_json_container(parsed))
        }
        value => Ok(value),
    }
}

fn try_parse_nested_json_container(value: JsonValue) -> JsonValue {
    let JsonValue::String(raw) = value else {
        return value;
    };

    let trimmed = raw.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return JsonValue::String(raw);
    }

    serde_json::from_str::<JsonValue>(trimmed).unwrap_or(JsonValue::String(raw))
}

fn row_array_to_object(headers: &[String], row: Vec<JsonValue>) -> JsonValue {
    let mut object = serde_json::Map::with_capacity(headers.len());

    for (index, header) in headers.iter().enumerate() {
        object.insert(
            header.clone(),
            row.get(index).cloned().unwrap_or(JsonValue::Null),
        );
    }

    JsonValue::Object(object)
}

fn json_to_i64(value: &JsonValue) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().map(|value| value as i64))
}

#[derive(Debug)]
pub enum Error {
    Database(libsql::Error),
    DatabaseId(database_id::DatabaseIdError),
    InvalidPageSize,
    Json(serde_json::Error),
    Sync(sync::SyncError),
    SyncDeltas(sync_deltas::SyncDeltasError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Database(error) => write!(f, "database error: {}", error),
            Error::DatabaseId(error) => write!(f, "database id error: {}", error),
            Error::InvalidPageSize => write!(f, "page_size must be greater than zero"),
            Error::Json(error) => write!(f, "json error: {}", error),
            Error::Sync(sync::SyncError::DatabaseError(message)) => {
                write!(f, "sync database error: {}", message)
            }
            Error::Sync(sync::SyncError::SqlGenerationError(message)) => {
                write!(f, "sync sql generation error: {}", message)
            }
            Error::Sync(sync::SyncError::PermissionError(message)) => {
                write!(f, "sync permission error: {}", message)
            }
            Error::Sync(sync::SyncError::InvalidPageSize) => {
                write!(f, "page_size must be greater than zero")
            }
            Error::Sync(sync::SyncError::InvalidSyncCursor(message)) => {
                write!(f, "invalid sync cursor: {}", message)
            }
            Error::SyncDeltas(sync_deltas::SyncDeltasError::TableNotFound(table_name)) => {
                write!(f, "sync delta table not found: {}", table_name)
            }
            Error::SyncDeltas(sync_deltas::SyncDeltasError::InvalidRowData(message)) => {
                write!(f, "sync delta invalid row data: {}", message)
            }
        }
    }
}

impl std::error::Error for Error {}
