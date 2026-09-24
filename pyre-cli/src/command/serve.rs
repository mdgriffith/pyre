use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use pyre::server::manifest::{Manifest, PyreSession};
use pyre::server::runtime::{
    DatabaseRuntime, Delivery, JoinEvidence, Participant, RuntimeConfig, RuntimeError,
    SharedWritePolicy, Subscription,
};
use pyre::server::schema::{load_schema_from_database, LoadedSchema};
use pyre::server::sync::{ConnectedSessions, SyncServer};
use pyre::sync::SyncCursor;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::convert::Infallible;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

use super::shared::Options;
use crate::db;

type HmacSha256 = Hmac<Sha256>;

const DEFAULT_SESSION_HEADER: &str = "x-pyre-session";
const DURABLE_CHANNEL_CAPACITY: usize = 64;
const EPHEMERAL_POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_CLIENT_REQUEST_SEQUENCE: u64 = 9_007_199_254_740_991;

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
    pub participant_shared_writes: bool,
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
    database: DatabaseOwner,
    manifest: Manifest,
    loaded_schema: LoadedSchema,
    database_id: String,
    session_source: SessionSource,
    page_size: usize,
    connections: StdMutex<HashMap<String, Connection>>,
    cors_origins: Vec<String>,
}

struct Connection {
    session: HashMap<String, pyre::sync::SessionValue>,
    sender: mpsc::Sender<JsonValue>,
    ephemeral: Option<EphemeralTransport>,
}

enum DatabaseOwner {
    Durable(libsql::Database),
    Ephemeral(DatabaseRuntime<libsql::Database>),
}

impl DatabaseOwner {
    fn database(&self) -> &libsql::Database {
        match self {
            Self::Durable(database) => database,
            Self::Ephemeral(runtime) => runtime.database(),
        }
    }

    fn ephemeral(&self) -> Option<&DatabaseRuntime<libsql::Database>> {
        match self {
            Self::Durable(_) => None,
            Self::Ephemeral(runtime) => Some(runtime),
        }
    }
}

#[derive(Clone)]
struct EphemeralTransport {
    participant: Option<Participant>,
    subscription: Arc<StdMutex<Subscription>>,
}

struct AuthenticatedSession {
    session: PyreSession,
    owner_id: String,
}

#[derive(Deserialize)]
struct SyncRequest {
    #[serde(rename = "databaseId")]
    database_id: Option<String>,
    #[serde(rename = "databaseEpoch")]
    database_epoch: Option<String>,
    #[serde(rename = "syncCursor")]
    sync_cursor: ClientSyncCursor,
}

#[derive(Deserialize)]
struct ClientSyncCursor {
    tables: SyncCursor,
}

#[derive(Deserialize)]
struct RequestQuery {
    #[serde(rename = "databaseId")]
    database_id: Option<String>,
    sync: Option<String>,
    #[serde(rename = "ephemeralWrite")]
    ephemeral_write: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EphemeralRequest {
    database_id: String,
    ephemeral_epoch: String,
    connection_id: String,
    #[serde(deserialize_with = "deserialize_client_request_sequence")]
    client_request_sequence: u64,
    #[serde(default)]
    patch: Option<JsonValue>,
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
    #[serde(rename = "sessionKey")]
    session_key: Option<String>,
}

#[derive(Debug)]
enum ServeError {
    BadRequest(String),
    Unauthorized(String),
    Forbidden(String),
    Conflict(String),
    PayloadTooLarge(String),
    Internal(String),
}

impl ServeError {
    fn status(&self) -> StatusCode {
        match self {
            ServeError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ServeError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            ServeError::Forbidden(_) => StatusCode::FORBIDDEN,
            ServeError::Conflict(_) => StatusCode::CONFLICT,
            ServeError::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            ServeError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(&self) -> &str {
        match self {
            ServeError::BadRequest(message)
            | ServeError::Unauthorized(message)
            | ServeError::Forbidden(message)
            | ServeError::Conflict(message)
            | ServeError::PayloadTooLarge(message)
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
    let database_id = require_non_empty(options.database_id, "databaseId")?;
    let database = match manifest.ephemeral.clone() {
        Some(contract) if contract.connection.is_some() || contract.shared.is_some() => {
            let mut config = RuntimeConfig::default();
            if options.participant_shared_writes {
                config.shared_write_policy = SharedWritePolicy::ParticipantWritable;
            }
            DatabaseOwner::Ephemeral(
                DatabaseRuntime::new(database_id.clone(), db, contract, config).map_err(
                    |error| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("invalid ephemeral runtime configuration: {error}"),
                        )
                    },
                )?,
            )
        }
        _ => DatabaseOwner::Durable(db),
    };
    let state = Arc::new(AppState {
        database,
        manifest,
        loaded_schema,
        database_id,
        session_source,
        page_size: options.page_size,
        connections: StdMutex::new(HashMap::new()),
        cors_origins: options.cors_origins.clone(),
    });

    let app = Router::new()
        .route("/health", get(health).options(cors_preflight))
        .route("/sync", post(sync).options(cors_preflight))
        .route("/sync/events", get(sync_events).options(cors_preflight))
        .route(
            "/ephemeral/connection",
            patch(patch_connection).options(cors_preflight),
        )
        .route(
            "/ephemeral/shared",
            patch(patch_shared).options(cors_preflight),
        )
        .route(
            "/ephemeral/lease",
            post(refresh_lease).options(cors_preflight),
        )
        .route(
            "/ephemeral/resnapshot",
            post(resnapshot).options(cors_preflight),
        )
        .route("/db/:query_id", post(run_query).options(cors_preflight))
        .with_state(Arc::clone(&state));

    println!("Pyre server listening on http://{}", addr);
    println!("Database ID: {}", options.database_id);
    println!("SSE endpoint: http://{}/sync/events", addr);

    let lease_sweeper = state.database.ephemeral().map(|_| {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(EPHEMERAL_POLL_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Some(runtime) = state.database.ephemeral() else {
                    break;
                };
                if runtime.expire_leases().is_err() {
                    break;
                }
            }
        })
    });
    let result = axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error));
    if let Some(sweeper) = lease_sweeper {
        sweeper.abort();
        let _ = sweeper.await;
    }
    if let Some(runtime) = state.database.ephemeral() {
        let _ = runtime.close();
    }
    result
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
    let authenticated = authenticated_session_from_request(&state, &headers)?;
    let conn = state
        .database
        .database()
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
            &body.sync_cursor.tables,
            authenticated.session.logical(),
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
    let authenticated = authenticated_session_from_request(&state, &headers)?;
    let conn = state
        .database
        .database()
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
    let (session_id, ephemeral, initial_ephemeral) =
        if let Some(runtime) = state.database.ephemeral() {
            let contract = state
                .manifest
                .ephemeral
                .as_ref()
                .expect("runtime requires an ephemeral contract");
            if contract.connection.is_some() {
                let joined = runtime
                    .join(JoinEvidence {
                        owner_id: authenticated.owner_id.clone(),
                        trusted_session: authenticated.session.json().clone(),
                        writable: query.ephemeral_write.unwrap_or(true),
                    })
                    .map_err(runtime_serve_error)?;
                let id = joined.participant.connection_id().to_string();
                let transport = EphemeralTransport {
                    participant: Some(joined.participant),
                    subscription: Arc::new(StdMutex::new(joined.subscription)),
                };
                (id, Some(transport), Some(joined.snapshot))
            } else {
                let subscribed = runtime
                    .subscribe(
                        &authenticated.owner_id,
                        query.ephemeral_write.unwrap_or(true),
                    )
                    .map_err(runtime_serve_error)?;
                let id = subscribed.subscription.connection_id().to_string();
                let transport = EphemeralTransport {
                    participant: None,
                    subscription: Arc::new(StdMutex::new(subscribed.subscription)),
                };
                (id, Some(transport), Some(subscribed.snapshot))
            }
        } else {
            (new_connection_id(), None, None)
        };
    let (sender, mut receiver) = mpsc::channel(DURABLE_CHANNEL_CAPACITY);
    state
        .connections
        .lock()
        .map_err(|_| ServeError::Internal("connection map is poisoned".to_string()))?
        .insert(
            session_id.clone(),
            Connection {
                session: authenticated.session.logical().clone(),
                sender,
                ephemeral: ephemeral.clone(),
            },
        );

    let connected = json!({
        "type": "connected",
        "sessionId": session_id,
        "connectionId": session_id,
        "databaseId": state.database_id,
        "databaseEpoch": database_epoch,
        "ephemeralEpoch": state.database.ephemeral().map(DatabaseRuntime::epoch),
    });
    let cleanup = ConnectionCleanup {
        state: Arc::clone(&state),
        session_id: session_id.clone(),
        ephemeral: ephemeral.clone(),
    };
    let stream_state = Arc::clone(&state);

    let stream = async_stream::stream! {
        let _cleanup = cleanup;
        yield Ok::<_, Infallible>(Event::default().json_data(connected).unwrap_or_else(|_| Event::default()));
        if let Some(snapshot) = initial_ephemeral {
            let delivery = Delivery::Snapshot { snapshot };
            yield Ok::<_, Infallible>(Event::default().json_data(delivery).unwrap_or_else(|_| Event::default()));
        }
        let mut interval = tokio::time::interval(EPHEMERAL_POLL_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                message = receiver.recv() => match message {
                    Some(message) => yield Ok::<_, Infallible>(Event::default().json_data(message).unwrap_or_else(|_| Event::default())),
                    None => break,
                },
                _ = interval.tick(), if ephemeral.is_some() => {
                    let Some(runtime) = stream_state.database.ephemeral() else { break };
                    let subscription = ephemeral
                        .as_ref()
                        .and_then(|transport| transport.subscription.lock().ok().map(|value| value.clone()));
                    let Some(subscription) = subscription else { break };
                    match runtime.poll(&subscription) {
                        Ok(Some(delivery)) => yield Ok::<_, Infallible>(Event::default().json_data(delivery).unwrap_or_else(|_| Event::default())),
                        Ok(None) => {}
                        Err(RuntimeError::StaleSubscription) => continue,
                        Err(_) => break,
                    }
                }
            }
        }
    };

    let response = Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    Ok(with_cors(&state, &headers, response))
}

async fn patch_connection(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<EphemeralRequest>,
) -> Response {
    let sequence = body.client_request_sequence;
    let result = (|| {
        let authenticated = authenticated_session_from_request(&state, &headers)?;
        let runtime = ephemeral_runtime(&state)?;
        validate_ephemeral_identity(&state, runtime, &body)?;
        let participant = participant_for_request(&state, &body.connection_id)?;
        let patch = body
            .patch
            .as_ref()
            .ok_or_else(|| ServeError::BadRequest("patch is required".to_string()))?;
        let change = runtime
            .patch_connection(&participant, &authenticated.owner_id, patch)
            .map_err(runtime_serve_error)?;
        let revision = change
            .as_ref()
            .map(|change| change.revision)
            .map(Ok)
            .unwrap_or_else(|| runtime.revision().map_err(runtime_serve_error))?;
        Ok(("connectionPatch", revision, None))
    })();
    ephemeral_response(&state, &headers, sequence, result)
}

async fn patch_shared(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<EphemeralRequest>,
) -> Response {
    let sequence = body.client_request_sequence;
    let result = (|| {
        let authenticated = authenticated_session_from_request(&state, &headers)?;
        let runtime = ephemeral_runtime(&state)?;
        validate_ephemeral_identity(&state, runtime, &body)?;
        let transport = transport_for_request(&state, &body.connection_id)?;
        let patch = body
            .patch
            .as_ref()
            .ok_or_else(|| ServeError::BadRequest("patch is required".to_string()))?;
        let change = if let Some(participant) = transport.participant {
            runtime.patch_shared_from_participant(&participant, &authenticated.owner_id, patch)
        } else {
            let subscription = transport
                .subscription
                .lock()
                .map_err(|_| ServeError::Internal("subscription lock is poisoned".to_string()))?
                .clone();
            runtime.patch_shared_from_subscription(&subscription, &authenticated.owner_id, patch)
        }
        .map_err(runtime_serve_error)?;
        let revision = change
            .as_ref()
            .map(|change| change.revision)
            .map(Ok)
            .unwrap_or_else(|| runtime.revision().map_err(runtime_serve_error))?;
        Ok(("sharedPatch", revision, None))
    })();
    ephemeral_response(&state, &headers, sequence, result)
}

async fn refresh_lease(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<EphemeralRequest>,
) -> Response {
    let sequence = body.client_request_sequence;
    let result = (|| {
        let authenticated = authenticated_session_from_request(&state, &headers)?;
        let runtime = ephemeral_runtime(&state)?;
        validate_ephemeral_identity(&state, runtime, &body)?;
        let transport = transport_for_request(&state, &body.connection_id)?;
        let (revision, lease) = if let Some(participant) = transport.participant {
            let (change, lease) = runtime
                .refresh_and_renew(
                    &participant,
                    &authenticated.owner_id,
                    authenticated.session.json(),
                )
                .map_err(runtime_serve_error)?;
            let revision = change
                .as_ref()
                .map(|change| change.revision)
                .map(Ok)
                .unwrap_or_else(|| runtime.revision().map_err(runtime_serve_error))?;
            (revision, lease)
        } else {
            let subscription = transport
                .subscription
                .lock()
                .map_err(|_| ServeError::Internal("subscription lock is poisoned".to_string()))?
                .clone();
            let lease = runtime
                .renew_subscription(&subscription, &authenticated.owner_id)
                .map_err(runtime_serve_error)?;
            (runtime.revision().map_err(runtime_serve_error)?, lease)
        };
        update_connection_session(
            &state,
            &body.connection_id,
            authenticated.session.logical().clone(),
        )?;
        Ok((
            "leaseRefresh",
            revision,
            Some(json!({ "leaseDeadlineMillis": lease.deadline_millis })),
        ))
    })();
    ephemeral_response(&state, &headers, sequence, result)
}

async fn resnapshot(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<EphemeralRequest>,
) -> Response {
    let sequence = body.client_request_sequence;
    let result = (|| {
        let authenticated = authenticated_session_from_request(&state, &headers)?;
        let runtime = ephemeral_runtime(&state)?;
        validate_ephemeral_identity(&state, runtime, &body)?;
        let transport = transport_for_request(&state, &body.connection_id)?;
        let current = transport
            .subscription
            .lock()
            .map_err(|_| ServeError::Internal("subscription lock is poisoned".to_string()))?
            .clone();
        let refreshed = runtime
            .resubscribe(&current, &authenticated.owner_id)
            .map_err(runtime_serve_error)?;
        *transport
            .subscription
            .lock()
            .map_err(|_| ServeError::Internal("subscription lock is poisoned".to_string()))? =
            refreshed.subscription;
        let revision = refreshed.snapshot.revision;
        Ok((
            "resnapshot",
            revision,
            Some(json!({ "ephemeralSnapshot": refreshed.snapshot })),
        ))
    })();
    ephemeral_response(&state, &headers, sequence, result)
}

fn ephemeral_runtime(state: &AppState) -> Result<&DatabaseRuntime<libsql::Database>, ServeError> {
    state.database.ephemeral().ok_or_else(|| {
        ServeError::BadRequest("this schema declares no ephemeral state".to_string())
    })
}

fn validate_ephemeral_identity(
    state: &AppState,
    runtime: &DatabaseRuntime<libsql::Database>,
    body: &EphemeralRequest,
) -> Result<(), ServeError> {
    ensure_database_id(state, Some(&body.database_id))?;
    if body.ephemeral_epoch != runtime.epoch() {
        return Err(ServeError::Conflict(
            "ephemeral runtime epoch is stale".to_string(),
        ));
    }
    Ok(())
}

fn transport_for_request(
    state: &AppState,
    connection_id: &str,
) -> Result<EphemeralTransport, ServeError> {
    state
        .connections
        .lock()
        .map_err(|_| ServeError::Internal("connection map is poisoned".to_string()))?
        .get(connection_id)
        .and_then(|connection| connection.ephemeral.clone())
        .ok_or_else(|| ServeError::Conflict("connection is not active".to_string()))
}

fn participant_for_request(
    state: &AppState,
    connection_id: &str,
) -> Result<Participant, ServeError> {
    transport_for_request(state, connection_id)?
        .participant
        .ok_or_else(|| ServeError::Forbidden("subscription is read-only".to_string()))
}

fn update_connection_session(
    state: &AppState,
    connection_id: &str,
    session: HashMap<String, pyre::sync::SessionValue>,
) -> Result<(), ServeError> {
    let mut connections = state
        .connections
        .lock()
        .map_err(|_| ServeError::Internal("connection map is poisoned".to_string()))?;
    let connection = connections
        .get_mut(connection_id)
        .ok_or_else(|| ServeError::Conflict("connection is not active".to_string()))?;
    connection.session = session;
    Ok(())
}

fn ephemeral_response(
    state: &AppState,
    headers: &HeaderMap,
    sequence: u64,
    result: Result<(&'static str, u64, Option<JsonValue>), ServeError>,
) -> Response {
    let response = match result {
        Ok((operation, revision, extra)) => {
            let mut value = json!({
                "type": "ephemeralAccepted",
                "operation": operation,
                "clientRequestSequence": sequence,
                "ephemeralEpoch": state.database.ephemeral().map(DatabaseRuntime::epoch),
                "revision": revision,
            });
            if let (Some(extra), Some(object)) = (extra, value.as_object_mut()) {
                if let Some(extra) = extra.as_object() {
                    object.extend(extra.clone());
                }
            }
            Json(value).into_response()
        }
        Err(error) => (
            error.status(),
            Json(json!({
                "type": "ephemeralRejected",
                "clientRequestSequence": sequence,
                "error": error.message(),
            })),
        )
            .into_response(),
    };
    with_cors(state, headers, response)
}

async fn run_query(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<RequestQuery>,
    AxumPath(query_id): AxumPath<String>,
    Json(input): Json<JsonValue>,
) -> Result<Response, ServeError> {
    ensure_database_id(&state, query.database_id.as_deref())?;
    let authenticated = authenticated_session_from_request(&state, &headers)?;
    let conn = state
        .database
        .database()
        .connect()
        .map_err(|error| ServeError::Internal(format!("database error: {}", error)))?;
    let mut result = if query.sync.as_deref() == Some("true") {
        pyre::server::query::run_sync(
            &conn,
            &state.manifest,
            &query_id,
            input,
            &authenticated.session,
        )
        .await
    } else {
        pyre::server::query::run(
            &conn,
            &state.manifest,
            &query_id,
            input,
            &authenticated.session,
        )
        .await
    }
    .map_err(query_error)?;

    if query.sync.as_deref() == Some("true") {
        let mut connected_sessions = connected_sessions(&state)?;
        // HTTP authority belongs to the authenticated request, even without SSE.
        // Never use a client connectionId to select authority or suppress a peer.
        let origin_id = new_connection_id();
        connected_sessions.insert(origin_id.clone(), authenticated.session.logical().clone());
        let context = state
            .loaded_schema
            .context()
            .map_err(|error| ServeError::Internal(error.to_string()))?;
        let server = SyncServer::new(context);
        let messages = server
            .calculate_deltas(
                &conn,
                &mut result,
                &connected_sessions,
                &state.database_id,
                Some(&origin_id),
            )
            .await
            .map_err(|error| ServeError::Internal(error.to_string()))?;
        send_messages(&state, messages).await;
    }

    Ok(with_cors(
        &state,
        &headers,
        Json(result.response).into_response(),
    ))
}

fn connected_sessions(state: &AppState) -> Result<ConnectedSessions, ServeError> {
    Ok(state
        .connections
        .lock()
        .map_err(|_| ServeError::Internal("connection map is poisoned".to_string()))?
        .iter()
        .map(|(id, connection)| (id.clone(), connection.session.clone()))
        .collect())
}

fn query_error(error: pyre::server::query::Error) -> ServeError {
    match error {
        pyre::server::query::Error::OutcomeUnknown(_) => ServeError::Internal(error.to_string()),
        _ => ServeError::BadRequest(error.to_string()),
    }
}

async fn send_messages(state: &AppState, messages: Vec<pyre::server::sync::SessionDeltaMessage>) {
    let terminated = {
        let Ok(mut connections) = state.connections.lock() else {
            return;
        };
        send_messages_to_connections(&mut connections, messages)
    };
    if let Some(runtime) = state.database.ephemeral() {
        for transport in terminated {
            cleanup_ephemeral_transport(runtime, &transport);
        }
    }
}

fn send_messages_to_connections(
    connections: &mut HashMap<String, Connection>,
    messages: Vec<pyre::server::sync::SessionDeltaMessage>,
) -> Vec<EphemeralTransport> {
    let mut terminate = Vec::new();
    for message in messages {
        if let Some(connection) = connections.get(&message.session_id) {
            if let Ok(value) = serde_json::to_value(message.message) {
                if connection.sender.try_send(value).is_err() {
                    // Closing the bounded stream forces durable catchup on reconnect.
                    terminate.push(message.session_id);
                }
            }
        }
    }
    terminate
        .into_iter()
        .filter_map(|id| connections.remove(&id)?.ephemeral)
        .collect()
}

struct ConnectionCleanup {
    state: Arc<AppState>,
    session_id: String,
    ephemeral: Option<EphemeralTransport>,
}

impl Drop for ConnectionCleanup {
    fn drop(&mut self) {
        if let Ok(mut connections) = self.state.connections.lock() {
            connections.remove(&self.session_id);
        }
        let (Some(runtime), Some(transport)) =
            (self.state.database.ephemeral(), self.ephemeral.as_ref())
        else {
            return;
        };
        cleanup_ephemeral_transport(runtime, transport);
    }
}

fn cleanup_ephemeral_transport(
    runtime: &DatabaseRuntime<libsql::Database>,
    transport: &EphemeralTransport,
) {
    if let Some(participant) = &transport.participant {
        let _ = runtime.transport_closed(participant);
    } else if let Ok(subscription) = transport.subscription.lock() {
        let _ = runtime.unsubscribe(&subscription);
    }
}

fn authenticated_session_from_request(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AuthenticatedSession, ServeError> {
    let (value, owner_id) = match &state.session_source {
        SessionSource::Empty => (
            JsonValue::Object(serde_json::Map::new()),
            format!("empty:{}", state.database_id),
        ),
        SessionSource::Dev(value) => (value.clone(), format!("dev:{}", state.database_id)),
        SessionSource::Header { name, secret } => {
            let raw = headers
                .get(name)
                .ok_or_else(|| ServeError::Unauthorized(format!("missing {} header", name)))?
                .to_str()
                .map_err(|_| ServeError::Unauthorized(format!("invalid {} header", name)))?;
            if let Some(secret) = secret {
                let payload = decode_signed_session(raw, secret)?;
                let owner_id =
                    match payload.session_key {
                        Some(key) if !key.trim().is_empty() => owner_id_from_session_key(&key),
                        _ if state.database.ephemeral().is_none() => owner_id_from_credential(raw),
                        _ => return Err(ServeError::Unauthorized(
                            "signed sessions require a non-empty sessionKey for ephemeral state"
                                .to_string(),
                        )),
                    };
                (payload.session, owner_id)
            } else {
                (decode_unsigned_session(raw)?, owner_id_from_credential(raw))
            }
        }
    };

    let session = PyreSession::new(value, &state.manifest.session_schema)
        .map_err(|error| ServeError::Unauthorized(format!("invalid Pyre session: {}", error)))?;
    Ok(AuthenticatedSession { session, owner_id })
}

fn owner_id_from_credential(raw: &str) -> String {
    let digest = Sha256::digest(raw.as_bytes());
    format!("credential:{digest:x}")
}

fn owner_id_from_session_key(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    format!("session:{digest:x}")
}

fn runtime_serve_error(error: RuntimeError) -> ServeError {
    match error {
        RuntimeError::Validation(_) | RuntimeError::StateNotDeclared(_) => {
            ServeError::BadRequest(error.to_string())
        }
        RuntimeError::OwnerMismatch
        | RuntimeError::ReadOnly
        | RuntimeError::SharedServerOnly
        | RuntimeError::EmptyOwnerId => ServeError::Forbidden(error.to_string()),
        RuntimeError::WrongDatabase
        | RuntimeError::StaleEpoch
        | RuntimeError::UnknownConnection
        | RuntimeError::StaleParticipant
        | RuntimeError::UnknownSubscription
        | RuntimeError::StaleSubscription
        | RuntimeError::LeaseExpired
        | RuntimeError::Closed => ServeError::Conflict(error.to_string()),
        RuntimeError::PayloadTooLarge | RuntimeError::Capacity => {
            ServeError::PayloadTooLarge(error.to_string())
        }
        _ => ServeError::Internal(error.to_string()),
    }
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

fn deserialize_client_request_sequence<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let sequence = u64::deserialize(deserializer)?;
    if sequence > MAX_CLIENT_REQUEST_SEQUENCE {
        return Err(serde::de::Error::custom(
            "clientRequestSequence must be a non-negative JavaScript-safe integer",
        ));
    }
    Ok(sequence)
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
        HeaderValue::from_static("GET, POST, PATCH, OPTIONS"),
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
    use pyre::server::manifest::FieldSchema;

    #[test]
    fn sync_request_accepts_public_client_cursor_envelope() {
        let request: SyncRequest = serde_json::from_value(json!({
            "databaseId": "proof",
            "syncCursor": { "tables": {} }
        }))
        .unwrap();

        assert_eq!(request.database_id.as_deref(), Some("proof"));
        assert!(request.sync_cursor.tables.is_empty());
    }

    #[tokio::test]
    async fn unknown_commit_outcome_is_not_an_http_rejection() {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let conn = db.connect().unwrap();
        let error = conn.execute("COMMIT", ()).await.unwrap_err();
        let response =
            query_error(pyre::server::query::Error::OutcomeUnknown(error)).into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let rejected =
            query_error(pyre::server::query::Error::InvalidInput("invalid".into())).into_response();
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    }

    fn manifest_with_session() -> Manifest {
        Manifest {
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
            ephemeral: None,
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
            participant_shared_writes: false,
        }
    }

    #[test]
    fn unsigned_session_header_contains_full_session_json() {
        let encoded = URL_SAFE_NO_PAD.encode(r#"{"userId":123}"#);
        let session = decode_unsigned_session(&encoded).expect("decoded session");

        assert_eq!(session["userId"], json!(123));
    }

    #[test]
    fn credential_owners_are_stable_and_do_not_contain_credentials() {
        let first = owner_id_from_credential("secret credential");
        assert_eq!(first, owner_id_from_credential("secret credential"));
        assert_ne!(first, owner_id_from_credential("other credential"));
        assert!(!first.contains("secret credential"));
    }

    #[test]
    fn signed_session_keys_are_stable_across_token_refresh() {
        let first = owner_id_from_session_key("stable-session");
        assert_eq!(first, owner_id_from_session_key("stable-session"));
        assert_ne!(first, owner_id_from_session_key("another-session"));
        assert!(!first.contains("stable-session"));
    }

    #[test]
    fn signed_session_header_verifies_signature_and_expiration() {
        let payload = URL_SAFE_NO_PAD
            .encode(r#"{"session":{"userId":123},"exp":4102444800,"sessionKey":"stable"}"#);
        let mut mac = HmacSha256::new_from_slice(b"secret").expect("hmac");
        mac.update(payload.as_bytes());
        let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        let raw = format!("{}.{}", payload, signature);

        let payload = decode_signed_session(&raw, "secret").expect("decoded session");

        assert_eq!(payload.session["userId"], json!(123));
        assert_eq!(payload.session_key.as_deref(), Some("stable"));
        assert!(decode_signed_session(&raw, "wrong-secret").is_err());
    }

    #[test]
    fn client_request_sequence_is_a_js_safe_non_negative_integer() {
        let request = |sequence: JsonValue| {
            serde_json::from_value::<EphemeralRequest>(json!({
                "databaseId": "db",
                "ephemeralEpoch": "epoch",
                "connectionId": "connection",
                "clientRequestSequence": sequence,
            }))
        };

        assert_eq!(
            request(json!(MAX_CLIENT_REQUEST_SEQUENCE))
                .unwrap()
                .client_request_sequence,
            MAX_CLIENT_REQUEST_SEQUENCE
        );
        assert!(request(json!(-1)).is_err());
        assert!(request(json!(1.5)).is_err());
        assert!(request(json!(MAX_CLIENT_REQUEST_SEQUENCE + 1)).is_err());
        assert!(request(json!({"arbitrary": true})).is_err());
    }

    #[test]
    fn durable_overflow_removes_connection_and_closes_receiver() {
        use pyre::server::sync::{DeltaMessage, SessionDeltaMessage};

        let (sender, mut receiver) = mpsc::channel(1);
        let mut connections = HashMap::from([(
            "slow".to_string(),
            Connection {
                session: HashMap::new(),
                sender,
                ephemeral: None,
            },
        )]);
        let message = || SessionDeltaMessage {
            session_id: "slow".to_string(),
            message: DeltaMessage::sync_required(),
        };

        let terminated = send_messages_to_connections(&mut connections, vec![message(), message()]);

        assert!(!connections.contains_key("slow"));
        assert!(terminated.is_empty());
        assert!(receiver.try_recv().is_ok());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
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
