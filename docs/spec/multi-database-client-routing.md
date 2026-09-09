# Multi-Database Client Routing

## Goal

Support apps where one app-facing Pyre client may query, mutate, and sync against different source databases over the same server endpoints.

The app explicitly provides the database ID for each query or mutation and controls which database IDs are active for sync. Pyre carries those routing decisions through requests and keeps local cache state isolated per source database.

## Non-Goals

- Pyre does not interpret app concepts like campaign, tenant, workspace, or shard.
- Pyre does not put routing metadata inside delta table payloads. Sync message envelopes do include `databaseId` so the client can route the message to the correct cache.
- Pyre does not require server-side auth session storage.
- Pyre does not multiplex rows from multiple source databases into one IndexedDB cache.

## Terms

- **Database ID**: An opaque server-defined string identifying a source database, such as `main`, `campaign:123`, or `tenant:acme`.
- **Database Namespace**: A Pyre schema namespace, such as `Main` or `Campaign`, used by generated metadata to describe which database family a query expects.
- **Cache Namespace**: A server-defined string identifying the authenticated local-cache boundary, usually the user ID.
- **Source Database**: The server-side SQLite/libSQL database selected by a database ID.
- **Cache Database**: The browser IndexedDB database used by Pyre for one source database.
- **Active Sync Database**: A database ID currently being caught up and listened to for live deltas. Multiple databases may be active at once.
- **Request Target Database**: The database ID selected for a specific query or mutation request.
- **Meta-Client**: The app-facing Pyre client. It routes requests and sync messages to internal single-database clients.
- **Internal Database Client**: A Pyre client instance responsible for one database ID, one cache database, one sync cursor, and one live sync lifecycle.

## Core Rules

1. A Pyre IndexedDB cache is one-to-one with a source database.
2. Query and mutation routing is independent from live sync routing.
3. The app controls which database IDs are actively synced.
4. Every query and mutation request must include a target database ID.
5. The server validates that the authenticated user may access the requested database ID.
6. The server fans out live deltas only to connections registered for the same database ID.
7. Sync responses and live sync messages include the database ID they came from so the client can write them to the correct cache database.
8. The app should not manage one Pyre client per database. Pyre owns any internal per-database clients needed to keep the implementation simple.

These rules apply to multiple source databases that share one Pyre schema. If an app has distinct schema families, such as a command-plane schema and a tenant schema, each schema family needs separate generated artifacts and separate client/server runtime wiring unless Pyre has explicit multi-schema-family support.

## Client API Shape

The client requires the target database ID at the operation call site:

```ts
type DatabaseId = string;
type CacheNamespace = string;

client.run(databaseId, queryModule, input, callback);
```

Pyre should reject query or mutation requests that do not include a database ID.

The client should expose sync controls:

```ts
client.syncDatabase(databaseId);
client.unsyncDatabase(databaseId);
client.setSyncedDatabases(databaseIds);
```

The app is the source of truth for the active sync set. Pyre should start and stop catchup/live sync work to match that set.

The public client is a meta-client. Internally, Pyre may create one single-database client per database ID so existing one-database assumptions remain local to those internal clients.

The server should provide the cache namespace during app bootstrap or connection setup. The most common cache namespace is the authenticated user ID.

```ts
const client = await PyreClient.create({
  schema,
  server,
  session,
  cacheNamespace: bootstrap.userId,
});
```

## Request Transport

Pyre sends the selected database ID with every server request. Catchup uses a POST body so cursor state is not placed in URLs; live sync and mutation routes still use URL routing where appropriate.

```text
POST /sync
GET  /sync/events?databaseId=campaign%3A123
POST /db/CreateNote?databaseId=campaign%3A123
```

Catchup body:

```json
{
  "databaseId": "campaign:123",
  "syncCursor": {
    "tables": {}
  }
}
```

`databaseId` is used instead of `database` because the value is an identifier, not a database object or connection.

The value is opaque to Pyre. Pyre only encodes and transmits it.

Server responses that contain sync data also identify their source database.

Catchup response:

```json
{
  "databaseId": "campaign:123",
  "tables": {},
  "has_more": false
}
```

Live delta message:

```json
{
  "type": "delta",
  "databaseId": "campaign:123",
  "data": []
}
```

The database ID is message routing metadata. It is not part of the delta payload itself.

## Local Cache Isolation

Each source database needs a distinct IndexedDB backing store within the authenticated cache namespace.

Pyre derives cache database names from the server-defined cache namespace and database ID:

```ts
indexedDbName: `${baseIndexedDbName}:${safe(cacheNamespace)}:${safe(databaseId)}`
```

The app may configure the base IndexedDB name, but the cache namespace and database ID should come from the server. The resulting IndexedDB name should remain readable in browser devtools so developers can identify which cache belongs to which user and source database.

Pyre should safely encode names before using them in IndexedDB, but it should prefer a readable encoding over a hash-only name.

Examples:

```text
pyre-client:user_42:main
pyre-client:user_42:campaign_123
```

If a cache namespace is omitted in an authenticated app, Pyre may reuse local data across users who share a browser profile. Multi-database apps should treat `cacheNamespace` as required.

The app-facing client may sync multiple databases concurrently. Pyre should manage one internal cache service or internal single-database client per database ID so the app does not need to create separate public clients.

Each inbound sync message carries a database ID, and Pyre uses that ID to select the IndexedDB backing store before applying the catchup page or live delta.

## Query And Mutation Routing

The app must pass the target database ID when issuing each query or mutation.

Example:

```ts
const client = await PyreClient.create({
  schema,
  server,
  session,
  cacheNamespace: bootstrap.userId,
});

client.run(
  campaignDatabaseId,
  Query.CampaignNotes,
  { limit: 50 },
  (result) => {
    render(result);
  }
);
```

The literal database ID is request routing metadata. It is not part of the Pyre query input unless the query itself needs that value for filtering or permissions.

Mutations follow the same rule:

```ts
client.run(
  campaignDatabaseId,
  Query.CreateCampaignNote,
  { body: "Hello" },
  (result) => {
    handleMutationResult(result);
  }
);
```

The generated operation metadata may still describe the database type an operation expects, but the literal database ID comes from the app at call time. Valid database IDs are defined by the server and usually come from bootstrap data, query results, or another server-owned source of truth.

## Elm App Routing

Elm integrations must also provide the database ID for every query and mutation request.

Generated Elm helpers require a `databaseId` argument and include it in the generated request value sent through ports. The concrete database ID string belongs to the app, but generated Elm types preserve the query's database namespace.

Generated support module shape:

```elm
type DatabaseId namespace
    = DatabaseId String

fromString : String -> DatabaseId namespace
toString : DatabaseId namespace -> String
```

Apps should define one source of truth for concrete IDs:

```elm
module App.Database exposing (main, campaign)

import Db.Database
import Pyre


main : Pyre.DatabaseId Pyre.Main
main =
    Db.Database.fromString "main"


campaign : Int -> Pyre.DatabaseId Pyre.Campaign
campaign campaignId =
    Db.Database.fromString ("campaign:" ++ String.fromInt campaignId)
```

Generated query and mutation constructors then require the matching namespace:

```elm
Pyre.QueryUpdate
    (Pyre.CampaignNotes (App.Database.campaign campaignId) queryId input)

Query.CreateCampaignNote.mutationRequest
    (App.Database.campaign campaignId)
    requestId
    input
```

The TypeScript bridge forwards that database ID to the meta-client:

```ts
client.run(message.databaseId, queryModule, message.queryInput, callback);
```

Elm-originated messages that omit `databaseId` should fail before reaching the server.

If a generated operation targets `Main`, Elm should not compile when passed `DatabaseId Campaign`; if it targets `Campaign`, Elm should not compile when passed `DatabaseId Main`. Pyre still treats the serialized database ID as opaque at runtime.

## Sync Routing

The app explicitly chooses which database IDs are synced. Pyre should not infer active sync databases from query history, mutation history, or session contents.

Example:

```ts
client.setSyncedDatabases([
  "main",
  campaignDatabaseId,
]);
```

The order is meaningful. Pyre should use the list order as the default catchup priority.

Changing the active sync set should:

1. start catchup for newly added database IDs,
2. open live sync for newly added database IDs,
3. close live sync for removed database IDs,
4. keep independent cursor and cache state for each database ID,
5. route each catchup page and live delta to the cache named by its database ID,
6. refresh registered queries that depend on changed local sync state.

Convenience methods may be provided:

```ts
client.syncDatabase(campaignDatabaseId);
client.unsyncDatabase(oldCampaignDatabaseId);
```

Each active database gets independent catchup cursor state, live connection state, and IndexedDB backing state.

Pyre controls catchup scheduling. The default should be serialized catchup: if the app activates multiple databases, Pyre catches up one database first, then proceeds to the next. This avoids request bursts and IndexedDB write contention.

By default, catchup priority follows the order passed to `setSyncedDatabases`. For example, `setSyncedDatabases(["main", campaignDatabaseId])` catches up `main` first, then the campaign database. Apps can put the active campaign first if that should be prioritized for the current screen.

The implementation can later allow limited parallel catchup, but the default model should not start one catchup loop per active database immediately.

The initial live sync model should use one live connection per active database. This keeps each internal database client isolated and avoids multiplexing complexity. Each live message still includes `databaseId` so the client can validate and debug routing.

## Internal Architecture

The app-facing client should behave like one client, but Pyre may implement it as a meta-client over internal single-database clients.

```text
PyreClient
  routes queries/mutations by database ID
  owns active sync set
  schedules catchup work
  aggregates sync state
  manages internal clients

InternalDatabaseClient(databaseId)
  owns one IndexedDB cache
  owns one sync cursor
  owns one catchup lifecycle
  owns one live sync lifecycle
```

This keeps the existing single-database runtime model intact while giving apps a single client API.

## Server Contract

The app server owns database lookup, authorization, and connection grouping.

```ts
const { databaseId, syncCursor } = await request.json();
const session = authenticate(request);

authorize(session, databaseId);

const db = databaseFor(databaseId);
const connections = connectionsFor(databaseId);
const catchupResult = await Sync.catchup(db, syncCursor, session, 1000, databaseId);
```

For mutations:

```ts
const result = await Sync.run(
  db,
  queries,
  queryId,
  args,
  session,
  connections,
  databaseId
);

await result.sync((sessionId, message) => {
  connections.get(sessionId)?.send({
    ...message,
    databaseId,
  });
});
```

The server may derive allowed database IDs from a signed cookie, JWT, or other stateless session. The request still carries the target database ID because authorization and routing answer different questions.

The server is the source of truth for valid database IDs. The client chooses among server-defined IDs, but the server must still validate every request and live sync connection.

The server is also the source of truth for the cache namespace. In most apps this should be the authenticated user ID or another stable user/account cache identity.

## Multiple Schema Families

`databaseId` routes among source databases that have the same generated Pyre schema. It is not a schema selector.

For an app with both a command-plane schema and a tenant schema:

- Generate command-plane Pyre artifacts from the command-plane schema.
- Generate tenant Pyre artifacts from the tenant schema.
- Create one public `PyreClient` for command-plane work and one public `PyreClient` for tenant work, each using its matching `schemaMetadata`.
- Use distinct base IndexedDB names per schema family, such as `app-command-pyre` and `app-tenant-pyre`.
- Keep `cacheNamespace` server-defined and stable for the authenticated user/account.
- Route server requests by both schema family and `databaseId`; for example, `command:*` IDs use command-plane generated queries and DB connections, while `tenant:*` IDs use tenant generated queries and DB connections.
- Keep live-sync connection groups scoped to the selected `databaseId`.

Do not mix command-plane and tenant schemas in one `PyreClient` instance. A single client currently owns one `schemaMetadata` value and one generated query surface.
