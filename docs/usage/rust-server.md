# Rust Server Runtime

This guide is for wiring a Rust app server to Pyre using the native Rust server helpers instead of `@pyre/server`.

The app still runs `pyre generate`. Generated output includes:

- `pyre/generated/manifest.json`, which powers dynamic query and mutation execution in Rust
- `pyre/generated/rust/server.rs`, which exposes generated query ID constants and typed JSON boundary shapes for server-owned workflows
- `pyre/generated/rust/databases.rs`, which embeds one initializer per schema namespace
- `pyre/generated/rust/seed.rs`, which exposes typed schema-shaped fixture and import input

## Main Modules

```rust
pyre::server::manifest
pyre::server::database_id
pyre::server::query
pyre::server::schema
pyre::server::seed
pyre::server::sync
```

## Startup

Create or migrate each physical database with its schema-bound generated helper:

```rust
mod generated_databases {
    include!("../pyre/generated/rust/databases.rs");
}

generated_databases::main::ensure_database(&main_conn).await?;
generated_databases::campaign::ensure_database(&campaign_conn).await?;
```

The helper returns `Created`, `Migrated`, or `UpToDate`. It performs
introspection, migration planning, DDL, and schema recording in one immediate
transaction and rejects non-empty databases that are not already managed by
Pyre.

Load the generated manifest:

```rust
use pyre::server::manifest::Manifest;

let manifest = Manifest::load("pyre/generated/manifest.json")?;
```

Load the Pyre schema/context from the database:

```rust
use pyre::server::schema::load_schema_from_database;

let loaded_schema = load_schema_from_database(&conn).await?;
let context = loaded_schema.context()?;
```

Create the sync server:

```rust
use pyre::server::sync::SyncServer;

let sync_server = SyncServer::new(context);
```

## Session

Create a session from the logical app session shape defined in the Pyre schema:

```rust
use pyre::server::manifest::PyreSession;
use serde_json::json;

let session = PyreSession::new(
    json!({
        "userId": 1,
        "role": "admin"
    }),
    &manifest.session_schema,
)?;
```

`PyreSession` provides two views:

- `session.logical()` for sync permission checks
- `session.sql_args()` for query and mutation SQL execution

## Running Queries And Mutations

For generic dynamic execution, pass the query ID and JSON input directly:

```rust
use pyre::server::query;
use serde_json::json;

let result = query::run(
    &conn,
    &manifest,
    query_id,
    json!({ "body": "hello" }),
    &session,
).await?;
```

`result.response` contains the query or mutation response JSON.

`result.affected_rows` contains mutation affected rows for live sync.

## Generated Rust Server Metadata

`pyre generate` also emits `pyre/generated/rust/server.rs`. Include it from the app server crate:

```rust
mod pyre_generated {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/pyre/generated/rust/server.rs"));
}
```

The generated file expects these dependencies in the app server crate:

```toml
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_path_to_error = "0.1"
```

Generated query IDs are stable Rust names:

```rust
use pyre_generated::query_ids;

let result = query::run(
    &conn,
    &manifest,
    query_ids::GET_GAME,
    input,
    &session,
).await?;
```

If a query is renamed or deleted, references like `query_ids::GET_GAME` fail during `cargo check` instead of silently preserving a copied hash.

## Typed Server-Owned Workflows

For server-owned workflows, use the generated input and output aliases:

```rust
use pyre::server::query;
use pyre_generated::{query_ids, GetGameInput, GetGameOutput};

let result = query::run(
    &conn,
    &manifest,
    query_ids::GET_GAME,
    GetGameInput { id: game_id }.into_json(),
    &session,
).await?;

let output = GetGameOutput::try_from(result.response)?;
```

Input structs encode to `serde_json::Value` with `into_json()`. Output structs decode from `serde_json::Value` using `serde_path_to_error`, so malformed response JSON fails with a field path and the underlying serde error.

Omittable nullable inputs use `OptionalField<T>` so omitted and explicit `null` remain distinct:

```rust
use pyre_generated::OptionalField;

UpdateAssetInput {
    name: Some("logo".to_string()),
    description: OptionalField::Null,
}
```

The manifest runtime still validates dynamic input and remains the final fail-loud boundary before SQL execution.

## Initial Data Imports

For fixed-schema ingestion tools, include the generated seed module and construct its typed input:

```rust
mod pyre_seed {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/pyre/generated/rust/seed.rs"));
}

let input = pyre_seed::SeedInput {
    documents: Some(vec![pyre_seed::SeedDocumentsRow {
        title: Some(title),
        pages: Some(parsed_pages),
        ..Default::default()
    }]),
    ..Default::default()
};

let result = pyre_seed::seed(&conn, input).await?;
```

Call the generated database `ensure_database` helper before seeding. Seed insertion is atomic, derives foreign keys for nested links, and supports SQLite defaults, JSON, custom types, and DateTime values. It intentionally bypasses query permissions and sync metadata, so use it for initial database construction rather than application mutations.

Multi-namespace projects receive one seed module per physical database, matching the module layout in `databases.rs`. For example, use `pyre_seed::main::seed` with the Main connection and `pyre_seed::campaign::seed` with the Campaign connection.

## Catchup Endpoint

For a `POST /sync` equivalent:

```rust
use pyre::server::database_id::require_database_id;

let body: CatchupBody = request.json().await?;
let database_id = require_database_id(body.database_id)?;
let conn = database_for(&database_id).await?;

let sync_result = sync_server
    .catchup(
        &conn,
        &body.sync_cursor,
        session.logical(),
        1000,
        &database_id,
    )
    .await?;
```

Return `sync_result` as JSON. It includes `databaseId` so the browser runtime can route the catchup page to the matching local cache.

## Composed Operations

The existing manifest executor accepts ordered descriptors. The application supplies the authorized connection and authenticated `PyreSession`; clients supply compiled query IDs and inputs, never SQL or permission metadata.

```rust
use pyre::server::query::{self, OperationDescriptor};

let operations: Vec<OperationDescriptor> = serde_json::from_value(request_body)?;
let mut result = query::run_operations(
    &conn, &manifest, &operations, &session, true, // sync mode
).await?;
```

Alternatively pass `"$batch"` and the descriptor-array JSON to `query::run_sync`; use `query::run` for normal request/response mode. Operations target one namespace/database, execute in order in one transaction, and return indexed `{ index, queryId, result }` entries. Generated writes enforce exactly-one-row cardinality and generated UUID creates require canonical UUIDv7. Named commands retain their existing matching/empty-result behavior.

Execution failures roll back; `query::Error::OutcomeUnknown` means commit could have succeeded and must not be treated as a definite rejection or automatically retried. Map it to an HTTP server error for browser recovery. In sync mode, run delta calculation before returning the response as below.

## Live Deltas After Mutations

After running a mutation:

```rust
let mut result = query::run_sync(&conn, &manifest, query_id, input, &session).await?;

// Server-generated logical origin, independent of any client connectionId.
let origin_session_id = new_server_origin_id();
let mut recipients = connected_sessions.clone();
recipients.insert(origin_session_id.clone(), session.logical().clone());

let messages = sync_server.calculate_deltas(
    &conn,
    &mut result,
    &recipients,
    &database_id,
    Some(&origin_session_id),
).await?;
```

Send each message to its session:

```rust
for item in messages {
    send_to_session(item.session_id, item.message);
}
```

`new_server_origin_id` is application-owned and must return an ID unique among recipients. The synthetic origin supplies HTTP authority even without SSE; it is excluded from live fanout. Real live connections, including the caller's, keep their own permission-filtered messages. Do not derive origin permissions or suppress a peer using an untrusted `connectionId`. The built-in server uses this synthetic-origin approach. The worker fences duplicate HTTP/SSE authority by revision.

`run_sync` allocates the revision inside the write transaction. Delta calculation uses that committed revision and the original/final visibility of each affected row; it does not expose intermediate permission grants. Delivered removals carry identity-only tombstones. Return the wrapped `result.response` **after** calculating deltas, even with no live subscribers. Post-commit publication failure is not a rollback. For delivery-gap and exceptional recovery limits, see [Sync Setup](./sync.md#reconnect-and-recovery-boundaries).

`item.message` serializes as:

```json
{
  "type": "delta",
  "serverRevision": 12,
  "databaseId": "tenant:acme",
  "data": []
}
```

Use `pyre::server::database_id::require_database_id` at every Pyre endpoint boundary. The helper only validates presence/non-empty string; the app must still authenticate the request, authorize access to that `databaseId`, and map it to the correct database connection and schema family.

## Connected Sessions

Use `ConnectedSessions` for live delta permission filtering:

```rust
use pyre::server::sync::ConnectedSessions;

let connected_sessions: ConnectedSessions = /* session id -> logical session values */;
```

The concrete shape is:

```rust
HashMap<String, HashMap<String, pyre::sync::SessionValue>>
```

## Runtime Transformations

The Rust runtime handles these Pyre server transformations:

- JSON input stringification
- omittable `field__is_set` flags
- session SQL args as `session_<name>`
- per-statement SQL param filtering
- response formatting
- `_affectedRows` extraction

Do not reimplement these in the app server.

## Suggested Server Flow

1. Run `pyre generate` as part of the app build.
2. Load `pyre/generated/manifest.json` at server startup.
3. Load the Pyre schema context from the database.
4. Build `PyreSession` from the authenticated app session.
5. Include `pyre/generated/rust/server.rs` when the app has server-owned workflows.
6. Use generated `query_ids` and typed inputs/outputs for server-owned workflows.
7. Use `query::run` for request/response execution, or `query::run_sync` for synced operations (including `$batch`).
8. For synced writes, call `SyncServer::calculate_deltas` with the mutable result, database-scoped recipients and authenticated logical origin; then publish live messages using the revision already allocated in the write transaction.
9. Return `result.response` after publication/envelope preparation. Synced affected-row responses include `{ databaseEpoch, serverRevision, sync, result }` for the origin.
10. Use `SyncServer::catchup` for `/sync` catchup requests.

## Current Coverage

The Rust server helpers are covered by tests for:

- schema loading
- catchup sync
- live delta permission filtering
- generated insert/update/delete affected rows
- generated CRUD create/delete/update
- omitted vs explicit `null`
- JSON input serialization
- session argument binding
- manifest loading
- multi top-level query response formatting
- SQL parameter names with shared prefixes
- generated Rust query IDs and typed input/output boundary shapes
