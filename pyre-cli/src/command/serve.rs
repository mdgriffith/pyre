use axum::body::HttpBody;
use axum::extract::{Path as AxumPath, Query, RawBody, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use pyre::server::manifest::{Manifest, PyreSession};
use pyre::server::query::{self, BatchBinding, BatchRequest, MAX_BATCH_PAYLOAD_BYTES};
use pyre::server::schema::{load_schema_from_database, LoadedSchema};
use pyre::server::sync::{ConnectedSessions, SyncServer};
use pyre::sync::SyncCursor;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use sha2::Sha256;
use std::collections::HashMap;
use std::convert::Infallible;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, Mutex};

use super::shared::Options;
use crate::db;

type HmacSha256 = Hmac<Sha256>;

const DEFAULT_SESSION_HEADER: &str = "x-pyre-session";

pub struct ServeOptions<'a> {
    pub database: &'a str,
    pub auth: &'a Option<String>,
    pub host: &'a str,
    pub port: u16,
    pub generated: &'a str,
    pub database_id: &'a str,
    pub session_header: &'a Option<String>,
    pub session_secret: &'a Option<String>,
    pub dev_session: &'a Option<String>,
    pub cors_origins: &'a Vec<String>,
    pub page_size: usize,
    pub allow_unsafe_dev_session: bool,
    pub allow_unsafe_unsigned_session: bool,
}

#[derive(Clone, Debug)]
enum SessionSource {
    Empty,
    Dev(JsonValue),
    Header {
        name: String,
        secret: Option<String>,
    },
}

struct AppState {
    db: libsql::Database,
    manifest: Manifest,
    loaded_schema: LoadedSchema,
    database_id: String,
    session_source: SessionSource,
    page_size: usize,
    connections: Mutex<HashMap<String, Connection>>,
    cors_origins: Vec<String>,
}

struct Connection {
    fence: Option<pyre::server::sync::SyncFence>,
    session: HashMap<String, pyre::sync::SessionValue>,
    sender: mpsc::UnboundedSender<JsonValue>,
}

#[derive(Deserialize)]
struct SyncRequest {
    #[serde(rename = "databaseId")]
    database_id: Option<String>,
    #[serde(rename = "databaseEpoch")]
    database_epoch: Option<String>,
    #[serde(rename = "syncCursor")]
    sync_cursor: SyncCursor,
}

#[derive(Deserialize)]
struct RequestQuery {
    #[serde(rename = "databaseId")]
    database_id: Option<String>,
    #[serde(rename = "connectionId")]
    connection_id: Option<String>,
    sync: Option<String>,
}

#[derive(Serialize)]
struct HealthResponse<'a> {
    ok: bool,
    #[serde(rename = "databaseId")]
    database_id: &'a str,
}

#[derive(Deserialize)]
struct SignedSessionPayload {
    session: JsonValue,
    exp: i64,
    // Ordinary routes accept legacy {session, exp}. Batch authentication additionally
    // requires this claim inside the HMAC-signed payload, not in application session data.
    #[serde(default, rename = "localEdit")]
    local_edit: Option<LocalEditBinding>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalEditBinding {
    instance: String,
    auth_generation: u64,
}

#[derive(Debug)]
enum ServeError {
    BadRequest(String),
    Unauthorized(String),
    Internal(String),
}

impl ServeError {
    fn status(&self) -> StatusCode {
        match self {
            ServeError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ServeError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            ServeError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(&self) -> &str {
        match self {
            ServeError::BadRequest(message)
            | ServeError::Unauthorized(message)
            | ServeError::Internal(message) => message,
        }
    }
}

impl IntoResponse for ServeError {
    fn into_response(self) -> Response {
        let status = self.status();
        (status, Json(json!({ "error": self.message() }))).into_response()
    }
}

pub async fn serve<'a>(_: &'a Options<'a>, options: ServeOptions<'a>) -> io::Result<()> {
    let host: IpAddr = options.host.parse().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid --host '{}': {}", options.host, error),
        )
    })?;
    let addr = SocketAddr::from((host, options.port));
    let loopback = host.is_loopback();

    let db = db::connect(&options.database.to_string(), options.auth)
        .await
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error.format_error()))?;
    let conn = db.connect().map_err(|error| {
        io::Error::new(io::ErrorKind::Other, format!("database error: {}", error))
    })?;
    let loaded_schema = load_schema_from_database(&conn)
        .await
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))?;
    let manifest_path = PathBuf::from(options.generated).join("manifest.json");
    let manifest = Manifest::load(&manifest_path).map_err(|error| {
        io::Error::new(
            io::ErrorKind::Other,
            format!(
                "failed to load {}: {}\nRun `pyre generate` and try again.",
                manifest_path.display(),
                error
            ),
        )
    })?;
    if db::is_remote(options.database)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.format_error()))?
    {
        pyre::server::query::validate_remote_manifest(&manifest).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cannot serve generated queries: {error}"),
            )
        })?;
    }

    let session_source = session_source(&manifest, &options, loopback)?;
    let state = Arc::new(AppState {
        db,
        manifest,
        loaded_schema,
        database_id: require_non_empty(options.database_id, "databaseId")?,
        session_source,
        page_size: options.page_size,
        connections: Mutex::new(HashMap::new()),
        cors_origins: options.cors_origins.clone(),
    });

    let app = Router::new()
        .route("/health", get(health).options(cors_preflight))
        .route("/sync", post(sync).options(cors_preflight))
        .route("/sync/events", get(sync_events).options(cors_preflight))
        .route(
            "/sync/replacement",
            post(replacement).options(cors_preflight),
        )
        .route(
            "/sync/replacement/events",
            post(replacement_events).options(cors_preflight),
        )
        .route("/db", post(run_batch).options(cors_preflight))
        .route("/db/:query_id", post(run_query).options(cors_preflight))
        .with_state(state);

    println!("Pyre server listening on http://{}", addr);
    println!("Database ID: {}", options.database_id);
    println!("SSE endpoint: http://{}/sync/events", addr);

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error))
}

fn session_source(
    manifest: &Manifest,
    options: &ServeOptions<'_>,
    loopback: bool,
) -> io::Result<SessionSource> {
    if let Some(raw_session) = options.dev_session {
        if !loopback && !options.allow_unsafe_dev_session {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--dev-session is only allowed on loopback bind addresses unless --allow-unsafe-dev-session is passed",
            ));
        }
        let session: JsonValue = serde_json::from_str(raw_session).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid --dev-session JSON: {}", error),
            )
        })?;
        PyreSession::new(session.clone(), &manifest.session_schema).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid --dev-session: {}", error),
            )
        })?;
        if !loopback {
            eprintln!("WARNING: using --dev-session on a non-loopback bind address.");
        }
        return Ok(SessionSource::Dev(session));
    }

    let explicit_header = options.session_header.as_ref();
    let secret = options.session_secret.clone();
    if explicit_header.is_some() || secret.is_some() {
        if !loopback && secret.is_none() && !options.allow_unsafe_unsigned_session {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsigned session headers are only allowed on loopback bind addresses unless --allow-unsafe-unsigned-session is passed",
            ));
        }
        if !loopback && secret.is_none() {
            eprintln!("WARNING: using unsigned session headers on a non-loopback bind address.");
        }
        return Ok(SessionSource::Header {
            name: explicit_header
                .cloned()
                .unwrap_or_else(|| DEFAULT_SESSION_HEADER.to_string()),
            secret,
        });
    }

    if manifest.session_schema.is_empty() {
        Ok(SessionSource::Empty)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "This Pyre schema requires session data.\n\nFor local development:\n  pyre serve {} --dev-session '{{...}}'\n\nFor production, run behind authenticated upstream infrastructure and pass:\n  --session-header {} --session-secret <secret>",
                options.database, DEFAULT_SESSION_HEADER
            ),
        ))
    }
}

async fn health(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    with_cors(
        &state,
        &headers,
        Json(HealthResponse {
            ok: true,
            database_id: &state.database_id,
        })
        .into_response(),
    )
}

async fn cors_preflight(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    with_cors(&state, &headers, StatusCode::NO_CONTENT.into_response())
}

async fn sync(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<SyncRequest>,
) -> Result<Response, ServeError> {
    ensure_database_id(&state, body.database_id.as_deref())?;
    let session = pyre_session_from_request(&state, &headers)?;
    let conn = state
        .db
        .connect()
        .map_err(|error| ServeError::Internal(format!("database error: {}", error)))?;
    let context = state
        .loaded_schema
        .context()
        .map_err(|error| ServeError::Internal(error.to_string()))?;
    let server = SyncServer::new(context);
    let result = server
        .catchup_protocol(
            &conn,
            &body.sync_cursor,
            session.logical(),
            state.page_size,
            &state.database_id,
            body.database_epoch.as_deref(),
        )
        .await
        .map_err(|error| ServeError::Internal(error.to_string()))?;

    Ok(with_cors(&state, &headers, Json(result).into_response()))
}

async fn sync_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<RequestQuery>,
) -> Result<Response, ServeError> {
    ensure_database_id(&state, query.database_id.as_deref())?;
    let session = pyre_session_from_request(&state, &headers)?;
    let conn = state
        .db
        .connect()
        .map_err(|error| ServeError::Internal(format!("database error: {}", error)))?;
    let mut epoch_rows = conn
        .query("SELECT database_epoch FROM _pyre_sync WHERE id = 1", ())
        .await
        .map_err(|error| ServeError::Internal(format!("database error: {}", error)))?;
    let database_epoch: String = epoch_rows
        .next()
        .await
        .map_err(|error| ServeError::Internal(format!("database error: {}", error)))?
        .ok_or_else(|| ServeError::Internal("missing _pyre_sync row".to_string()))?
        .get(0)
        .map_err(|error| ServeError::Internal(format!("database error: {}", error)))?;
    let session_id = new_connection_id();
    let (sender, mut receiver) = mpsc::unbounded_channel();
    state.connections.lock().await.insert(
        session_id.clone(),
        Connection {
            fence: None,
            session: session.logical().clone(),
            sender,
        },
    );

    let connected = json!({
        "type": "connected",
        "sessionId": session_id,
        "connectionId": session_id,
        "databaseId": state.database_id,
        "databaseEpoch": database_epoch,
    });
    let cleanup = ConnectionCleanup {
        state: Arc::clone(&state),
        session_id: session_id.clone(),
    };

    let stream = async_stream::stream! {
        let _cleanup = cleanup;
        yield Ok::<_, Infallible>(Event::default().json_data(connected).unwrap_or_else(|_| Event::default()));
        while let Some(message) = receiver.recv().await {
            yield Ok::<_, Infallible>(Event::default().json_data(message).unwrap_or_else(|_| Event::default()));
        }
    };

    let response = Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    Ok(with_cors(&state, &headers, response))
}

async fn run_batch(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    RawBody(mut body): RawBody,
) -> Response {
    // Bound actual streamed bytes, not Content-Length or reserialized JSON.
    let mut bytes = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(_) => {
                return with_cors(
                    &state,
                    &headers,
                    batch_failure(None, StatusCode::BAD_REQUEST, "InvalidRequest", None),
                )
            }
        };
        if chunk.len() > MAX_BATCH_PAYLOAD_BYTES - bytes.len() {
            return with_cors(
                &state,
                &headers,
                batch_failure(None, StatusCode::PAYLOAD_TOO_LARGE, "InvalidRequest", None),
            );
        }
        bytes.extend_from_slice(&chunk);
    }
    let request: BatchRequest = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(_) => {
            return with_cors(
                &state,
                &headers,
                batch_failure(None, StatusCode::BAD_REQUEST, "InvalidRequest", None),
            )
        }
    };
    let result = async {
        let (session, local_edit) = effective_session_from_request(&state, &headers)
            .map_err(|_| (StatusCode::UNAUTHORIZED, "InvalidSession", None))?;
        let signed = matches!(
            &state.session_source,
            SessionSource::Header {
                secret: Some(_),
                ..
            }
        );
        // Empty, fixed dev, and explicitly enabled unsigned sessions are development
        // bindings: generation is always zero; instance is correlation, not authority.
        let (instance, auth_generation) = match local_edit.as_ref() {
            Some(binding) => (binding.instance.as_str(), binding.auth_generation),
            None if !signed => (request.instance.as_str(), 0),
            None => return Err((StatusCode::UNAUTHORIZED, "InvalidSession", None)),
        };
        let context = state
            .loaded_schema
            .context()
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "TransactionFailed", None))?;
        let schema = state
            .loaded_schema
            .schema()
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "TransactionFailed", None))?;
        if !context.valid_namespaces.contains(&schema.namespace) {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "TransactionFailed", None));
        }
        let fingerprint = state.manifest.fingerprint();
        let binding = BatchBinding {
            database_id: &state.database_id,
            namespace: &schema.namespace,
            manifest: &fingerprint,
            instance,
            auth_generation,
        };
        let conn = state
            .db
            .connect()
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "TransactionFailed", None))?;
        // Also fence empty batches; the executor rechecks nonempty batches in its transaction.
        let epoch = async {
            let mut rows = conn
                .query("SELECT database_epoch FROM _pyre_sync WHERE id = 1", ())
                .await?;
            rows.next()
                .await?
                .map(|row| row.get::<String>(0))
                .transpose()
        }
        .await
        .map_err(|_: libsql::Error| {
            (StatusCode::INTERNAL_SERVER_ERROR, "TransactionFailed", None)
        })?;
        if epoch.as_deref() != Some(request.database_epoch.as_str()) {
            return Err((StatusCode::BAD_REQUEST, "InvalidRequest", None));
        }
        query::run_batch(&conn, &state.manifest, &binding, &request, &session)
            .await
            .map_err(|error| {
                (
                    if error.code() == "OutcomeUnknown" {
                        StatusCode::INTERNAL_SERVER_ERROR
                    } else {
                        StatusCode::BAD_REQUEST
                    },
                    error.code(),
                    error.operation_index(),
                )
            })
    }
    .await;
    let response = match result {
        Ok(result) => {
            if let Ok(context) = state.loaded_schema.context() {
                let connections = state.connections.lock().await;
                let recipients = connections
                    .iter()
                    .filter_map(|(id, connection)| {
                        connection.fence.clone().map(|fence| (id.clone(), fence))
                    })
                    .collect();
                for hint in SyncServer::new(context).replacement_messages(&result, &recipients) {
                    if let Some(connection) = connections.get(&hint.session_id) {
                        let _ = connection.sender.send(hint.message);
                    }
                }
            }
            Json(result.response).into_response()
        }
        Err((status, code, index)) => batch_failure(Some(&request), status, code, index),
    };
    with_cors(&state, &headers, response)
}

async fn materialize_replacement(
    state: &AppState,
    headers: &HeaderMap,
    request: &pyre::server::sync::ReplacementRequest,
) -> Result<pyre::server::sync::Replacement, ServeError> {
    let (session, local_edit) = effective_session_from_request(state, headers)
        .map_err(|_| ServeError::Unauthorized("InvalidSession".into()))?;
    let signed = matches!(
        &state.session_source,
        SessionSource::Header {
            secret: Some(_),
            ..
        }
    );
    let (instance, auth_generation) = match local_edit.as_ref() {
        Some(binding) => (binding.instance.as_str(), binding.auth_generation),
        None if !signed => (request.fence.instance.as_str(), 0),
        None => return Err(ServeError::Unauthorized("InvalidSession".into())),
    };
    let context = state
        .loaded_schema
        .context()
        .map_err(|_| ServeError::Internal("ReplacementFailed".into()))?;
    let schema = state
        .loaded_schema
        .schema()
        .map_err(|_| ServeError::Internal("ReplacementFailed".into()))?;
    let fingerprint = state.manifest.fingerprint();
    let binding = BatchBinding {
        database_id: &state.database_id,
        namespace: &schema.namespace,
        manifest: &fingerprint,
        instance,
        auth_generation,
    };
    let conn = state
        .db
        .connect()
        .map_err(|_| ServeError::Internal("ReplacementFailed".into()))?;
    SyncServer::new(context)
        .replacement(&conn, &state.manifest, &binding, request, &session)
        .await
        .map_err(|error| match error {
            pyre::server::sync::Error::InvalidFence => {
                ServeError::BadRequest("InvalidFence".into())
            }
            pyre::server::sync::Error::InvalidSession => {
                ServeError::Unauthorized("InvalidSession".into())
            }
            pyre::server::sync::Error::TargetNotReached => {
                ServeError::BadRequest("TargetNotReached".into())
            }
            _ => ServeError::Internal("ReplacementFailed".into()),
        })
}

async fn replacement(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<pyre::server::sync::ReplacementRequest>,
) -> Response {
    let response = match materialize_replacement(&state, &headers, &request).await {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(error) => replacement_failure(&request, error),
    };
    with_cors(&state, &headers, response)
}

fn replacement_failure(
    request: &pyre::server::sync::ReplacementRequest,
    error: ServeError,
) -> Response {
    let mut body = serde_json::to_value(&request.fence).expect("serializable fence");
    body["requestId"] = json!(request.request_id);
    body["target"] = json!(request.target);
    body["error"] = json!(error.message());
    (error.status(), Json(body)).into_response()
}

async fn replacement_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<pyre::server::sync::ReplacementRequest>,
) -> Response {
    let snapshot = match materialize_replacement(&state, &headers, &request).await {
        Ok(snapshot) => snapshot,
        Err(error) => return with_cors(&state, &headers, replacement_failure(&request, error)),
    };
    let session_id = new_connection_id();
    let (sender, mut receiver) = mpsc::unbounded_channel();
    state.connections.lock().await.insert(
        session_id.clone(),
        Connection {
            fence: Some(snapshot.fence.clone()),
            session: HashMap::new(),
            sender,
        },
    );
    // Send a hint, not the pre-registration snapshot: the client fetches after
    // registration, covering any commit between validation and registration.
    let mut connected = serde_json::to_value(&snapshot.fence).expect("serializable fence");
    connected["type"] = json!("syncRequired");
    connected["serverRevision"] = json!(snapshot.revision);
    connected["reconciliation"] = json!({
        "kind": "replaceRequired", "atLeast": snapshot.revision,
        "invalidate": true, "minimumSafeRevision": snapshot.revision
    });
    let cleanup = ConnectionCleanup {
        state: Arc::clone(&state),
        session_id,
    };
    let stream = async_stream::stream! {
        let _cleanup = cleanup;
        yield Ok::<_, Infallible>(Event::default().json_data(connected).unwrap_or_else(|_| Event::default()));
        while let Some(message) = receiver.recv().await {
            yield Ok::<_, Infallible>(Event::default().json_data(message).unwrap_or_else(|_| Event::default()));
        }
    };
    with_cors(
        &state,
        &headers,
        Sse::new(stream)
            .keep_alive(KeepAlive::default())
            .into_response(),
    )
}

fn batch_failure(
    request: Option<&BatchRequest>,
    status: StatusCode,
    code: &str,
    index: Option<usize>,
) -> Response {
    let mut body = json!({
        "status": if code == "OutcomeUnknown" { "outcomeUnknown" } else { "rejected" },
        "code": code,
    });
    if let Some(request) = request {
        body["requestId"] = json!(request.request_id);
        body["databaseId"] = json!(request.database_id);
        body["instance"] = json!(request.instance);
        body["authGeneration"] = json!(request.auth_generation);
        body["databaseEpoch"] = json!(request.database_epoch);
        body["namespace"] = json!(request.namespace);
        body["manifest"] = json!(request.manifest);
    }
    if let Some(index) = index {
        body["operationIndex"] = json!(index);
    }
    (status, Json(body)).into_response()
}

async fn run_query(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<RequestQuery>,
    AxumPath(query_id): AxumPath<String>,
    Json(input): Json<JsonValue>,
) -> Result<Response, ServeError> {
    ensure_database_id(&state, query.database_id.as_deref())?;
    let session = pyre_session_from_request(&state, &headers)?;
    let conn = state
        .db
        .connect()
        .map_err(|error| ServeError::Internal(format!("database error: {}", error)))?;
    let (mut result, commit) = pyre::server::query::run_with_revision(
        &conn,
        &state.manifest,
        &query_id,
        input,
        &session,
        query.sync.as_deref() == Some("true"),
    )
    .await
    .map_err(|error| ServeError::BadRequest(error.to_string()))?;

    if let Some(commit) = commit {
        let context = state
            .loaded_schema
            .context()
            .map_err(|error| ServeError::Internal(error.to_string()))?;
        let server = SyncServer::new(context);
        let fingerprint = state.manifest.fingerprint();
        {
            let connections = state.connections.lock().await;
            let recipients = connections
                .iter()
                .filter_map(|(id, connection)| {
                    connection.fence.clone().map(|fence| (id.clone(), fence))
                })
                .collect();
            for hint in server.committed_replacement_messages(
                &commit,
                &state.database_id,
                &state.manifest.queries[&query_id].primary_db,
                &fingerprint,
                &recipients,
            ) {
                if let Some(connection) = connections.get(&hint.session_id) {
                    let _ = connection.sender.send(hint.message);
                }
            }
        }
        if query.sync.as_deref() == Some("true") {
            let connected_sessions = connected_sessions(&state).await;
            let messages = server
                .calculate_committed_deltas(
                    &mut result,
                    &connected_sessions,
                    &state.database_id,
                    query.connection_id.as_deref(),
                    &commit,
                )
                .map_err(|error| ServeError::Internal(error.to_string()))?;
            send_messages(&state, messages).await;
        }
    }

    Ok(with_cors(
        &state,
        &headers,
        Json(result.response).into_response(),
    ))
}

async fn connected_sessions(state: &AppState) -> ConnectedSessions {
    state
        .connections
        .lock()
        .await
        .iter()
        .filter(|(_, connection)| connection.fence.is_none())
        .map(|(id, connection)| (id.clone(), connection.session.clone()))
        .collect()
}

async fn send_messages(state: &AppState, messages: Vec<pyre::server::sync::SessionDeltaMessage>) {
    let connections = state.connections.lock().await;
    for message in messages {
        if let Some(connection) = connections.get(&message.session_id) {
            if let Ok(value) = serde_json::to_value(message.message) {
                let _ = connection.sender.send(value);
            }
        }
    }
}

struct ConnectionCleanup {
    state: Arc<AppState>,
    session_id: String,
}

impl Drop for ConnectionCleanup {
    fn drop(&mut self) {
        let state = Arc::clone(&self.state);
        let session_id = self.session_id.clone();
        tokio::spawn(async move {
            state.connections.lock().await.remove(&session_id);
        });
    }
}

fn pyre_session_from_request(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<PyreSession, ServeError> {
    effective_session_from_request(state, headers).map(|(session, _)| session)
}

fn effective_session_from_request(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(PyreSession, Option<LocalEditBinding>), ServeError> {
    let (value, binding) = match &state.session_source {
        SessionSource::Empty => (json!({}), None),
        SessionSource::Dev(value) => (value.clone(), None),
        SessionSource::Header { name, secret } => {
            let raw = headers
                .get(name)
                .ok_or_else(|| ServeError::Unauthorized(format!("missing {} header", name)))?
                .to_str()
                .map_err(|_| ServeError::Unauthorized(format!("invalid {} header", name)))?;
            if let Some(secret) = secret {
                let payload = decode_signed_session(raw, secret)?;
                (payload.session, payload.local_edit)
            } else {
                (decode_unsigned_session(raw)?, None)
            }
        }
    };

    PyreSession::new(value, &state.manifest.session_schema)
        .map(|session| (session, binding))
        .map_err(|error| ServeError::Unauthorized(format!("invalid Pyre session: {}", error)))
}

fn decode_unsigned_session(raw: &str) -> Result<JsonValue, ServeError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| ServeError::Unauthorized("invalid session header encoding".to_string()))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ServeError::Unauthorized("invalid session header JSON".to_string()))
}

fn decode_signed_session(raw: &str, secret: &str) -> Result<SignedSessionPayload, ServeError> {
    let Some((payload, signature)) = raw.split_once('.') else {
        return Err(ServeError::Unauthorized(
            "signed session header must contain payload and signature".to_string(),
        ));
    };
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| ServeError::Unauthorized("invalid session signature encoding".to_string()))?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| ServeError::Internal("invalid session secret".to_string()))?;
    mac.update(payload.as_bytes());
    mac.verify_slice(&signature)
        .map_err(|_| ServeError::Unauthorized("invalid session signature".to_string()))?;

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| ServeError::Unauthorized("invalid session payload encoding".to_string()))?;
    let payload: SignedSessionPayload = serde_json::from_slice(&payload_bytes)
        .map_err(|_| ServeError::Unauthorized("invalid session payload JSON".to_string()))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ServeError::Internal("system clock is before unix epoch".to_string()))?
        .as_secs() as i64;
    if payload.exp <= now {
        return Err(ServeError::Unauthorized(
            "session header is expired".to_string(),
        ));
    }
    Ok(payload)
}

fn ensure_database_id(state: &AppState, value: Option<&str>) -> Result<(), ServeError> {
    if let Some(value) = value {
        if value != state.database_id {
            return Err(ServeError::BadRequest(format!(
                "databaseId '{}' does not match this server's databaseId '{}'",
                value, state.database_id
            )));
        }
    }
    Ok(())
}

fn require_non_empty(value: &str, label: &str) -> io::Result<String> {
    if value.trim().is_empty() {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is required", label),
        ))
    } else {
        Ok(value.to_string())
    }
}

fn new_connection_id() -> String {
    static NEXT_CONNECTION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = NEXT_CONNECTION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("conn_{}", id)
}

fn allowed_cors_origin<'a>(state: &'a AppState, headers: &HeaderMap) -> Option<&'a str> {
    allowed_cors_origin_for(&state.cors_origins, headers)
}

fn allowed_cors_origin_for<'a>(cors_origins: &'a [String], headers: &HeaderMap) -> Option<&'a str> {
    if cors_origins.is_empty() {
        return None;
    }
    let request_origin = headers.get(header::ORIGIN)?.to_str().ok()?;

    cors_origins
        .iter()
        .find(|origin| origin.as_str() == request_origin)
        .map(String::as_str)
}

fn with_cors(state: &AppState, request_headers: &HeaderMap, mut response: Response) -> Response {
    let Some(origin) = allowed_cors_origin(state, request_headers) else {
        return response;
    };

    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&origin) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
    }
    headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    let session_header = match &state.session_source {
        SessionSource::Header { name, .. } => name.as_str(),
        _ => DEFAULT_SESSION_HEADER,
    };
    if let Ok(value) = HeaderValue::from_str(&format!("content-type, {session_header}")) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use pyre::server::manifest::FieldSchema;

    async fn batch_state() -> (tempfile::TempDir, Arc<AppState>) {
        let dir = tempfile::tempdir().unwrap();
        let db = libsql::Builder::new_local(dir.path().join("batch.db"))
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        pyre::server::schema::ensure_database(
            &conn,
            pyre::ast::DEFAULT_SCHEMANAME,
            "record Item {\n id Id.Int @id\n name String\n @public\n}\n",
        )
        .await
        .unwrap();
        let loaded_schema = load_schema_from_database(&conn).await.unwrap();
        let namespace = loaded_schema.schema().unwrap().namespace.clone();
        let manifest = serde_json::from_value(json!({
            "version": 1, "compiledContract": pyre::generate::manifest::compiled_schema_contract(loaded_schema.context().unwrap()), "replacementContracts": pyre::generate::manifest::replacement_contracts(loaded_schema.context().unwrap()), "session_schema": {}, "queries": {
                "create": {
                    "id": "create", "operation": "insert", "primary_db": namespace,
                    "input_schema": {}, "session_args": [], "optional_input_args": [], "json_input_args": [],
                    "sql": [{"include": false, "params": [], "sql": "INSERT INTO items (name) VALUES ('private value')"}]
                },
                "fail": {
                    "id": "fail", "operation": "insert", "primary_db": namespace,
                    "input_schema": {}, "session_args": [], "optional_input_args": [], "json_input_args": [],
                    "sql": [{"include": false, "params": [], "sql": "INSERT INTO private_missing_table VALUES ('secret')"}]
                }
            }
        })).unwrap();
        let state = Arc::new(AppState {
            db,
            manifest,
            loaded_schema,
            database_id: "server-db".into(),
            session_source: SessionSource::Empty,
            page_size: 100,
            connections: Mutex::new(HashMap::new()),
            cors_origins: vec!["http://localhost:5173".into()],
        });
        (dir, state)
    }

    async fn batch_body(state: &AppState) -> JsonValue {
        let conn = state.db.connect().unwrap();
        let mut rows = conn
            .query("SELECT database_epoch FROM _pyre_sync WHERE id = 1", ())
            .await
            .unwrap();
        let epoch: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        json!({"version": 1, "databaseId": state.database_id,
            "namespace": state.loaded_schema.schema().unwrap().namespace,
            "manifest": state.manifest.fingerprint(), "instance": "tab", "authGeneration": 0,
            "databaseEpoch": epoch, "requestId": "r1", "sequence": 1,
            "operations": [{"operation": "create", "input": {}}]})
    }

    fn origin_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:5173"),
        );
        headers
    }

    async fn response_json(mut response: Response) -> JsonValue {
        assert_eq!(
            response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "http://localhost:5173"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.body_mut().data().await {
            bytes.extend_from_slice(&chunk.unwrap());
        }
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn submit(state: Arc<AppState>, body: JsonValue, headers: HeaderMap) -> Response {
        run_batch(State(state), headers, RawBody(Body::from(body.to_string()))).await
    }

    #[tokio::test]
    async fn batch_route_accepts_and_rolls_back_without_private_errors_or_prefix() {
        let (_dir, state) = batch_state().await;
        let (sender, mut receiver) = mpsc::unbounded_channel();
        state.connections.lock().await.insert(
            "legacy".into(),
            Connection {
                fence: None,
                session: HashMap::new(),
                sender,
            },
        );
        let body = batch_body(&state).await;
        let response = submit(state.clone(), body.clone(), origin_headers()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let accepted = response_json(response).await;
        assert_eq!(accepted["status"], "accepted");
        assert_eq!(accepted["results"].as_array().unwrap().len(), 1);
        assert_eq!(accepted["reconciliation"]["kind"], "replaceRequired");
        let mut failed = body;
        failed["operations"]
            .as_array_mut()
            .unwrap()
            .push(json!({"operation": "fail", "input": {}}));
        let response = submit(state.clone(), failed, origin_headers()).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let rejected = response_json(response).await;
        assert_eq!(rejected["status"], "rejected");
        assert_eq!(rejected["code"], "TransactionFailed");
        assert_eq!(rejected["operationIndex"], 1);
        assert!(rejected.get("results").is_none());
        assert!(!rejected.to_string().contains("private"));
        assert!(!rejected.to_string().contains("secret"));
        let conn = state.db.connect().unwrap();
        let mut rows = conn.query("SELECT count(*) FROM items", ()).await.unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            1
        );
        let mut rows = conn
            .query("SELECT server_revision FROM _pyre_sync WHERE id = 1", ())
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            accepted["commitRevision"].as_i64().unwrap()
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn batch_route_uses_trusted_fences() {
        let (_dir, state) = batch_state().await;
        let body = batch_body(&state).await;
        for (key, value) in [
            ("databaseId", json!("other")),
            ("namespace", json!("other")),
            ("manifest", json!("other")),
            ("authGeneration", json!(8)),
            ("databaseEpoch", json!("other")),
        ] {
            let mut wrong = body.clone();
            wrong[key] = value;
            let response = submit(state.clone(), wrong, origin_headers()).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{key}");
            assert_eq!(response_json(response).await["code"], "InvalidRequest");
        }
    }

    #[tokio::test]
    async fn named_mutations_invalidate_replacement_recipients_at_the_committed_revision() {
        for sync_mode in [false, true] {
            let (_dir, mut state) = batch_state().await;
            let conn = state.db.connect().unwrap();
            pyre::server::schema::ensure_database(&conn, pyre::ast::DEFAULT_SCHEMANAME,
                "record Item {\n id Id.Int @id\n name String\n @allow(query) { name != \"hidden\" }\n @allow(insert, update, delete) { True }\n}\n").await.unwrap();
            let mutable = Arc::get_mut(&mut state).unwrap();
            mutable.loaded_schema = load_schema_from_database(&conn).await.unwrap();
            mutable.manifest.compiled_contract = pyre::generate::manifest::compiled_schema_contract(
                mutable.loaded_schema.context().unwrap(),
            );
            mutable.manifest.replacement_contracts = pyre::generate::manifest::replacement_contracts(mutable.loaded_schema.context().unwrap()).into_iter().collect();
            let namespace = mutable.loaded_schema.schema().unwrap().namespace.clone();
            for (id, operation, sql, affected) in [
                (
                    "delete",
                    "delete",
                    "DELETE FROM items WHERE id=1",
                    json!([{ "table_name":"items", "headers":["id","name"], "rows":[[1,"visible"]] }]),
                ),
                (
                    "hide",
                    "update",
                    "UPDATE items SET name='hidden' WHERE id=2",
                    json!([{ "table_name":"items", "headers":["id","name"], "rows":[[2,"hidden"]] }]),
                ),
                (
                    "noop",
                    "update",
                    "UPDATE items SET name='unused' WHERE id=999",
                    json!([]),
                ),
                ("read", "query", "SELECT 1", json!([])),
                (
                    "restore",
                    "update",
                    "UPDATE items SET name='visible' WHERE id=2",
                    json!([]),
                ),
            ] {
                mutable.manifest.queries.insert(id.into(), serde_json::from_value(json!({
                    "id":id,"operation":operation,"primary_db":namespace,
                    "input_schema":{},"session_args":[],"optional_input_args":[],"json_input_args":[],
                    "sql":[{"include":operation == "query","params":[],"sql":sql},
                        {"include":true,"params":[],"sql":format!("SELECT json('[]') AS item, '{}' AS _affectedRows", affected)}]
                })).unwrap());
            }
            conn.execute(
                "INSERT INTO items(id,name) VALUES(1,'visible'),(2,'visible')",
                (),
            )
            .await
            .unwrap();
            let body = batch_body(&state).await;
            let fence = pyre::server::sync::SyncFence {
                database_id: state.database_id.clone(),
                namespace,
                manifest: state.manifest.fingerprint(),
                database_epoch: body["databaseEpoch"].as_str().unwrap().into(),
                instance: "recipient".into(),
                auth_generation: 0,
            };
            let (sender, mut receiver) = mpsc::unbounded_channel();
            state.connections.lock().await.insert(
                "replacement".into(),
                Connection {
                    fence: Some(fence.clone()),
                    session: HashMap::new(),
                    sender,
                },
            );
            let (sender, mut legacy_receiver) = mpsc::unbounded_channel();
            state.connections.lock().await.insert(
                "legacy".into(),
                Connection {
                    fence: None,
                    session: HashMap::new(),
                    sender,
                },
            );
            for (index, name) in ["delete", "hide", "noop"].into_iter().enumerate() {
                let revision = index as i64 + 1;
                let response = run_query(
                    State(state.clone()),
                    origin_headers(),
                    Query(RequestQuery {
                        database_id: Some(state.database_id.clone()),
                        connection_id: None,
                        sync: sync_mode.then(|| "true".into()),
                    }),
                    AxumPath(name.into()),
                    Json(json!({})),
                )
                .await
                .unwrap();
                let response = response_json(response).await;
                if sync_mode && name != "noop" {
                    assert_eq!(response["result"], json!({"item":[]}));
                    assert_eq!(response["serverRevision"], revision);
                    if name == "delete" {
                        assert_eq!(
                            legacy_receiver.try_recv().unwrap()["serverRevision"],
                            revision
                        );
                    }
                } else {
                    assert_eq!(response, json!({"item":[]}));
                }
                let mut expected = serde_json::to_value(&fence).unwrap();
                expected["type"] = json!("syncRequired");
                expected["serverRevision"] = json!(revision);
                expected["reconciliation"] = json!({"kind":"replaceRequired","atLeast":revision,"invalidate":true,"minimumSafeRevision":revision});
                assert_eq!(receiver.try_recv().unwrap(), expected);
                let snapshot = materialize_replacement(
                    &state,
                    &origin_headers(),
                    &pyre::server::sync::ReplacementRequest {
                        version: 1,
                        fence: fence.clone(),
                        request_id: format!("catchup-{revision}"),
                        target: revision,
                    },
                )
                .await
                .unwrap();
                assert_eq!(snapshot.revision, revision);
                assert_eq!(
                    snapshot.tables["items"].rows.len(),
                    if name == "delete" { 1 } else { 0 }
                );
            }
            for name in ["read", "fail", "restore"] {
                if name == "restore" {
                    conn.execute("CREATE TRIGGER reject_revision BEFORE UPDATE ON _pyre_sync BEGIN SELECT RAISE(ABORT, 'revision unavailable'); END", ()).await.unwrap();
                }
                let response = run_query(
                    State(state.clone()),
                    origin_headers(),
                    Query(RequestQuery {
                        database_id: None,
                        connection_id: None,
                        sync: sync_mode.then(|| "true".into()),
                    }),
                    AxumPath(name.into()),
                    Json(json!({})),
                )
                .await;
                assert_eq!(response.is_ok(), name == "read", "{name}: {response:?}");
                assert!(receiver.try_recv().is_err());
            }
            let row = conn
                .query(
                    "SELECT server_revision, (SELECT name FROM items WHERE id=2) FROM _pyre_sync",
                    (),
                )
                .await
                .unwrap()
                .next()
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.get::<i64>(0).unwrap(), 3);
            assert_eq!(row.get::<String>(1).unwrap(), "hidden");
        }
    }

    #[tokio::test]
    async fn replacement_route_and_publication_use_recipient_fences() {
        let (_dir, state) = batch_state().await;
        let body = batch_body(&state).await;
        let request = pyre::server::sync::ReplacementRequest {
            version: 1,
            request_id: "catchup".into(),
            target: 0,
            fence: pyre::server::sync::SyncFence {
                database_id: body["databaseId"].as_str().unwrap().into(),
                namespace: body["namespace"].as_str().unwrap().into(),
                manifest: body["manifest"].as_str().unwrap().into(),
                database_epoch: body["databaseEpoch"].as_str().unwrap().into(),
                instance: "recipient".into(),
                auth_generation: 0,
            },
        };
        let response = replacement(
            State(state.clone()),
            origin_headers(),
            Json(request.clone()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let snapshot = response_json(response).await;
        assert_eq!(snapshot["requestId"], "catchup");
        assert_eq!(snapshot["instance"], "recipient");
        assert_eq!(snapshot["tables"]["items"], json!({"rows":[]}));
        let (sender, mut receiver) = mpsc::unbounded_channel();
        state.connections.lock().await.insert(
            "recipient".into(),
            Connection {
                fence: Some(request.fence.clone()),
                session: HashMap::new(),
                sender,
            },
        );
        let accepted = response_json(submit(state.clone(), body, origin_headers()).await).await;
        let hint = receiver.try_recv().unwrap();
        assert_eq!(hint["instance"], "recipient");
        assert_eq!(hint["type"], "syncRequired");
        assert_eq!(hint["serverRevision"], accepted["commitRevision"]);
        assert_eq!(
            hint["reconciliation"],
            json!({"kind":"replaceRequired", "atLeast":accepted["commitRevision"], "invalidate":true, "minimumSafeRevision":accepted["commitRevision"]})
        );
        assert!(hint.get("results").is_none());
        let mut catchup = request.clone();
        catchup.target = accepted["commitRevision"].as_i64().unwrap();
        let snapshot =
            response_json(replacement(State(state.clone()), origin_headers(), Json(catchup)).await)
                .await;
        assert_eq!(snapshot["type"], "replacement");
        assert_eq!(snapshot["serverRevision"], accepted["commitRevision"]);
        assert_eq!(
            snapshot["tables"]["items"]["rows"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let mut invalid = request.clone();
        invalid.fence.auth_generation = 123;
        let response = replacement(State(state.clone()), origin_headers(), Json(invalid)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let failure = response_json(response).await;
        assert_eq!(failure["requestId"], "catchup");
        assert_eq!(failure["error"], "InvalidFence");
        assert!(failure.get("tables").is_none());
        let mut future = request;
        future.target = 999;
        let response = replacement(State(state), origin_headers(), Json(future)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response_json(response).await["error"], "TargetNotReached");
    }

    #[tokio::test]
    async fn replacement_events_authenticate_registration_and_stream_fenced_hints() {
        let (_dir, mut state) = batch_state().await;
        Arc::get_mut(&mut state).unwrap().session_source = SessionSource::Header {
            name: DEFAULT_SESSION_HEADER.into(),
            secret: Some("secret".into()),
        };
        let mut body = batch_body(&state).await;
        let request = pyre::server::sync::ReplacementRequest {
            version: 1,
            request_id: "subscribe".into(),
            target: 0,
            fence: pyre::server::sync::SyncFence {
                database_id: body["databaseId"].as_str().unwrap().into(),
                namespace: body["namespace"].as_str().unwrap().into(),
                manifest: body["manifest"].as_str().unwrap().into(),
                database_epoch: body["databaseEpoch"].as_str().unwrap().into(),
                instance: "recipient".into(),
                auth_generation: 7,
            },
        };
        let signed_headers = |claims: JsonValue| {
            let payload = URL_SAFE_NO_PAD.encode(
                json!({"session":{}, "exp":4102444800_i64, "localEdit":claims}).to_string(),
            );
            let mut mac = HmacSha256::new_from_slice(b"secret").unwrap();
            mac.update(payload.as_bytes());
            let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
            let mut headers = origin_headers();
            headers.insert(
                DEFAULT_SESSION_HEADER,
                HeaderValue::from_str(&format!("{payload}.{signature}")).unwrap(),
            );
            headers
        };
        let recipient_headers = signed_headers(json!({"instance":"recipient", "authGeneration":7}));
        let mut wrong = request.clone();
        wrong.fence.auth_generation = 8;
        assert_eq!(
            replacement_events(State(state.clone()), recipient_headers.clone(), Json(wrong))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert!(state.connections.lock().await.is_empty());
        let response =
            replacement_events(State(state.clone()), recipient_headers, Json(request)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let mut stream = response.into_body();
        let first = stream.data().await.unwrap().unwrap();
        let first = std::str::from_utf8(&first).unwrap();
        assert!(first.contains("\"instance\":\"recipient\""));
        assert!(first.contains("\"authGeneration\":7"));
        assert!(first.contains("\"type\":\"syncRequired\""));
        assert!(first.contains("\"serverRevision\":0"));
        assert!(first.contains("\"reconciliation\":{\"atLeast\":0,\"invalidate\":true,\"kind\":\"replaceRequired\",\"minimumSafeRevision\":0}"));
        body["instance"] = json!("origin");
        body["authGeneration"] = json!(2);
        let accepted = response_json(
            submit(
                state.clone(),
                body,
                signed_headers(json!({"instance":"origin", "authGeneration":2})),
            )
            .await,
        )
        .await;
        assert_eq!(accepted["status"], "accepted");
        let next = stream.data().await.unwrap().unwrap();
        let next = std::str::from_utf8(&next).unwrap();
        assert!(next.contains("\"instance\":\"recipient\""));
        assert!(next.contains("\"authGeneration\":7"));
        assert!(next.contains("\"minimumSafeRevision\":1"));
        assert!(next.contains("\"serverRevision\":1"));
        assert!(next.contains("\"type\":\"syncRequired\""));
        assert!(!next.contains("origin"));
    }

    #[tokio::test]
    async fn batch_requires_complete_envelope_and_fences_empty_batches() {
        let (_dir, state) = batch_state().await;
        let mut body = batch_body(&state).await;
        for key in body.as_object().unwrap().keys() {
            let mut incomplete = body.clone();
            incomplete.as_object_mut().unwrap().remove(key);
            let response = submit(state.clone(), incomplete, origin_headers()).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{key}");
            assert_eq!(
                response_json(response).await,
                json!({"status": "rejected", "code": "InvalidRequest"})
            );
        }
        body["operations"] = json!([]);
        let response = submit(state.clone(), body.clone(), origin_headers()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let confirmed = response_json(response).await;
        assert_eq!(confirmed["status"], "confirmed");
        assert_eq!(confirmed["results"], json!([]));
        assert!(confirmed.get("commitRevision").is_none());
        body["databaseEpoch"] = json!("stale");
        let response = submit(state, body, origin_headers()).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response_json(response).await["code"], "InvalidRequest");
    }

    fn signed_header(payload: JsonValue) -> HeaderValue {
        let payload = URL_SAFE_NO_PAD.encode(payload.to_string());
        let mut mac = HmacSha256::new_from_slice(b"secret").unwrap();
        mac.update(payload.as_bytes());
        let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        HeaderValue::from_str(&format!("{payload}.{signature}")).unwrap()
    }

    #[tokio::test]
    async fn batch_signed_auth_requires_claim_and_never_uses_body_generation() {
        let (_dir, mut state) = batch_state().await;
        Arc::get_mut(&mut state).unwrap().session_source = SessionSource::Header {
            name: DEFAULT_SESSION_HEADER.into(),
            secret: Some("secret".into()),
        };
        let mut body = batch_body(&state).await;
        let mut headers = origin_headers();
        headers.insert(
            DEFAULT_SESSION_HEADER,
            signed_header(json!({"session": {}, "exp": 4102444800_i64})),
        );
        assert!(pyre_session_from_request(&state, &headers).is_ok());
        let response = submit(state.clone(), body.clone(), headers.clone()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response_json(response).await["code"], "InvalidSession");
        headers.insert(
            DEFAULT_SESSION_HEADER,
            signed_header(json!({"session": {}, "exp": 4102444800_i64,
            "localEdit": {"instance": "tab", "authGeneration": 7}})),
        );
        let response = submit(state.clone(), body.clone(), headers.clone()).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        body["authGeneration"] = json!(7);
        body["instance"] = json!("wrong-tab");
        assert_eq!(
            submit(state.clone(), body.clone(), headers.clone())
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        body["instance"] = json!("tab");
        let response = submit(state, body, headers).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_json(response).await["authGeneration"], 7);
    }

    #[tokio::test]
    async fn batch_rejects_tampered_signed_claims_with_sanitized_auth_error() {
        let (_dir, mut state) = batch_state().await;
        Arc::get_mut(&mut state).unwrap().session_source = SessionSource::Header {
            name: DEFAULT_SESSION_HEADER.into(),
            secret: Some("secret".into()),
        };
        let mut body = batch_body(&state).await;
        body["authGeneration"] = json!(9);
        let original = signed_header(json!({"session": {}, "exp": 4102444800_i64,
            "localEdit": {"instance": "tab", "authGeneration": 7}}));
        let (_, signature) = original.to_str().unwrap().split_once('.').unwrap();
        let forged = URL_SAFE_NO_PAD.encode(
            json!({"session": {}, "exp": 4102444800_i64,
            "localEdit": {"instance": "tab", "authGeneration": 9}})
            .to_string(),
        );
        let mut headers = origin_headers();
        headers.insert(
            DEFAULT_SESSION_HEADER,
            HeaderValue::from_str(&format!("{forged}.{signature}")).unwrap(),
        );
        let response = submit(state, body.clone(), headers).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let rejected = response_json(response).await;
        assert_eq!(rejected["status"], "rejected");
        assert_eq!(rejected["code"], "InvalidSession");
        assert!(rejected.get("results").is_none());
        assert!(rejected.get("error").is_none());
        for key in [
            "requestId",
            "databaseId",
            "instance",
            "authGeneration",
            "databaseEpoch",
            "namespace",
            "manifest",
        ] {
            assert_eq!(rejected[key], body[key], "{key}");
        }
    }

    #[tokio::test]
    async fn batch_raw_body_bound_and_malformed_errors_have_cors() {
        let (_dir, state) = batch_state().await;
        let mut valid = batch_body(&state).await.to_string().into_bytes();
        valid.resize(MAX_BATCH_PAYLOAD_BYTES, b' ');
        let response = run_batch(
            State(state.clone()),
            origin_headers(),
            RawBody(Body::from(valid)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_json(response).await["status"], "accepted");
        for (size, expected) in [
            (MAX_BATCH_PAYLOAD_BYTES, StatusCode::BAD_REQUEST),
            (MAX_BATCH_PAYLOAD_BYTES + 1, StatusCode::PAYLOAD_TOO_LARGE),
        ] {
            let response = run_batch(
                State(state.clone()),
                origin_headers(),
                RawBody(Body::from(vec![b' '; size])),
            )
            .await;
            assert_eq!(response.status(), expected);
            assert_eq!(
                response_json(response).await,
                json!({"status": "rejected", "code": "InvalidRequest"})
            );
        }
        let stream = async_stream::stream! {
            yield Ok::<_, Infallible>(vec![b' '; MAX_BATCH_PAYLOAD_BYTES]);
            yield Ok::<_, Infallible>(vec![b' '; 1]);
        };
        let response = run_batch(
            State(state),
            origin_headers(),
            RawBody(Body::wrap_stream(stream)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(response_json(response).await["code"], "InvalidRequest");
    }

    #[tokio::test]
    async fn batch_stream_failure_is_sanitized_and_custom_header_preflight_works() {
        let (_dir, mut state) = batch_state().await;
        Arc::get_mut(&mut state).unwrap().session_source = SessionSource::Header {
            name: "x-custom-session".into(),
            secret: Some("secret".into()),
        };
        let preflight = cors_preflight(State(state.clone()), origin_headers()).await;
        assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            preflight.headers()[header::ACCESS_CONTROL_ALLOW_HEADERS],
            "content-type, x-custom-session"
        );
        let stream = async_stream::stream! {
            yield Err::<Vec<u8>, _>(io::Error::new(io::ErrorKind::UnexpectedEof, "private transport detail"));
        };
        let response = run_batch(
            State(state.clone()),
            origin_headers(),
            RawBody(Body::wrap_stream(stream)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response_json(response).await,
            json!({"status": "rejected", "code": "InvalidRequest"})
        );
        let mut denied = HeaderMap::new();
        denied.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://evil.example"),
        );
        let response = run_batch(State(state), denied, RawBody(Body::from("invalid JSON"))).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());
    }

    #[tokio::test]
    async fn commit_failure_is_unknown_not_rejected() {
        let response = batch_failure(
            None,
            StatusCode::INTERNAL_SERVER_ERROR,
            query::Error::OutcomeUnknown.code(),
            None,
        );
        let (_dir, state) = batch_state().await;
        let value = response_json(with_cors(&state, &origin_headers(), response)).await;
        assert_eq!(
            value,
            json!({"status": "outcomeUnknown", "code": "OutcomeUnknown"})
        );
    }

    #[tokio::test]
    async fn dev_and_unsigned_batches_use_fixed_generation_zero() {
        for source in [
            SessionSource::Dev(json!({})),
            SessionSource::Header {
                name: DEFAULT_SESSION_HEADER.into(),
                secret: None,
            },
        ] {
            let (_dir, mut state) = batch_state().await;
            Arc::get_mut(&mut state).unwrap().session_source = source;
            let mut headers = origin_headers();
            headers.insert(
                DEFAULT_SESSION_HEADER,
                HeaderValue::from_str(&URL_SAFE_NO_PAD.encode("{}")).unwrap(),
            );
            let mut body = batch_body(&state).await;
            body["authGeneration"] = json!(7);
            assert_eq!(
                submit(state.clone(), body.clone(), headers.clone())
                    .await
                    .status(),
                StatusCode::BAD_REQUEST
            );
            body["authGeneration"] = json!(0);
            assert_eq!(submit(state, body, headers).await.status(), StatusCode::OK);
        }
    }

    fn manifest_with_session() -> Manifest {
        Manifest {
            replacement_contracts: Default::default(),
            compiled_contract: String::new(),
            version: 1,
            session_schema: HashMap::from([(
                "userId".to_string(),
                FieldSchema {
                    type_: "Int".to_string(),
                    is_enum: false,
                    enum_variants: Vec::new(),
                    tagged_union_variants: HashMap::new(),
                    tagged_union_types: HashMap::new(),
                    nullable: false,
                    omittable: false,
                },
            )]),
            queries: HashMap::new(),
        }
    }

    fn empty_auth() -> Option<String> {
        None
    }

    fn serve_options<'a>(
        auth: &'a Option<String>,
        session_header: &'a Option<String>,
        session_secret: &'a Option<String>,
        dev_session: &'a Option<String>,
        cors_origins: &'a Vec<String>,
    ) -> ServeOptions<'a> {
        ServeOptions {
            database: "db.sqlite",
            auth,
            host: "127.0.0.1",
            port: 3000,
            generated: "pyre/generated",
            database_id: "default",
            session_header,
            session_secret,
            dev_session,
            cors_origins,
            page_size: 1000,
            allow_unsafe_dev_session: false,
            allow_unsafe_unsigned_session: false,
        }
    }

    #[test]
    fn unsigned_session_header_contains_full_session_json() {
        let encoded = URL_SAFE_NO_PAD.encode(r#"{"userId":123}"#);
        let session = decode_unsigned_session(&encoded).expect("decoded session");

        assert_eq!(session["userId"], json!(123));
    }

    #[test]
    fn signed_session_header_verifies_signature_and_expiration() {
        let payload = URL_SAFE_NO_PAD.encode(r#"{"session":{"userId":123},"exp":4102444800}"#);
        let mut mac = HmacSha256::new_from_slice(b"secret").expect("hmac");
        mac.update(payload.as_bytes());
        let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        let raw = format!("{}.{}", payload, signature);

        let session = decode_signed_session(&raw, "secret").expect("decoded session");

        assert_eq!(session.session["userId"], json!(123));
        assert!(session.local_edit.is_none());
        assert!(decode_signed_session(&raw, "wrong-secret").is_err());
        let expired = signed_header(json!({"session": {"userId": 123}, "exp": 1}));
        assert!(decode_signed_session(expired.to_str().unwrap(), "secret").is_err());
    }

    #[test]
    fn non_loopback_dev_session_requires_explicit_unsafe_flag() {
        let auth = empty_auth();
        let session_header = None;
        let session_secret = None;
        let dev_session = Some(r#"{"userId":1}"#.to_string());
        let cors_origins = Vec::new();
        let options = serve_options(
            &auth,
            &session_header,
            &session_secret,
            &dev_session,
            &cors_origins,
        );

        let error = session_source(&manifest_with_session(), &options, false)
            .expect_err("expected unsafe dev session rejection");

        assert!(error.to_string().contains("--allow-unsafe-dev-session"));
    }

    #[test]
    fn schema_with_session_requires_session_source() {
        let auth = empty_auth();
        let session_header = None;
        let session_secret = None;
        let dev_session = None;
        let cors_origins = Vec::new();
        let options = serve_options(
            &auth,
            &session_header,
            &session_secret,
            &dev_session,
            &cors_origins,
        );

        let error = session_source(&manifest_with_session(), &options, true)
            .expect_err("expected missing session source rejection");

        assert!(error.to_string().contains("requires session data"));
    }

    #[test]
    fn cors_echoes_only_matching_request_origin() {
        let cors_origins = vec![
            "http://localhost:5173".to_string(),
            "http://localhost:3001".to_string(),
        ];
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:5173"),
        );

        assert_eq!(
            allowed_cors_origin_for(&cors_origins, &headers),
            Some("http://localhost:5173")
        );
    }

    #[test]
    fn cors_ignores_unlisted_request_origin() {
        let cors_origins = vec!["http://localhost:5173".to_string()];
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://evil.example"),
        );

        assert_eq!(allowed_cors_origin_for(&cors_origins, &headers), None);
    }
}
