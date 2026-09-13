# Pyre Elm Client

A headless Elm application for Pyre data synchronization and querying. This client manages data in memory and communicates with IndexedDB and SSE via TypeScript ports.

## Architecture

- **Elm (`src/`)**: Manages in-memory state, executes queries, handles deltas, and sends mutations
- **TypeScript (`src-ts/`)**: Boots the Elm app and wires IndexedDB/SSE/query manager services
- See [`docs/CLIENT_DATA_FLOW.md`](docs/CLIENT_DATA_FLOW.md) for the end-to-end client data flow and delta shapes.
- See [`docs/TABLE_ENTITY_STREAM.md`](docs/TABLE_ENTITY_STREAM.md) for a draft lower-level table/entity stream API.

## Setup

1. Install dependencies:
```bash
npm install
```

2. Build Elm:
```bash
npm run build
# or for development:
npm run dev
```

3. Build TypeScript:
```bash
npm run typecheck
```

## Usage

Start with the [sync guide](../../docs/usage/sync.md) for authentication, app-owned database selection, and local query boundaries. See [Elm integration](../../docs/usage/elm-sync.md) for generated UI wiring and [multi-database integration](../../docs/usage/multi-database-upgrade.md) when adding source databases.

Use one `PyreClient` per schema family in your browser app. A `databaseId` selects a source database within that family, not a different schema.

### Local Edits

For receipt-based writes, opt in through `ServerConfig.localEdits(databaseId)` and
use the [local-edit guide](../../docs/usage/local-edits.md) for complete configuration
and fixture-backed TypeScript/Elm examples. Bind generated TypeScript builders with
`await client.localEdits(databaseId, Main)`, then call `db.submit(editOrBatch)`
synchronously. Import schema-specific `batch` and `Commands` from generated
`typescript/edits/<Namespace>`, not the low-level client helpers.

Generated Elm uses `Db.<Namespace>.Edit.<Record>` opaque patches, nullable `Maybe`
setters and required create records with optional `createWith` setters. Store the
`( Model, Effect, Receipt )` from `Pyre.submit`/`Pyre.batch` and forward effects in
order through the bridge; lifecycle/failure events return on `pyre_receiveQueryDelta`.
The default compiler namespace is TypeScript `Main`, Elm `Db.Database.Default`,
and runtime `_default`. Typed database IDs select instances, not authorization.

Install `db.onEditFailure` even when ignoring receipts. Acceptance proves commit;
confirmation also requires authoritative coverage. Pending writes are memory-only,
not a durable outbox, and writes are never automatically retried. JSON/unions are
whole-value replacements and may overwrite concurrent member changes. Prediction
is conservative; one unpredictable member disables optimism for the entire batch.
Generated CRUD does not enforce invariants encoded only in named commands; keep
those writes on their command (MEC-117 follow-up).

The initialization below shows legacy transport configuration, not fenced local
edits. Cross-runtime release conformance remains subject to verification.

### Initialization

```typescript
import { PyreClient } from '@pyre/client';
import { schemaMetadata } from './generated/typescript/core/schema';

const bootstrap = await fetch('/bootstrap', { credentials: 'include' })
  .then((response) => response.json());

const client = await PyreClient.create({
  schema: schemaMetadata,
  server: {
    baseUrl: window.location.origin,
    endpoints: {
      catchup: '/sync',
      events: '/sync/events',
      query: '/db',
    },
    credentials: 'include',
  },
  indexedDbName: 'pyre-client',
  cacheNamespace: bootstrap.userId,
  debug: true,
});

await client.setSyncedDatabases([bootstrap.mainDatabaseId]);

// Set `debug: true` to enable verbose runtime logging. By default,
// `@pyre/client` does not emit its internal `console.log` diagnostics.

const unsubscribeSync = client.onSyncState((syncState) => {
  console.log(syncState.status); // "not_started" | "catching_up" | "live"
  // "live" means initial catchup is complete and current queries have been fulfilled
  console.log(syncState.tables); // Record<tableName, "waiting" | "catching_up" | "live">
});

// Optional legacy callback (derived from sync state)
client.onSyncProgress((progress) => {
  console.log(progress.complete);
});
```

### Server auth configuration

`@pyre/client` is auth-neutral. Configure the HTTP behavior your app needs and let your server build the Pyre session from its normal authenticated request context.

Cookie-authenticated APIs should use `credentials: 'include'`:

```typescript
const client = await PyreClient.create({
  schema: schemaMetadata,
  cacheNamespace: userId,
  server: {
    baseUrl: 'https://api.example.com',
    credentials: 'include',
  },
});
```

For HTTP catchup and mutation requests, bearer tokens, API keys, and CSRF headers can use static headers. These headers are not sent by native browser SSE; use cookie authentication for that live transport.

```typescript
const client = await PyreClient.create({
  schema: schemaMetadata,
  cacheNamespace: userId,
  server: {
    baseUrl: 'https://api.example.com',
    credentials: 'include',
    headers: {
      Authorization: `Bearer ${token}`,
      'X-CSRF-Token': csrfToken,
    },
  },
});
```

Use dynamic headers for rotating tokens:

```typescript
const client = await PyreClient.create({
  schema: schemaMetadata,
  cacheNamespace: userId,
  server: {
    baseUrl: 'https://api.example.com',
    credentials: 'include',
    headers: async () => ({
      Authorization: `Bearer ${await getAccessToken()}`,
      'X-CSRF-Token': getCsrfToken(),
    }),
  },
});
```

`credentials` accepts the standard fetch values: `'omit'`, `'same-origin'`, or `'include'`. The older `withCredentials: true` option is equivalent to `credentials: 'include'`.

Custom headers are applied to HTTP catchup and mutation requests. Native browser `EventSource` does not support custom headers. Same-origin cookies are sent normally; use `credentials: 'include'` for cross-origin cookie-authenticated SSE, with the corresponding server CORS and cookie configuration.

### Registering Queries

```typescript
const subscription = await client.run(
  bootstrap.mainDatabaseId,
  ListUsersAndPosts,
  {},
  (result) => {
    console.log('Query result:', result);
  }
);

subscription?.update({});
subscription?.unsubscribe();
```

### Entity Change Streams

Use `onEntityChanges` when you want table rows directly instead of a query-shaped
result tree. Legacy mode starts with `indexeddb-initial`. Fenced local edits start
from the worker's visible state with source `local-edits`, never IndexedDB, and
publish authoritative replacement plus eligible pending intent together.

```typescript
const posts = new Map<string | number, unknown>();

const unsubscribe = await client.onEntityChanges(
  'main',
  {
    tables: [
      { tableName: 'posts', where: { author_id: currentUserId } },
      { tableName: 'comments', where: { post_id: { $in: visiblePostIds } } },
    ],
  },
  (batch) => {
    for (const change of batch.changes) {
      if (change.tableName === 'posts') {
        if (change.op === 'remove') posts.delete(change.id);
        else posts.set(change.id, change.row);
      }
    }

    renderPosts([...posts.values()]);
  }
);

unsubscribe();
```

Legacy entity streams emit current rows:

- `source: 'indexeddb-initial'` for the initial persisted snapshot
- `source: 'catchup'` or `source: 'live'` for incoming server deltas
- `op: 'row'` for every change
- no delete events, previous values, field-level diffs, or membership-left events

Fenced streams also emit `{ tableName, id, op: 'remove' }` for deletes, rejected
creates, filter exits and replacement omissions. Removal events have no row payload.

If filter inputs change, unsubscribe and create a new subscription.

`QueryShape` supports:

- selected fields
- `@where`
- `@sort`
- `@limit`

Generated query shapes use `{"$var":"fieldName"}` placeholders in `@where` for ordinary query inputs. `PyreClient` resolves these inputs before sending the query to the internal Elm query engine.

Local query inputs filter already-authorized data; they do not grant permissions. Keep `Session` on the server. See the [sync guide](../../docs/usage/sync.md) for the local query boundary and the [query reference](../../docs/usage/query.md) for explicit server execution.

### Updating Query Input

```typescript
subscription?.update({});
```

### Attaching An Elm Bridge

If your app already has Elm ports for Pyre messages, you can let `@pyre/client` own the bridge wiring:

```typescript
const bootstrap = await fetch('/bootstrap', { credentials: 'include' })
  .then((response) => response.json());

const client = await PyreClient.create({
  schema: schemaMetadata,
  cacheNamespace: bootstrap.userId,
  server: {
    baseUrl: window.location.origin,
    credentials: 'include',
    liveSyncTransport: 'sse',
    endpoints: {
      catchup: '/sync',
      events: '/sync/events',
      query: '/db',
    },
  },
  elm: {
    app,
    onError: (error, context) => {
      console.error(context.phase, error);
    },
  },
});

await client.setSyncedDatabases([bootstrap.mainDatabaseId]);
```

`PyreClient.create(...)` automatically attaches the bridge when `elm` is provided.

If you need lower-level control, `client.attachElmBridge(...)` is still available.

Default ports:

- receive: `pyreStoreOut`
- query results: `pyre_receiveQueryDelta`
- entity stream results: `pyre_receiveEntityChanges`
- sync state: `pyre_receiveSyncState`
- mutation results: `pyre_receiveMutationResult`

Pass port names only if your app uses different names.

This built-in bridge handles:

- `register`
- `update-input`
- `unregister`
- `mutate`
- `register-entity-stream`
- `unregister-entity-stream`
- forwarding revisioned query results back into Elm
- forwarding entity stream batches back into Elm with `streamId`
- sending mutation requests to the server automatically
- forwarding mutation results back into Elm with `requestId`
- forwarding sync state back into Elm

Provide `elm.onMutation` only when you need to override that default mutation behavior.

### Sending Mutations

Named calls remain supported. In fenced mode, `run` uses the same ordered worker
queue as generated edits, without inferred optimism. Its callback receives
`{ ok: true, value }` only on confirmation; other outcomes have `ok: false`, an
outcome-kind `error`, and a structured `outcome`. Do not treat every callback as
success or every failure as proof of rollback. Use generated `Commands` plus
receipts to observe acceptance and late settlement; subscribe to `onEditFailure`
independently. See [migration details](../../docs/usage/local-edits.md#migrating-named-calls).

```typescript
await client.run(bootstrap.mainDatabaseId, CreatePost, { title: 'Hello' }, (result) => {
  console.log('Mutation result:', result);
});
```

For generated Elm mutation modules, send the generated request payload through your outbound port:

```elm
import Db.Database


port pyreStoreOut : Encode.Value -> Cmd msg


sendCreatePost : Db.Database.DatabaseId Db.Database.Main -> Cmd msg
sendCreatePost databaseId =
    pyreStoreOut
        (Query.CreatePost.mutationRequest databaseId "create-post-1"
            { title = "Hello" }
        )
```

In legacy mode, `PyreClient` posts that mutation to the configured query endpoint
and publishes the result to `pyre_receiveMutationResult`. Fenced named calls use
the local-edit queue; new Elm edit effects instead return lifecycle events through
`pyre_receiveQueryDelta`.

This example assumes `CreatePost` belongs to the `Main` schema namespace. Construct its typed database ID from your app's bootstrap string with `Db.Database.fromString`; use the generated namespace for your own schema.

## Ports

### Outgoing (Elm -> TypeScript)

- `requestInitialData`: Request all data from IndexedDB
- `writeDelta`: Write a delta to IndexedDB
- `connectSSE`: Connect to SSE endpoint
- `disconnectSSE`: Disconnect from SSE
- `queryResult`: Send query results (callbackPort, result)
- `mutationResult`: Send mutation results (`requestId`, `mutationId`, result)
- `syncStateOut`: High-level sync state (`status`, `tables`)

### Incoming (TypeScript -> Elm)

- `receiveInitialData`: Receive all data from IndexedDB
- `receiveDelta`: Receive a synced delta
- `receiveSyncProgress`: Receive sync progress updates
- `receiveSyncComplete`: Receive sync complete notification
- `receiveSSEConnected`: Receive SSE connection confirmation
- `receiveSSEError`: Receive SSE error
- `receiveRegisterQuery`: Register a new query (queryId, queryShape, input)
- `receiveUpdateQueryInput`: Update query input (queryId, queryShape, newInput)
- `receiveUnregisterQuery`: Unregister a query (queryId)
- `receiveSendMutation`: Send a mutation (`requestId`, `mutationId`, baseUrl, input)

## Generated Elm integration

When using generated Elm query code, the intended setup is:

1. Keep `Pyre.Model` inside your Elm application model
2. Route `Pyre.Msg` through your app update function
3. Forward `Pyre.Send` payloads to your JS/TS host
4. Let `PyreClient` execute/register/update those queries
5. Send results and deltas back into Elm and decode them with `Pyre.decodeIncomingDelta`

Generated `Pyre.elm` already uses the generated `Query.*.queryShape` values when registering and updating queries, so application code does not need to look up metadata by query name.

Generated Elm mutation modules expose:

- `id`
- `name`
- `mutationRequest : DatabaseId Main -> RequestId -> Input -> Encode.Value` (for a mutation in `Main`)
- `decodeMutationResult : Decode.Decoder MutationResult`

That lets Elm send a fully-specified mutation request with a server-defined `databaseId` and caller-owned `requestId`, while `PyreClient` handles the HTTP request and live sync remains the read path.

## Features

- ✅ In-memory data management
- ✅ Query execution against local state
- ✅ Automatic query re-execution on data changes
- ✅ Delta application and persistence
- ✅ HTTP mutation support
- ✅ SSE connection management
- ✅ IndexedDB persistence

## Limitations

- Query results with nested relationships are simplified (full JSON encoding needed)
- `$in` operator in filters needs proper list handling
- Dynamic port creation not supported (uses single queryResult port with routing)
