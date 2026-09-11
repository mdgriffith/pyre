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

/// Registered by the host after database authorization and authentication. Never
/// construct a recipient's fence from a mutation's origin envelope.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncFence {
    pub database_id: String,
    pub instance: String,
    pub auth_generation: u64,
    pub namespace: String,
    pub manifest: String,
    pub database_epoch: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplacementRequest {
    pub version: u32,
    #[serde(flatten)]
    pub fence: SyncFence,
    pub request_id: String,
    pub target: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Replacement {
    #[serde(flatten)]
    pub fence: SyncFence,
    pub request_id: String,
    pub target: i64,
    #[serde(rename = "serverRevision")]
    pub revision: i64,
    pub scope: &'static str,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub complete: bool,
    pub tables: HashMap<String, ReplacementTable>,
}

#[derive(Debug, Serialize)]
pub struct ReplacementTable {
    pub rows: Vec<JsonValue>,
}

#[derive(Debug, Serialize)]
pub struct SessionReplacementMessage {
    pub session_id: String,
    pub message: JsonValue,
}

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

    /// Route-independent, single-response replacement. The host supplies the
    /// authenticated binding, manifest/context pair and a dedicated connection.
    /// A stale epoch is an error, never an implicit adoption of a new lifetime.
    pub async fn replacement(
        &self,
        conn: &libsql::Connection,
        manifest: &crate::server::manifest::Manifest,
        binding: &crate::server::query::BatchBinding<'_>,
        request: &ReplacementRequest,
        session: &crate::server::manifest::PyreSession,
    ) -> Result<Replacement, Error> {
        let fence = &request.fence;
        if request.version != 1
            || manifest.version != 1
            || manifest.replacement_contracts.get(binding.namespace)
                != crate::generate::manifest::replacement_contract(self.context, binding.namespace)
                    .as_ref()
            || !manifest
                .replacement_contracts
                .contains_key(binding.namespace)
            || request.request_id.is_empty()
            || request.target < 0
            || fence.database_id != binding.database_id
            || fence.instance.is_empty()
            || fence.instance != binding.instance
            || fence.auth_generation != binding.auth_generation
            || fence.namespace != binding.namespace
            || !self.context.valid_namespaces.contains(&fence.namespace)
            || fence.manifest != binding.manifest
            || binding.manifest != manifest.fingerprint()
            || fence.database_epoch.is_empty()
        {
            return Err(Error::InvalidFence);
        }
        database_id::require_database_id(binding.database_id).map_err(Error::DatabaseId)?;
        let session = session
            .revalidate(&manifest.session_schema)
            .map_err(|_| Error::InvalidSession)?;
        let sql = sync::get_replacement_sql(self.context, session.logical(), binding.namespace)
            .map_err(Error::Sync)?;
        let tx = conn
            .transaction_with_behavior(libsql::TransactionBehavior::Deferred)
            .await
            .map_err(Error::Database)?;
        let execution = async {
            let mut databases = tx
                .query("PRAGMA database_list", ())
                .await
                .map_err(Error::Database)?;
            while let Some(database) = databases.next().await.map_err(Error::Database)? {
                let name = database.get::<String>(1).map_err(Error::Database)?;
                if name != "main" && name != "temp" {
                    return Err(Error::InvalidFence);
                }
            }
            drop(databases);
            let mut rows = tx
                .query(
                    "SELECT database_epoch, server_revision FROM _pyre_sync WHERE id = 1",
                    (),
                )
                .await
                .map_err(Error::Database)?;
            let row = rows
                .next()
                .await
                .map_err(Error::Database)?
                .ok_or(Error::InvalidFence)?;
            let epoch = row.get::<String>(0).map_err(Error::Database)?;
            let revision = row.get::<i64>(1).map_err(Error::Database)?;
            drop(rows);
            if epoch != fence.database_epoch {
                return Err(Error::InvalidFence);
            }
            if revision < request.target {
                return Err(Error::TargetNotReached);
            }
            let mut tables = HashMap::new();
            for table in sql.tables {
                let mut data = Vec::new();
                for (statement, params) in table.sql.iter().zip(&table.params) {
                    data.extend(expand_sync_rows(
                        query_objects(&tx, statement, params).await?,
                        &table.headers,
                    )?);
                }
                let group = AffectedRowTableGroup {
                    table_name: table.table_name.clone(),
                    headers: table.headers.clone(),
                    rows: data
                        .iter()
                        .map(|row| {
                            table
                                .headers
                                .iter()
                                .map(|header| row.get(header).cloned().unwrap_or(JsonValue::Null))
                                .collect()
                        })
                        .collect(),
                };
                let shaped =
                    sync::reshape_replacement_table(self.context, binding.namespace, &group)
                        .map_err(Error::Sync)?;
                let rows = shaped
                    .rows
                    .into_iter()
                    .map(|row| row_array_to_object(&shaped.headers, row))
                    .collect();
                tables.insert(table.table_name, ReplacementTable { rows });
            }
            Ok(Replacement {
                fence: fence.clone(),
                request_id: request.request_id.clone(),
                target: request.target,
                revision,
                scope: "database",
                kind: "replacement",
                complete: true,
                tables,
            })
        }
        .await;
        match execution {
            Ok(result) => {
                tx.commit().await.map_err(Error::Database)?;
                Ok(result)
            }
            Err(error) => {
                let _ = tx.rollback().await;
                Err(error)
            }
        }
    }

    /// Postcommit hints carry no row data or origin request identity. Repeated or
    /// coalesced delivery only raises the recipient's required/security revision.
    pub fn replacement_messages(
        &self,
        result: &crate::server::query::BatchResult,
        recipients: &HashMap<String, SyncFence>,
    ) -> Vec<SessionReplacementMessage> {
        let response = &result.response;
        let Some(revision) = response
            .get("commitRevision")
            .and_then(JsonValue::as_i64)
            .filter(|r| *r >= 0)
        else {
            return Vec::new();
        };
        if response["status"] != "accepted" {
            return Vec::new();
        }
        let (Some(database_id), Some(namespace), Some(manifest), Some(database_epoch)) = (
            response["databaseId"].as_str(),
            response["namespace"].as_str(),
            response["manifest"].as_str(),
            response["databaseEpoch"].as_str(),
        ) else {
            return Vec::new();
        };
        self.committed_replacement_messages(
            &crate::server::query::CommittedRevision {
                database_epoch: database_epoch.into(),
                revision,
            },
            database_id,
            namespace,
            manifest,
            recipients,
        )
    }

    pub fn committed_replacement_messages(
        &self,
        commit: &crate::server::query::CommittedRevision,
        database_id: &str,
        namespace: &str,
        manifest: &str,
        recipients: &HashMap<String, SyncFence>,
    ) -> Vec<SessionReplacementMessage> {
        let revision = commit.revision;
        let mut messages = Vec::new();
        for (session_id, fence) in recipients {
            if fence.instance.is_empty()
                || fence.database_epoch.is_empty()
                || database_id != fence.database_id
                || commit.database_epoch != fence.database_epoch
                || namespace != fence.namespace
                || manifest != fence.manifest
            {
                continue;
            }
            let mut message =
                serde_json::to_value(fence).expect("SyncFence serialization is infallible");
            message["type"] = JsonValue::from("syncRequired");
            message["serverRevision"] = JsonValue::from(revision);
            message["reconciliation"] = serde_json::json!({
                "kind": "replaceRequired", "atLeast": revision,
                "invalidate": true, "minimumSafeRevision": revision
            });
            messages.push(SessionReplacementMessage {
                session_id: session_id.clone(),
                message,
            });
        }
        messages.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        messages
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

    pub async fn catchup_protocol(
        &self,
        conn: &libsql::Connection,
        sync_cursor: &SyncCursor,
        session: &SyncSession,
        page_size: usize,
        database_id: impl AsRef<str>,
        client_database_epoch: Option<&str>,
    ) -> Result<CatchupResponse, Error> {
        let page = self
            .catchup(conn, sync_cursor, session, page_size, database_id)
            .await?;
        if client_database_epoch.is_some_and(|client_epoch| client_epoch != page.database_epoch) {
            return Ok(CatchupResponse::Reset(DatabaseReset {
                type_: "reset".to_string(),
                database_id: page.database_id.clone(),
                database_epoch: page.database_epoch,
                operation: "replace".to_string(),
                scope: "database".to_string(),
                reason: "database_epoch_changed".to_string(),
            }));
        }

        Ok(CatchupResponse::Page(page))
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

    /// Legacy delta wire shape, using execution's committed revision rather than
    /// allocating a second one during postcommit publication.
    pub fn calculate_committed_deltas(
        &self,
        query_result: &mut QueryResult,
        connected_sessions: &ConnectedSessions,
        database_id: &str,
        origin_session_id: Option<&str>,
        commit: &crate::server::query::CommittedRevision,
    ) -> Result<Vec<SessionDeltaMessage>, Error> {
        let messages = build_delta_messages_for_database(
            self.context,
            &query_result.affected_rows,
            &sessions_without_origin(connected_sessions, origin_session_id),
            database_id,
        )?;
        let origin_message = build_origin_delta_message(
            self.context,
            &query_result.affected_rows,
            connected_sessions,
            database_id,
            origin_session_id,
        )?;
        stamp_messages_and_response(
            messages,
            query_result,
            origin_message,
            &commit.database_epoch,
            commit.revision,
        )
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
    #[serde(rename = "databaseEpoch", skip_serializing_if = "Option::is_none")]
    pub database_epoch: Option<String>,
    #[serde(rename = "databaseId", skip_serializing_if = "Option::is_none")]
    pub database_id: Option<DatabaseId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciliation: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data: Vec<AffectedRowTableGroup>,
}

impl DeltaMessage {
    pub fn delta(data: Vec<AffectedRowTableGroup>) -> Self {
        Self {
            type_: "delta".to_string(),
            server_revision: None,
            database_epoch: None,
            database_id: None,
            reconciliation: None,
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
            database_epoch: None,
            database_id: Some(
                database_id::require_database_id(database_id).map_err(Error::DatabaseId)?,
            ),
            data,
            reconciliation: None,
        })
    }

    pub fn sync_required() -> Self {
        Self {
            type_: "syncRequired".to_string(),
            server_revision: None,
            database_epoch: None,
            database_id: None,
            reconciliation: None,
            data: Vec::new(),
        }
    }

    pub fn sync_required_for_database(database_id: impl AsRef<str>) -> Result<Self, Error> {
        Ok(Self {
            type_: "syncRequired".to_string(),
            server_revision: None,
            database_epoch: None,
            database_id: Some(
                database_id::require_database_id(database_id).map_err(Error::DatabaseId)?,
            ),
            data: Vec::new(),
            reconciliation: None,
        })
    }
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum CatchupResponse {
    Reset(DatabaseReset),
    Page(SyncPageResult),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DatabaseReset {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(rename = "databaseId", skip_serializing_if = "Option::is_none")]
    pub database_id: Option<DatabaseId>,
    #[serde(rename = "databaseEpoch")]
    pub database_epoch: String,
    pub operation: String,
    pub scope: String,
    pub reason: String,
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
        database_epoch: sync_status.database_epoch,
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

            for row in rows {
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
        }

        // The cursor is one ordered tuple; both components must come from the
        // same final retained row.
        let last_cursor_row = table_rows.last();
        if updated_at_index.is_some() {
            max_updated_at = last_cursor_row
                .and_then(|row| row.get("updatedAt"))
                .and_then(json_to_i64);
        }
        let last_seen_primary_key = last_cursor_row
            .and_then(|row| row.get(&table_sql.primary_key))
            .filter(|value| !value.is_null())
            .cloned();

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

        let normalized_groups =
            sync_shape::normalize_json_columns(&[raw_group], context).map_err(Error::SyncShape)?;
        let reshaped_group = sync_shape::reshape_table_groups(&normalized_groups, context)
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
                last_seen_primary_key,
            },
        );
    }

    Ok(result)
}

pub(crate) async fn next_server_revision(
    conn: &libsql::Connection,
) -> Result<(String, i64), Error> {
    let mut rows = conn
        .query(
            "UPDATE _pyre_sync SET server_revision = server_revision + 1 WHERE id = 1 RETURNING database_epoch, server_revision",
            (),
        )
        .await
        .map_err(Error::Database)?;

    let Some(row) = rows.next().await.map_err(Error::Database)? else {
        return Err(Error::Sync(sync::SyncError::DatabaseError(
            "failed to allocate Pyre sync server revision".to_string(),
        )));
    };

    Ok((
        row.get::<String>(0).map_err(Error::Database)?,
        row.get::<i64>(1).map_err(Error::Database)?,
    ))
}

/// Start a new database sync lifetime and reset its revision sequence.
pub async fn rotate_database_epoch(conn: &libsql::Connection) -> Result<String, Error> {
    let mut rows = conn
        .query(
            "UPDATE _pyre_sync SET database_epoch = lower(hex(randomblob(16))), server_revision = 0 WHERE id = 1 RETURNING database_epoch",
            (),
        )
        .await
        .map_err(Error::Database)?;
    let Some(row) = rows.next().await.map_err(Error::Database)? else {
        return Err(Error::Sync(sync::SyncError::DatabaseError(
            "failed to rotate Pyre database epoch".to_string(),
        )));
    };
    row.get::<String>(0).map_err(Error::Database)
}

async fn stamp_messages_and_response_with_next_server_revision(
    conn: &libsql::Connection,
    messages: Vec<SessionDeltaMessage>,
    query_result: &mut QueryResult,
    origin_message: Option<DeltaMessage>,
) -> Result<Vec<SessionDeltaMessage>, Error> {
    if query_result.affected_rows.is_empty() && messages.is_empty() && origin_message.is_none() {
        return Ok(messages);
    }

    let (database_epoch, server_revision) = next_server_revision(conn).await?;
    stamp_messages_and_response(
        messages,
        query_result,
        origin_message,
        &database_epoch,
        server_revision,
    )
}

fn stamp_messages_and_response(
    mut messages: Vec<SessionDeltaMessage>,
    query_result: &mut QueryResult,
    mut origin_message: Option<DeltaMessage>,
    database_epoch: &str,
    server_revision: i64,
) -> Result<Vec<SessionDeltaMessage>, Error> {
    if query_result.affected_rows.is_empty() && messages.is_empty() && origin_message.is_none() {
        return Ok(messages);
    }
    for message in &mut messages {
        message.message.server_revision = Some(server_revision);
        message.message.database_epoch = Some(database_epoch.to_string());
        if let Some(reconciliation) = &mut message.message.reconciliation {
            reconciliation["atLeast"] = JsonValue::from(server_revision);
            reconciliation["minimumSafeRevision"] = JsonValue::from(server_revision);
        }
    }
    if let Some(origin_message) = &mut origin_message {
        origin_message.server_revision = Some(server_revision);
        origin_message.database_epoch = Some(database_epoch.to_string());
        if let Some(reconciliation) = &mut origin_message.reconciliation {
            reconciliation["atLeast"] = JsonValue::from(server_revision);
            reconciliation["minimumSafeRevision"] = JsonValue::from(server_revision);
        }
    }

    let mut envelope = serde_json::Map::new();
    envelope.insert(
        "serverRevision".to_string(),
        JsonValue::from(server_revision),
    );
    envelope.insert("databaseEpoch".to_string(), JsonValue::from(database_epoch));
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
    if connected_sessions.is_empty() {
        return Ok(Vec::new());
    }

    if sync::requires_replacement(context) {
        let mut message = DeltaMessage::sync_required();
        message.database_id = database_id;
        message.reconciliation =
            Some(serde_json::json!({"kind":"replaceRequired", "invalidate":true}));
        let mut messages = connected_sessions
            .keys()
            .map(|session_id| SessionDeltaMessage {
                session_id: session_id.clone(),
                message: message.clone(),
            })
            .collect::<Vec<_>>();
        messages.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        return Ok(messages);
    }
    if affected_row_groups.is_empty() {
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
    InvalidFence,
    InvalidSession,
    TargetNotReached,
    Database(libsql::Error),
    DatabaseId(database_id::DatabaseIdError),
    InvalidPageSize,
    Json(serde_json::Error),
    Sync(sync::SyncError),
    SyncDeltas(sync_deltas::SyncDeltasError),
    SyncShape(sync_shape::SyncShapeError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidFence => write!(f, "InvalidFence"),
            Error::InvalidSession => write!(f, "InvalidSession"),
            Error::TargetNotReached => write!(f, "TargetNotReached"),
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
            Error::SyncShape(error) => write!(f, "sync shape error: {}", error),
        }
    }
}

impl std::error::Error for Error {}
