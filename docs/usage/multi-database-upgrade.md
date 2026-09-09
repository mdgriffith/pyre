# Multi-Database Upgrade Guide

Use this guide when upgrading a Pyre client/server integration for multiple source databases. The client session API migration below also applies to single-database apps. For a new integration, start with [Sync Setup](./sync.md).

## Client Session API Migration

Remove `session` from client configuration and `connect` results (`connect.session`), and remove `setSession` calls. The browser no longer performs local `$session` substitution. Keep effective Pyre sessions on the server, constructed from ordinary authenticated requests; server sessions, schema permissions, and auth cookies still apply.

Replace explicit local `Session` filters with ordinary query inputs, or explicitly execute those queries on the authenticated server. See [Local Queries And Session](./query.md#local-queries-and-session) for examples and the rejection behavior. Permission-only schema usage remains compatible with local queries.

This change is required even when the app uses only one database. Supply its database ID through the application and select it for sync as described in [Sync Setup](./sync.md#select-databases-to-sync).

## Model

- `databaseId` is an opaque server-defined string, such as `command`, `tenant:acme`, or `tenant:42`.
- The client sends `databaseId` with every Pyre query, mutation, catchup request, and live-sync connection.
- The server authenticates the request, authorizes access to the requested `databaseId`, and maps that ID to the correct database connection.
- Pyre keeps one IndexedDB cache per `databaseId` inside the configured `cacheNamespace`.
- Generated Elm APIs type database IDs by Pyre schema namespace, but the app owns the concrete string format.

## Client Requirements

Create the client with server-provided bootstrap data:

```ts
const bootstrap = await fetch("/bootstrap", { credentials: "include" }).then((response) => response.json())

const client = await PyreClient.create({
  schema: schemaMetadata,
  cacheNamespace: bootstrap.cacheNamespace,
  server: {
    baseUrl: window.location.origin,
    credentials: "include",
    endpoints: {
      catchup: "/sync",
      events: "/sync/events",
      query: "/db",
    },
  },
})
```

Sync only the databases the app needs locally:

```ts
await client.setSyncedDatabases([
  bootstrap.commandDatabaseId,
  bootstrap.activeTenantDatabaseId,
])
```

The application supplies the accessible database list through its own bootstrap or other app-owned source; Pyre adds no enumeration or session routes. That list is independent of the active sync set selected with `setSyncedDatabases`. Neither list membership nor sync selection is a permission grant; the server still authorizes each request.

Use `syncDatabase(id)` to add to the active set instead of replacing it. Awaiting either selection method completes scheduling, not catchup or query readiness; see [Sync Setup](./sync.md#select-databases-to-sync).

Route every TypeScript query or mutation with a `databaseId`:

```ts
client.run(bootstrap.activeTenantDatabaseId, Query.ListDocuments, {}, (result) => {
  render(result)
})
```

Generated Elm query constructors include `databaseId` and `queryId`. The database ID is typed by the query's generated namespace:

```elm
Pyre.QueryUpdate
    (Pyre.ListDocuments tenantDatabaseId "documents" {})
```

Generated Elm mutation modules also require typed `databaseId`:

```elm
Query.DocumentCreate.mutationRequest tenantDatabaseId "create-document-1" input
```

Centralize app-specific database ID construction instead of concatenating strings at call sites:

```elm
module App.Database exposing (command, tenant)

import Db.Database
import Pyre


command : Pyre.DatabaseId Pyre.Command
command =
    Db.Database.fromString "command"


tenant : String -> Pyre.DatabaseId Pyre.Tenant
tenant tenantKey =
    Db.Database.fromString ("tenant:" ++ tenantKey)
```

The generated types prevent passing a `Command` database ID to a `Tenant` query, while the wire payload still sends the opaque string `databaseId`.

## Server Requirements

Every Pyre endpoint must require an authorized `databaseId`. Catchup receives it in the POST body with the sync cursor:

```ts
const { databaseId, syncCursor } = await request.json()
const session = authenticate(request)

authorizeDatabaseAccess(session, databaseId)

const db = databaseFor(databaseId)
const connectedClients = connectionsForDatabase(databaseId)
```

Catchup must pass `databaseId` through to Pyre:

```ts
const result = await Sync.catchup(db, syncCursor, session, 1000, databaseId)
return json(result)
```

The Rust server helper has the same shape:

```rust
let result = sync_server
    .catchup(&conn, &sync_cursor, session.logical(), 1000, &database_id)
    .await?;
```

Live sync connections must be stored by `databaseId` and session/client id:

```ts
connectSSEClient({ databaseId, sessionId, session, stream })
```

Queries and mutations must use the database-specific connection group:

```ts
const result = await Sync.run(
  db,
  queries,
  queryId,
  args,
  session,
  connectionsForDatabase(databaseId),
  databaseId
)

await result.sync((sessionId, message) => {
  sendPyreSyncMessage(databaseId, sessionId, message)
})
```

For Rust mutation deltas, pass the same `databaseId` to delta calculation:

```rust
let messages = sync_server.calculate_deltas(
    &conn,
    &mut result,
    connections_for_database(&database_id),
    &database_id,
    Some(origin_session_id),
).await?;
```

The server must never broadcast deltas across database IDs.

## IndexedDB Cache Migration

Multi-database clients derive each local IndexedDB name from the base IndexedDB name, `cacheNamespace`, and `databaseId`. If an app previously handcrafted names such as `my-app-campaign-123`, switching to `cacheNamespace + databaseId` will usually create different IndexedDB databases.

Choose one cache policy during the upgrade:

- Preserve caches by keeping the new derived names equivalent to the old names where possible.
- Migrate caches deliberately in app code before starting Pyre sync.
- Discard old caches and let Pyre catch up from the server.

Do not assume existing local caches will be reused automatically after changing the naming scheme.

Removing browser sessions does not clean up cached rows when permissions contract. Permission-contraction cache cleanup remains separate work, not a guarantee of this upgrade.

## Command Plane And Tenant Schemas

If the app has one command-plane schema and one tenant schema, treat them as different Pyre schema families.

The current `PyreClient.create` call accepts one `schemaMetadata`, and the current server sync runtime loads one schema into the WASM sync cache. That means a single client/runtime is appropriate only for databases that share the same Pyre schema.

Recommended upgrade shape:

- Generate Pyre artifacts separately for the command-plane schema and the tenant schema, usually by running `pyre generate` once per schema source/output directory.
- Use one `PyreClient` instance for command-plane operations and one `PyreClient` instance for tenant operations, each with its own generated `schemaMetadata`.
- Give each schema family a distinct base IndexedDB name, such as `my-app-command-pyre` and `my-app-tenant-pyre`.
- Keep `cacheNamespace` stable across both clients for the authenticated user/account.
- On the server, route `command:*` database IDs to command-plane generated queries and command-plane DB connections.
- On the server, route `tenant:*` database IDs to tenant generated queries and tenant DB connections.
- Load/use the correct Pyre server runtime schema for the selected schema family before running catchup or sync delta calculation.

If the generated metadata contains namespaces that are all loaded by the same runtime and all referenced queries can resolve their `primary_db`, routing by `databaseId` is safe inside that schema family. If those namespaces represent separate database connections with separate generated/runtime schemas, use separate artifact directories and separate client/server runtime wiring.

If Pyre is extended later to support multiple schema families inside one public client/runtime, that feature must route by both `schemaFamily` and `databaseId`. Until then, do not rely on one `schemaMetadata` value to select between independently loaded schema families.

## Handoff Checklist

- The app supplies `cacheNamespace` and accessible database IDs, not effective Pyre session data.
- Client passes `databaseId` to every query and mutation.
- Elm apps centralize concrete database ID constructors and pass generated typed IDs to query/mutation constructors.
- Client calls `setSyncedDatabases` with the databases that should sync locally.
- Elm-generated query and mutation calls include `databaseId`.
- Server rejects Pyre requests missing `databaseId`.
- Server authorizes every requested `databaseId`.
- Server maps each `databaseId` to the correct schema family and DB connection.
- Server groups live-sync connections by `databaseId`.
- Server calls `Sync.catchup(..., databaseId)` and `Sync.run(..., connectionsForDatabase(databaseId), databaseId)`.
- Command-plane and tenant schemas use separate generated artifacts and separate Pyre client/runtime wiring unless Pyre has explicit multi-schema support.
