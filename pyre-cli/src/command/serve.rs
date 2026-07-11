use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use pyre::server::manifest::{Manifest, PyreSession};
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
    session: HashMap<String, pyre::sync::SessionValue>,
    sender: mpsc::UnboundedSender<JsonValue>,
}

#[derive(Deserialize)]
struct SyncRequest {
    #[serde(rename = "databaseId")]
    database_id: Option<String>,
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
        .catchup(
            &conn,
            &body.sync_cursor,
            session.logical(),
            state.page_size,
            &state.database_id,
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
    let session_id = new_connection_id();
    let (sender, mut receiver) = mpsc::unbounded_channel();
    state.connections.lock().await.insert(
        session_id.clone(),
        Connection {
            session: session.logical().clone(),
            sender,
        },
    );

    let connected = json!({
        "type": "connected",
        "sessionId": session_id,
        "connectionId": session_id,
        "databaseId": state.database_id,
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
    let mut result = if query.sync.as_deref() == Some("true") {
        pyre::server::query::run_sync(&conn, &state.manifest, &query_id, input, &session).await
    } else {
        pyre::server::query::run(&conn, &state.manifest, &query_id, input, &session).await
    }
    .map_err(|error| ServeError::BadRequest(error.to_string()))?;

    if query.sync.as_deref() == Some("true") {
        let connected_sessions = connected_sessions(&state).await;
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
                query.connection_id.as_deref(),
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

async fn connected_sessions(state: &AppState) -> ConnectedSessions {
    state
        .connections
        .lock()
        .await
        .iter()
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
    let value = match &state.session_source {
        SessionSource::Empty => JsonValue::Object(serde_json::Map::new()),
        SessionSource::Dev(value) => value.clone(),
        SessionSource::Header { name, secret } => {
            let raw = headers
                .get(name)
                .ok_or_else(|| ServeError::Unauthorized(format!("missing {} header", name)))?
                .to_str()
                .map_err(|_| ServeError::Unauthorized(format!("invalid {} header", name)))?;
            if let Some(secret) = secret {
                decode_signed_session(raw, secret)?
            } else {
                decode_unsigned_session(raw)?
            }
        }
    };

    PyreSession::new(value, &state.manifest.session_schema)
        .map_err(|error| ServeError::Unauthorized(format!("invalid Pyre session: {}", error)))
}

fn decode_unsigned_session(raw: &str) -> Result<JsonValue, ServeError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| ServeError::Unauthorized("invalid session header encoding".to_string()))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ServeError::Unauthorized("invalid session header JSON".to_string()))
}

fn decode_signed_session(raw: &str, secret: &str) -> Result<JsonValue, ServeError> {
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
    Ok(payload.session)
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
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type, x-pyre-session"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyre::server::manifest::FieldSchema;

    fn manifest_with_session() -> Manifest {
        Manifest {
            version: 1,
            session_schema: HashMap::from([(
                "userId".to_string(),
                FieldSchema {
                    type_: "Int".to_string(),
                    is_enum: false,
                    enum_variants: Vec::new(),
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

        assert_eq!(session["userId"], json!(123));
        assert!(decode_signed_session(&raw, "wrong-secret").is_err());
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
