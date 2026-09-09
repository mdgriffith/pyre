//! Internal lifecycle slice. No routes, live delivery, or delta publication.
//! Schema coherence, projection dependencies, and operation exposure are trusted
//! configuration until generated metadata can establish them. Invalidation is a
//! local barrier only; authoritative-read staleness adds to the maximum lease.
#![allow(dead_code)] // Deliberately not wired into the public server API yet.

use super::{ContextMessage, ContextRequest};
use crate::server::{
    manifest::{Manifest, PyreSession},
    query, sync,
};
use crate::typecheck;
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Supplied by authentication middleware, never decoded from context JSON.
#[derive(Clone)]
pub(crate) struct AuthenticatedRequest {
    pub identity: String,
    pub credential: String,
    pub expires_at: SystemTime,
}

/// Construct once from a coherent compilation, then share this exact allocation.
pub(crate) struct SchemaArtifact {
    pub schema_id: String,
    pub manifest: Arc<Manifest>,
    pub context: Arc<typecheck::Context>,
}

#[derive(Clone)]
pub(crate) struct AuthorizedDatabase {
    database_id: String,
    connection: Arc<tokio::sync::Mutex<libsql::Connection>>,
    schema: Arc<SchemaArtifact>,
}

impl AuthorizedDatabase {
    /// Transfers exclusive use of this connection to the runtime. Do not retain
    /// or use external connection clones, or wrap them in another handle. Clone
    /// this handle across negotiations so every scope shares the same SQL lock.
    pub(crate) fn new(
        database_id: String,
        connection: libsql::Connection,
        schema: Arc<SchemaArtifact>,
    ) -> Self {
        Self {
            database_id,
            connection: Arc::new(tokio::sync::Mutex::new(connection)),
            schema,
        }
    }
}

pub(crate) struct Resolution {
    pub database: AuthorizedDatabase,
    pub session: Value,
    pub cache_scope: String,
    pub authority_revision: String,
    pub access_deadline: Option<SystemTime>,
}

pub(crate) struct Config {
    pub schema: Arc<SchemaArtifact>,
    pub client_fields: HashSet<String>,
    pub required_client_fields: HashSet<String>,
    pub exposed_operations: HashSet<String>,
    pub max_age: Duration,
}

#[derive(Debug)]
pub(crate) enum Error {
    Unauthenticated,
    Denied,
    Unavailable,
    InvalidConfiguration,
    InvalidResolution,
    ContextMismatch,
    StaleResult,
    /// A dispatched mutation might have committed. Never automatically retry.
    MutationOutcomeUnknown,
    Query(query::Error),
    Sync(sync::Error),
}

struct Lease {
    wall: SystemTime,
    monotonic: Instant,
}

impl Lease {
    fn new(wall: SystemTime, start_wall: SystemTime, start: Instant) -> Result<Self, Error> {
        let remaining = wall
            .duration_since(start_wall)
            .map_err(|_| Error::ContextMismatch)?;
        Ok(Self {
            wall,
            monotonic: start
                .checked_add(remaining)
                .ok_or(Error::InvalidConfiguration)?,
        })
    }

    fn valid_at(&self, wall: SystemTime, monotonic: Instant) -> bool {
        wall < self.wall && monotonic < self.monotonic
    }

    fn valid(&self) -> bool {
        self.valid_at(SystemTime::now(), Instant::now())
    }
}

struct Entry {
    id: String,
    auth: AuthenticatedRequest,
    database: AuthorizedDatabase,
    session: PyreSession,
    lease: Lease,
}

struct Registry {
    // Pending negotiations retain the old allocation, so pointer identity cannot
    // be reused (unlike a wrapping counter). Unrelated pending work is fenced too.
    generation: Arc<()>,
    entries: HashMap<String, Arc<Entry>>,
}

pub(crate) struct Runtime<R> {
    config: Config,
    resolver: R,
    registry: Arc<Mutex<Registry>>,
    operations: Arc<HashSet<String>>,
}

#[derive(Clone, Copy)]
pub(crate) enum Invalidation<'a> {
    Credential(&'a str),
    IdentityDatabase {
        identity: &'a str,
        database: &'a str,
    },
    Database(&'a str),
}

impl<R> Runtime<R> {
    pub(crate) fn new(config: Config, resolver: R) -> Result<Self, Error> {
        if config.max_age.is_zero()
            || SystemTime::now().checked_add(config.max_age).is_none()
            || Instant::now().checked_add(config.max_age).is_none()
            || !config
                .required_client_fields
                .is_subset(&config.client_fields)
            || config
                .client_fields
                .iter()
                .any(|field| !config.schema.manifest.session_schema.contains_key(field))
            || config
                .exposed_operations
                .iter()
                .any(|id| !config.schema.manifest.queries.contains_key(id))
        {
            return Err(Error::InvalidConfiguration);
        }
        Ok(Self {
            operations: Arc::new(config.exposed_operations.clone()),
            config,
            resolver,
            registry: Arc::new(Mutex::new(Registry {
                generation: Arc::new(()),
                entries: HashMap::new(),
            })),
        })
    }

    pub(crate) async fn negotiate<F>(
        &self,
        auth: AuthenticatedRequest,
        request: ContextRequest,
    ) -> Result<ContextMessage, Error>
    where
        R: Fn(AuthenticatedRequest, String) -> F,
        F: Future<Output = Result<Resolution, Error>>,
    {
        // Both clocks and the invalidation fence precede application I/O.
        let start = Instant::now();
        let start_wall = SystemTime::now();
        validate_auth(&auth, start_wall)?;
        let request: ContextRequest = serde_json::from_value(
            serde_json::to_value(request).map_err(|_| Error::InvalidResolution)?,
        )
        .map_err(|_| Error::InvalidResolution)?;
        let generation = self
            .registry
            .lock()
            .map_err(|_| Error::Unavailable)?
            .generation
            .clone();
        let resolution = (self.resolver)(auth.clone(), request.database_id.clone()).await?;
        if resolution.database.database_id != request.database_id {
            return Err(Error::Denied);
        }
        if !Arc::ptr_eq(&resolution.database.schema, &self.config.schema) {
            return Err(Error::InvalidResolution);
        }
        let session = PyreSession::new(
            resolution.session.clone(),
            &self.config.schema.manifest.session_schema,
        )
        .map_err(|_| Error::InvalidResolution)?;
        let effective = resolution
            .session
            .as_object()
            .ok_or(Error::InvalidResolution)?;
        let mut projection = Map::new();
        for field in &self.config.client_fields {
            // Missing optional server fields are not silently projected as null.
            projection.insert(
                field.clone(),
                effective
                    .get(field)
                    .ok_or(Error::InvalidResolution)?
                    .clone(),
            );
        }
        let deadline = start_wall
            .checked_add(self.config.max_age)
            .ok_or(Error::InvalidConfiguration)?
            .min(auth.expires_at)
            .min(resolution.access_deadline.unwrap_or(auth.expires_at));
        let expires_at = u64::try_from(
            deadline
                .duration_since(UNIX_EPOCH)
                .map_err(|_| Error::InvalidResolution)?
                .as_millis(),
        )
        .map_err(|_| Error::InvalidResolution)?;
        let lease = Lease::new(
            UNIX_EPOCH + Duration::from_millis(expires_at),
            start_wall,
            start,
        )?;
        if !lease.valid() {
            return Err(Error::ContextMismatch);
        }
        let connection = resolution.database.connection.lock().await;
        if !lease.valid()
            || !Arc::ptr_eq(
                &generation,
                &self
                    .registry
                    .lock()
                    .map_err(|_| Error::Unavailable)?
                    .generation,
            )
        {
            return Err(Error::ContextMismatch);
        }
        let mut rows = connection
            .query("SELECT database_epoch FROM _pyre_sync WHERE id = 1", ())
            .await
            .map_err(|_| Error::Unavailable)?;
        let epoch: String = rows
            .next()
            .await
            .map_err(|_| Error::Unavailable)?
            .ok_or(Error::Unavailable)?
            .get(0)
            .map_err(|_| Error::Unavailable)?;
        drop(rows);
        drop(connection);
        let mut random = [0u8; 32];
        getrandom::getrandom(&mut random).map_err(|_| Error::Unavailable)?;
        let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let message = ContextMessage::Context {
            protocol_version: 1,
            database_id: request.database_id,
            context_id: id.clone(),
            schema_id: self.config.schema.schema_id.clone(),
            cache_scope: resolution.cache_scope,
            authority_revision: resolution.authority_revision,
            database_epoch: epoch,
            session: projection,
            expires_at,
        };
        // Public wire constructors don't validate. Roundtrip through the codec to
        // reject unsafe integers and malformed identifiers before registration.
        let message = serde_json::from_value(
            serde_json::to_value(message).map_err(|_| Error::InvalidResolution)?,
        )
        .map_err(|_| Error::InvalidResolution)?;
        let mut registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
        if !Arc::ptr_eq(&generation, &registry.generation) || !lease.valid() {
            return Err(Error::ContextMismatch);
        }
        if registry.entries.contains_key(&id) {
            return Err(Error::Unavailable);
        }
        registry.entries.retain(|_, entry| entry.lease.valid());
        registry.entries.insert(
            id.clone(),
            Arc::new(Entry {
                id,
                auth,
                database: resolution.database,
                session,
                lease,
            }),
        );
        Ok(message)
    }

    pub(crate) fn authorize(
        &self,
        auth: &AuthenticatedRequest,
        database: &str,
        context: &str,
    ) -> Result<AuthorizedScope, Error> {
        let start = Instant::now();
        let wall = SystemTime::now();
        validate_auth(auth, wall)?;
        let registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
        let entry = registry
            .entries
            .get(context)
            .ok_or(Error::ContextMismatch)?;
        if entry.auth.identity != auth.identity
            || entry.auth.credential != auth.credential
            || entry.database.database_id != database
            || !entry.lease.valid()
        {
            return Err(Error::ContextMismatch);
        }
        Ok(AuthorizedScope {
            registry: self.registry.clone(),
            entry: entry.clone(),
            request_lease: Lease::new(auth.expires_at.min(entry.lease.wall), wall, start)?,
            operations: self.operations.clone(),
        })
    }

    /// Atomically withdraws affected scopes and fences every pending negotiation.
    /// Does not cancel SQL already dispatched or provide cross-process revocation.
    pub(crate) fn invalidate(&self, target: Invalidation<'_>) -> Result<(), Error> {
        let mut registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
        registry.generation = Arc::new(());
        registry.entries.retain(|_, entry| !match target {
            Invalidation::Credential(credential) => entry.auth.credential == credential,
            Invalidation::IdentityDatabase { identity, database } => {
                entry.auth.identity == identity && entry.database.database_id == database
            }
            Invalidation::Database(database) => entry.database.database_id == database,
        });
        Ok(())
    }
}

fn validate_auth(auth: &AuthenticatedRequest, now: SystemTime) -> Result<(), Error> {
    if auth.identity.trim().is_empty()
        || auth.credential.trim().is_empty()
        || auth.expires_at <= now
    {
        Err(Error::Unauthenticated)
    } else {
        Ok(())
    }
}

/// No unchecked connection/session access and no origin connection ID. Each
/// request must authenticate again to obtain a scope; leases also bound its use.
pub(crate) struct AuthorizedScope {
    registry: Arc<Mutex<Registry>>,
    entry: Arc<Entry>,
    request_lease: Lease,
    operations: Arc<HashSet<String>>,
}

pub(crate) struct BoundResult<T> {
    pub database_id: String,
    pub context_id: String,
    pub value: T,
}

impl AuthorizedScope {
    fn check(&self) -> Result<(), Error> {
        let registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
        if !self.request_lease.valid()
            || !self.entry.lease.valid()
            || !registry
                .entries
                .get(&self.entry.id)
                .is_some_and(|entry| Arc::ptr_eq(entry, &self.entry))
        {
            return Err(Error::ContextMismatch);
        }
        Ok(())
    }

    fn finish<T>(&self, result: Result<T, Error>, mutation: bool) -> Result<BoundResult<T>, Error> {
        if self.check().is_err() {
            return Err(if mutation {
                Error::MutationOutcomeUnknown
            } else {
                Error::StaleResult
            });
        }
        Ok(BoundResult {
            database_id: self.entry.database.database_id.clone(),
            context_id: self.entry.id.clone(),
            value: result?,
        })
    }

    pub(crate) async fn query(
        &self,
        operation: &str,
        input: Value,
    ) -> Result<BoundResult<Value>, Error> {
        self.check()?;
        if !self.operations.contains(operation) {
            return Err(Error::Denied);
        }
        let database = &self.entry.database;
        let connection = database.connection.lock().await;
        self.check()?;
        let mutation = database.schema.manifest.queries[operation].operation != "query";
        // Fixed ordinary execution policy. No caller-selected sync SQL and no raw
        // affected rows exposed for accidental unfiltered publication.
        let result = query::run(
            &connection,
            &database.schema.manifest,
            operation,
            input,
            &self.entry.session,
        )
        .await
        .map(|result| result.response)
        .map_err(Error::Query);
        self.finish(result, mutation)
    }

    pub(crate) async fn catchup(
        &self,
        cursor: &crate::sync::SyncCursor,
        page_size: usize,
        client_epoch: Option<&str>,
    ) -> Result<BoundResult<sync::CatchupResponse>, Error> {
        self.check()?;
        let database = &self.entry.database;
        let connection = database.connection.lock().await;
        self.check()?;
        let result = sync::SyncServer::new(&database.schema.context)
            .catchup_protocol(
                &connection,
                cursor,
                self.entry.session.logical(),
                page_size,
                &database.database_id,
                client_epoch,
            )
            .await
            .map_err(Error::Sync);
        self.finish(result, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        pin::Pin,
        sync::atomic::{AtomicBool, Ordering},
        task::Poll,
    };

    fn run(future: impl Future<Output = ()>) {
        tokio::runtime::Runtime::new().unwrap().block_on(future);
    }

    fn schema() -> Arc<SchemaArtifact> {
        Arc::new(SchemaArtifact {
            schema_id: "test-schema".into(),
            context: Arc::new(typecheck::empty_context()),
            manifest: Arc::new(serde_json::from_value(json!({
                "version": 1,
                "session_schema": {
                    "role": {"type": "String", "nullable": false, "omittable": false},
                    "secret": {"type": "String", "nullable": false, "omittable": false},
                    "optional": {"type": "Int", "nullable": true, "omittable": true}
                },
                "queries": {
                    "role": {"id": "role", "operation": "query", "input_schema": {},
                        "session_args": ["role"], "optional_input_args": [], "json_input_args": [],
                        "sql": [{"include": true, "params": ["session_role"],
                            "sql": "SELECT json_quote($session_role) AS role"}],
                        "syncSql": [{"include": true, "params": [], "sql": "SELECT 'false' AS role"}]},
                    "write": {"id": "write", "operation": "insert", "input_schema": {},
                        "session_args": ["role"], "optional_input_args": [], "json_input_args": [],
                        "sql": [{"include": false, "params": ["session_role"],
                            "sql": "INSERT INTO writes(role) VALUES ($session_role)"}]},
                    "private": {"id": "private", "operation": "query", "input_schema": {},
                        "session_args": [], "optional_input_args": [], "json_input_args": [], "sql": []}
                }
            })).unwrap()),
        })
    }

    fn config(schema: Arc<SchemaArtifact>) -> Config {
        Config {
            schema,
            client_fields: HashSet::from(["role".into()]),
            required_client_fields: HashSet::from(["role".into()]),
            exposed_operations: HashSet::from(["role".into(), "write".into()]),
            max_age: Duration::from_secs(60),
        }
    }

    fn auth() -> AuthenticatedRequest {
        AuthenticatedRequest {
            identity: "alice".into(),
            credential: "login-1".into(),
            expires_at: SystemTime::now() + Duration::from_secs(120),
        }
    }

    fn request(database: &str) -> ContextRequest {
        ContextRequest {
            protocol_version: 1,
            database_id: database.into(),
        }
    }

    async fn connection() -> Arc<tokio::sync::Mutex<libsql::Connection>> {
        let db = libsql::Builder::new_local(":memory:")
            .build()
            .await
            .unwrap();
        let connection = db.connect().unwrap();
        connection.execute_batch("CREATE TABLE _pyre_sync(id INTEGER PRIMARY KEY, database_epoch TEXT, server_revision INTEGER); INSERT INTO _pyre_sync VALUES (1, 'epoch', 0); CREATE TABLE writes(role TEXT);").await.unwrap();
        Arc::new(tokio::sync::Mutex::new(connection))
    }

    fn resolution(
        schema: Arc<SchemaArtifact>,
        connection: Arc<tokio::sync::Mutex<libsql::Connection>>,
        database: String,
    ) -> Resolution {
        let role = if database == "a" { "Admin" } else { "Player" };
        Resolution {
            database: AuthorizedDatabase {
                database_id: database,
                schema,
                connection,
            },
            session: json!({"role": role, "secret": "server-only"}),
            cache_scope: "alice-cache".into(),
            authority_revision: "revision-1".into(),
            access_deadline: None,
        }
    }

    fn id(message: &ContextMessage) -> &str {
        match message {
            ContextMessage::Context { context_id, .. } => context_id,
            _ => panic!("expected context"),
        }
    }

    // A deterministic async resolver barrier: poll to Pending, invalidate, then
    // release and poll again. No sleeps or scheduler ordering assumptions.
    async fn pending(future: Pin<&mut impl Future>) {
        let mut future = future;
        std::future::poll_fn(|cx| {
            assert!(future.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }

    #[test]
    fn generated_permissions_and_shared_connection_serialization() {
        run(async {
            use crate::{
                ast,
                db::{diff, introspect, migrate},
                generate, parser,
            };
            use generate::sql::to_sql::SqlAndParams;

            // Same parse/typecheck/migrate/manifest path as the query_server and
            // sync_server fixtures, without their filesystem test harness.
            let mut schema = ast::Schema::default();
            parser::run(
                "schema.pyre",
                r#"
session {
    userId Int
}
record Note {
    id Int @id
    ownerId Int
    body String
    updatedAt Int
    @allow(*) { ownerId == Session.userId }
}
"#,
                &mut schema,
            )
            .unwrap();
            let context = typecheck::check_schema(&ast::Database {
                schemas: vec![schema.clone()],
            })
            .unwrap();
            let queries = parser::parse_query(
                "query.pyre",
                r#"
query VisibleNotes {
    note {
        @sort(id, Asc)
        id
        body
    }
}
insert CreateNote($body: String) {
    note {
        ownerId = Session.userId
        body = $body
        updatedAt = 30
    }
}
"#,
            )
            .unwrap();
            let query_info = typecheck::check_queries(&queries, &context).unwrap();
            let mut files = Vec::new();
            generate::manifest::generate_queries(&context, &queries, &query_info, &mut files);
            let manifest: Manifest = serde_json::from_str(
                &files
                    .iter()
                    .find(|file| file.path == std::path::Path::new("manifest.json"))
                    .unwrap()
                    .contents,
            )
            .unwrap();
            let read_id = manifest
                .queries
                .values()
                .find(|query| query.operation == "query")
                .unwrap()
                .id
                .clone();
            let write_id = manifest
                .queries
                .values()
                .find(|query| query.operation == "insert")
                .unwrap()
                .id
                .clone();
            let empty = introspect::Introspection {
                tables: vec![],
                migration_state: introspect::MigrationState::NoMigrationTable,
                schema: introspect::SchemaResult::Success {
                    schema: ast::Schema::default(),
                    context: typecheck::empty_context(),
                },
            };
            let db = libsql::Builder::new_local(":memory:")
                .build()
                .await
                .unwrap();
            let connection = db.connect().unwrap();
            for statement in migrate::internal_setup_sql()
                .into_iter()
                .chain(diff::to_sql::to_sql(&diff::diff(&context, &schema, &empty)))
            {
                match statement {
                    SqlAndParams::Sql(sql) => {
                        connection.execute_batch(&sql).await.unwrap();
                    }
                    SqlAndParams::SqlWithParams { sql, args } => {
                        connection
                            .execute(&sql, libsql::params_from_iter(args))
                            .await
                            .unwrap();
                    }
                }
            }
            connection.execute_batch("INSERT INTO notes(id, ownerId, body, updatedAt) VALUES (1, 1, 'alice-only', 10), (2, 2, 'bob-only', 20);").await.unwrap();
            let schema = Arc::new(SchemaArtifact {
                schema_id: "owner-permission-schema".into(),
                manifest: Arc::new(manifest),
                context: Arc::new(context),
            });
            let database = AuthorizedDatabase::new("a".into(), connection, schema.clone());
            let runtime = Runtime::new(
                Config {
                    schema,
                    client_fields: HashSet::from(["userId".into()]),
                    required_client_fields: HashSet::from(["userId".into()]),
                    exposed_operations: HashSet::from([read_id.clone(), write_id.clone()]),
                    max_age: Duration::from_secs(60),
                },
                |auth: AuthenticatedRequest, requested: String| {
                    assert_eq!(requested, "a");
                    std::future::ready(Ok(Resolution {
                        database: database.clone(),
                        session: json!({"userId": if auth.identity == "alice" { 1 } else { 2 }}),
                        cache_scope: auth.identity,
                        authority_revision: "1".into(),
                        access_deadline: None,
                    }))
                },
            )
            .unwrap();
            let alice = auth();
            let mut bob = auth();
            bob.identity = "bob".into();
            bob.credential = "login-2".into();
            let alice_context = runtime
                .negotiate(alice.clone(), request("a"))
                .await
                .unwrap();
            let bob_context = runtime.negotiate(bob.clone(), request("a")).await.unwrap();
            let alice_scope = runtime.authorize(&alice, "a", id(&alice_context)).unwrap();
            let bob_scope = runtime.authorize(&bob, "a", id(&bob_context)).unwrap();
            for (scope, owner, body) in
                [(&alice_scope, 1, "alice-only"), (&bob_scope, 2, "bob-only")]
            {
                assert_eq!(
                    scope.query(&read_id, json!({})).await.unwrap().value,
                    json!({"note": [{"id": owner, "body": body}]})
                );
                let sync::CatchupResponse::Page(page) = scope
                    .catchup(&HashMap::new(), 10, None)
                    .await
                    .unwrap()
                    .value
                else {
                    panic!("expected non-reset page")
                };
                assert_eq!(page.tables["notes"].rows.len(), 1);
                assert_eq!(page.tables["notes"].rows[0]["id"], owner);
                assert_eq!(page.tables["notes"].rows[0]["body"], body);
            }

            let connection = database.connection.lock().await;
            let tx = connection
                .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
                .await
                .unwrap();
            tx.execute_batch("INSERT INTO notes(id, ownerId, body, updatedAt) VALUES (3, 1, 'dirty-alice', 25), (4, 2, 'dirty-bob', 25); UPDATE _pyre_sync SET database_epoch = 'dirty-epoch' WHERE id = 1;").await.unwrap();
            let cursor = HashMap::new();
            let mut read = Box::pin(alice_scope.query(&read_id, json!({})));
            let mut catchup = Box::pin(bob_scope.catchup(&cursor, 10, None));
            let mut write = Box::pin(alice_scope.query(&write_id, json!({"body": "committed"})));
            let mut negotiation = Box::pin(runtime.negotiate(alice.clone(), request("a")));
            pending(read.as_mut()).await;
            pending(catchup.as_mut()).await;
            pending(write.as_mut()).await;
            pending(negotiation.as_mut()).await;
            tx.rollback().await.unwrap();
            drop(connection);
            assert_eq!(
                read.await.unwrap().value,
                json!({"note": [{"id": 1, "body": "alice-only"}]})
            );
            let sync::CatchupResponse::Page(page) = catchup.await.unwrap().value else {
                panic!("expected page")
            };
            assert_eq!(page.tables["notes"].rows.len(), 1);
            assert_eq!(page.tables["notes"].rows[0]["id"], 2);
            write.await.unwrap(); // No nested transaction; the held transaction is gone.
            let ContextMessage::Context {
                database_epoch: fresh_epoch,
                ..
            } = negotiation.await.unwrap()
            else {
                unreachable!()
            };
            let ContextMessage::Context {
                database_epoch: original_epoch,
                ..
            } = alice_context
            else {
                unreachable!()
            };
            assert_eq!(fresh_epoch, original_epoch);

            let connection = database.connection.lock().await;
            let mut write =
                Box::pin(alice_scope.query(&write_id, json!({"body": "must-not-dispatch"})));
            let mut read = Box::pin(alice_scope.query(&read_id, json!({})));
            let mut catchup = Box::pin(alice_scope.catchup(&cursor, 10, None));
            let mut negotiation = Box::pin(runtime.negotiate(alice.clone(), request("a")));
            pending(write.as_mut()).await;
            pending(read.as_mut()).await;
            pending(catchup.as_mut()).await;
            pending(negotiation.as_mut()).await;
            runtime
                .invalidate(Invalidation::Credential(&alice.credential))
                .unwrap();
            drop(connection);
            assert!(matches!(write.await, Err(Error::ContextMismatch)));
            assert!(matches!(read.await, Err(Error::ContextMismatch)));
            assert!(matches!(catchup.await, Err(Error::ContextMismatch)));
            assert!(matches!(negotiation.await, Err(Error::ContextMismatch)));
            let connection = database.connection.lock().await;
            let mut rows = connection
                .query("SELECT id FROM notes ORDER BY id", ())
                .await
                .unwrap();
            let mut ids = Vec::new();
            while let Some(row) = rows.next().await.unwrap() {
                ids.push(row.get::<i64>(0).unwrap());
            }
            assert_eq!(ids, vec![1, 2, 3]);
        });
    }

    #[test]
    fn two_databases_tabs_binding_and_restart() {
        run(async {
            let schema = schema();
            let a = connection().await;
            let b = connection().await;
            let runtime = Runtime::new(
                config(schema.clone()),
                |auth: AuthenticatedRequest, database: String| {
                    assert_eq!(auth.identity, "alice");
                    std::future::ready(if database == "a" || database == "b" {
                        Ok(resolution(
                            schema.clone(),
                            if database == "a" {
                                a.clone()
                            } else {
                                b.clone()
                            },
                            database,
                        ))
                    } else {
                        Err(Error::Denied)
                    })
                },
            )
            .unwrap();
            let auth = auth();
            let first = runtime.negotiate(auth.clone(), request("a")).await.unwrap();
            let tab = runtime.negotiate(auth.clone(), request("a")).await.unwrap();
            let second = runtime.negotiate(auth.clone(), request("b")).await.unwrap();
            assert_ne!(id(&first), id(&tab));
            assert_ne!(id(&first), id(&second));
            assert_eq!(id(&first).len(), 64);
            for (message, database, role) in [
                (&first, "a", "Admin"),
                (&tab, "a", "Admin"),
                (&second, "b", "Player"),
            ] {
                let ContextMessage::Context { session, .. } = message else {
                    unreachable!()
                };
                assert_eq!(session, json!({"role": role}).as_object().unwrap());
                let scope = runtime.authorize(&auth, database, id(message)).unwrap();
                let result = scope.query("role", json!({})).await.unwrap();
                assert_eq!(result.database_id, database);
                assert_eq!(result.context_id, id(message));
                assert_eq!(result.value, json!({"role": [role]}));
                assert!(matches!(
                    scope.query("private", json!({})).await,
                    Err(Error::Denied)
                ));
                assert!(matches!(
                    scope.query("unknown", json!({})).await,
                    Err(Error::Denied)
                ));
                let catchup = scope
                    .catchup(&HashMap::new(), 10, Some("old-epoch"))
                    .await
                    .unwrap();
                assert_eq!(catchup.context_id, id(message));
                assert!(matches!(catchup.value, sync::CatchupResponse::Reset(_)));
            }
            let mut forged = auth.clone();
            forged.identity = "mallory".into();
            assert!(matches!(
                runtime.authorize(&forged, "a", id(&first)),
                Err(Error::ContextMismatch)
            ));
            forged = auth.clone();
            forged.credential = "login-2".into();
            assert!(matches!(
                runtime.authorize(&forged, "a", id(&first)),
                Err(Error::ContextMismatch)
            ));
            assert!(matches!(
                runtime.authorize(&auth, "b", id(&first)),
                Err(Error::ContextMismatch)
            ));
            assert!(matches!(
                runtime.negotiate(auth.clone(), request("unknown")).await,
                Err(Error::Denied)
            ));
            let restarted = Runtime::new(config(schema), ()).unwrap();
            assert!(matches!(
                restarted.authorize(&auth, "a", id(&first)),
                Err(Error::ContextMismatch)
            ));
        });
    }

    #[test]
    fn invalidation_fences_pending_resolution_and_allows_fresh_negotiation() {
        run(async {
            for target in [
                Invalidation::Credential("login-1"),
                Invalidation::IdentityDatabase {
                    identity: "alice",
                    database: "a",
                },
                Invalidation::Database("a"),
            ] {
                let schema = schema();
                let conn = connection().await;
                let release = AtomicBool::new(true);
                let runtime = Runtime::new(
                    config(schema.clone()),
                    |_: AuthenticatedRequest, database: String| {
                        let resolved = resolution(schema.clone(), conn.clone(), database);
                        let release = &release;
                        async move {
                            std::future::poll_fn(|_| {
                                if release.load(Ordering::SeqCst) {
                                    Poll::Ready(())
                                } else {
                                    Poll::Pending
                                }
                            })
                            .await;
                            Ok(resolved)
                        }
                    },
                )
                .unwrap();
                let auth = auth();
                let existing = runtime.negotiate(auth.clone(), request("a")).await.unwrap();
                let unaffected = runtime.negotiate(auth.clone(), request("b")).await.unwrap();
                let mut other_login = auth.clone();
                other_login.credential = "login-2".into();
                let other_tab = runtime
                    .negotiate(other_login.clone(), request("a"))
                    .await
                    .unwrap();
                let scope = runtime.authorize(&auth, "a", id(&existing)).unwrap();
                release.store(false, Ordering::SeqCst);
                let mut negotiation = Box::pin(runtime.negotiate(auth.clone(), request("a")));
                pending(negotiation.as_mut()).await;
                runtime.invalidate(target).unwrap();
                assert!(matches!(
                    scope.query("write", json!({})).await,
                    Err(Error::ContextMismatch)
                ));
                assert!(matches!(
                    scope.catchup(&HashMap::new(), 10, None).await,
                    Err(Error::ContextMismatch)
                ));
                assert!(matches!(
                    runtime.authorize(&auth, "a", id(&existing)),
                    Err(Error::ContextMismatch)
                ));
                if !matches!(target, Invalidation::Credential(_)) {
                    assert!(runtime.authorize(&auth, "b", id(&unaffected)).is_ok());
                    assert!(matches!(
                        runtime.authorize(&other_login, "a", id(&other_tab)),
                        Err(Error::ContextMismatch)
                    ));
                } else {
                    assert!(matches!(
                        runtime.authorize(&auth, "b", id(&unaffected)),
                        Err(Error::ContextMismatch)
                    ));
                    assert!(runtime.authorize(&other_login, "a", id(&other_tab)).is_ok());
                }
                release.store(true, Ordering::SeqCst);
                assert!(matches!(negotiation.await, Err(Error::ContextMismatch)));
                let fresh = runtime.negotiate(auth.clone(), request("a")).await.unwrap();
                assert_ne!(id(&existing), id(&fresh));
                runtime
                    .authorize(&auth, "a", id(&fresh))
                    .unwrap()
                    .query("write", json!({}))
                    .await
                    .unwrap();
                let connection = conn.lock().await;
                let mut rows = connection
                    .query("SELECT count(*) FROM writes", ())
                    .await
                    .unwrap();
                assert_eq!(
                    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
                    1
                );
            }
        });
    }

    #[test]
    fn projection_and_schema_rejections() {
        run(async {
            let schema = schema();
            let conn = connection().await;
            let mut invalid = config(schema.clone());
            invalid.client_fields.insert("undeclared".into());
            assert!(matches!(
                Runtime::new(invalid, ()),
                Err(Error::InvalidConfiguration)
            ));
            let mut invalid = config(schema.clone());
            invalid.client_fields.clear();
            assert!(matches!(
                Runtime::new(invalid, ()),
                Err(Error::InvalidConfiguration)
            ));
            let mut invalid = config(schema.clone());
            invalid.max_age = Duration::ZERO;
            assert!(matches!(
                Runtime::new(invalid, ()),
                Err(Error::InvalidConfiguration)
            ));
            let mut invalid = config(schema.clone());
            invalid.exposed_operations.insert("not-generated".into());
            assert!(matches!(
                Runtime::new(invalid, ()),
                Err(Error::InvalidConfiguration)
            ));
            for (session, fields) in [
                (json!({"role": 3, "secret": "s"}), vec!["role"]),
                (
                    json!({"role": "Admin", "secret": "s"}),
                    vec!["role", "optional"],
                ),
                (
                    json!({"role": "Admin", "secret": "s", "optional": 9007199254740992_i64}),
                    vec!["role", "optional"],
                ),
            ] {
                let mut cfg = config(schema.clone());
                cfg.client_fields = fields.into_iter().map(str::to_string).collect();
                let runtime = Runtime::new(cfg, |_: AuthenticatedRequest, database: String| {
                    let mut result = resolution(schema.clone(), conn.clone(), database);
                    result.session = session.clone();
                    std::future::ready(Ok(result))
                })
                .unwrap();
                assert!(matches!(
                    runtime.negotiate(auth(), request("a")).await,
                    Err(Error::InvalidResolution)
                ));
                assert!(runtime.registry.lock().unwrap().entries.is_empty());
            }
            for wrong_database in [false, true] {
                let runtime = Runtime::new(
                    config(schema.clone()),
                    |_: AuthenticatedRequest, database: String| {
                        let mut result = resolution(schema.clone(), conn.clone(), database);
                        if wrong_database {
                            result.database.database_id = "b".into();
                        } else {
                            result.database.schema = Arc::new(SchemaArtifact {
                                schema_id: schema.schema_id.clone(),
                                manifest: schema.manifest.clone(),
                                context: schema.context.clone(),
                            });
                        }
                        std::future::ready(Ok(result))
                    },
                )
                .unwrap();
                assert!(matches!(
                    runtime.negotiate(auth(), request("a")).await,
                    Err(Error::InvalidResolution | Error::Denied)
                ));
            }
        });
    }

    #[test]
    fn expiry_bounds_and_resolver_time_are_not_renewed() {
        run(async {
            let schema = schema();
            let conn = connection().await;
            let called = AtomicBool::new(false);
            let runtime = Runtime::new(
                config(schema.clone()),
                |_: AuthenticatedRequest, database: String| {
                    called.store(true, Ordering::SeqCst);
                    std::future::ready(Ok(resolution(schema.clone(), conn.clone(), database)))
                },
            )
            .unwrap();
            let mut expired = auth();
            expired.expires_at = SystemTime::now();
            assert!(matches!(
                runtime.negotiate(expired.clone(), request("a")).await,
                Err(Error::Unauthenticated)
            ));
            assert!(!called.load(Ordering::SeqCst));
            assert!(matches!(
                runtime.authorize(&expired, "a", "unknown"),
                Err(Error::Unauthenticated)
            ));
            for mode in 0..3 {
                let before = SystemTime::now();
                let deadline = before + Duration::from_secs(5);
                let mut cfg = config(schema.clone());
                if mode == 0 {
                    cfg.max_age = Duration::from_secs(5);
                }
                let mut auth = auth();
                if mode == 1 {
                    auth.expires_at = deadline;
                }
                let runtime = Runtime::new(cfg, |_: AuthenticatedRequest, database: String| {
                    let mut result = resolution(schema.clone(), conn.clone(), database);
                    if mode == 2 {
                        result.access_deadline = Some(deadline);
                    }
                    std::future::ready(Ok(result))
                })
                .unwrap();
                let message = runtime.negotiate(auth, request("a")).await.unwrap();
                let ContextMessage::Context { expires_at, .. } = message else {
                    unreachable!()
                };
                let expiry = UNIX_EPOCH + Duration::from_millis(expires_at);
                assert!(
                    expiry
                        <= if mode == 0 {
                            SystemTime::now() + Duration::from_secs(5)
                        } else {
                            deadline
                        }
                );
                assert!(expiry > before);
            }
            for mode in 0..3 {
                let mut cfg = config(schema.clone());
                if mode == 0 {
                    cfg.max_age = Duration::from_secs(2);
                }
                let deadline = SystemTime::now() + Duration::from_secs(2);
                let mut auth = auth();
                if mode == 1 {
                    auth.expires_at = deadline;
                }
                let release = AtomicBool::new(false);
                let runtime = Runtime::new(cfg, |_: AuthenticatedRequest, database: String| {
                    let mut result = resolution(schema.clone(), conn.clone(), database);
                    if mode == 2 {
                        result.access_deadline = Some(deadline);
                    }
                    let release = &release;
                    async move {
                        std::future::poll_fn(|_| {
                            if release.load(Ordering::SeqCst) {
                                Poll::Ready(())
                            } else {
                                Poll::Pending
                            }
                        })
                        .await;
                        Ok(result)
                    }
                })
                .unwrap();
                let mut negotiation = Box::pin(runtime.negotiate(auth, request("a")));
                pending(negotiation.as_mut()).await;
                std::thread::sleep(Duration::from_secs(3));
                release.store(true, Ordering::SeqCst);
                assert!(matches!(negotiation.await, Err(Error::ContextMismatch)));
                assert!(runtime.registry.lock().unwrap().entries.is_empty());
            }
        });
    }

    #[test]
    fn clock_rollback_and_post_dispatch_results_fail_closed() {
        run(async {
            let wall = SystemTime::now();
            let monotonic = Instant::now();
            let lease = Lease::new(wall + Duration::from_secs(1), wall, monotonic).unwrap();
            assert!(lease.valid_at(wall, monotonic));
            assert!(!lease.valid_at(
                wall - Duration::from_secs(30),
                monotonic + Duration::from_secs(1)
            ));
            assert!(!lease.valid_at(wall + Duration::from_secs(1), monotonic));

            let schema = schema();
            let conn = connection().await;
            let runtime = Runtime::new(
                config(schema.clone()),
                |_: AuthenticatedRequest, database: String| {
                    std::future::ready(Ok(resolution(schema.clone(), conn.clone(), database)))
                },
            )
            .unwrap();
            let auth = auth();
            let message = runtime.negotiate(auth.clone(), request("a")).await.unwrap();
            let scope = runtime.authorize(&auth, "a", id(&message)).unwrap();
            let mut expiring = runtime.authorize(&auth, "a", id(&message)).unwrap();
            expiring.request_lease.monotonic = Instant::now();
            assert!(matches!(
                expiring.query("write", json!({})).await,
                Err(Error::ContextMismatch)
            ));
            assert!(matches!(
                expiring.catchup(&HashMap::new(), 10, None).await,
                Err(Error::ContextMismatch)
            ));
            // Exercise the exact completion gate after real dispatched/committed SQL.
            // Local libSQL may complete without yielding, so the barrier is placed at
            // the gate rather than relying on a database scheduling race.
            let connection = scope.entry.database.connection.lock().await;
            let result = query::run(
                &connection,
                &schema.manifest,
                "write",
                json!({}),
                &scope.entry.session,
            )
            .await
            .unwrap();
            runtime.invalidate(Invalidation::Database("a")).unwrap();
            assert!(matches!(
                scope.finish(Ok(result.response), true),
                Err(Error::MutationOutcomeUnknown)
            ));
            assert!(matches!(
                scope.finish(Ok(json!({})), false),
                Err(Error::StaleResult)
            ));
            assert!(matches!(
                scope.finish::<Value>(Err(Error::Unavailable), true),
                Err(Error::MutationOutcomeUnknown)
            ));
            let mut rows = connection
                .query("SELECT count(*) FROM writes", ())
                .await
                .unwrap();
            assert_eq!(
                rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
                1
            );
        });
    }
}
