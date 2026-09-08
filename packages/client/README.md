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

### Initialization

```typescript
import { PyreClient } from '@pyre/client';
import { schemaMetadata } from './generated/typescript/core/schema';

const bootstrap = await fetch('/bootstrap').then((response) => response.json());

const client = await PyreClient.create({
  schema: schemaMetadata,
  server: {
    baseUrl: 'http://localhost:3000',
    endpoints: {
      catchup: '/sync',
      events: '/sync/events',
      query: '/db',
    },
    headers: {
      Authorization: 'Bearer token',
    },
    credentials: 'same-origin',
  },
  indexedDbName: 'pyre-client',
  cacheNamespace: bootstrap.userId,
  debug: true,
  session: {
    userId: 1,
  },
});

await client.setSyncedDatabases([bootstrap.mainDatabaseId]);

// Or:
const readyClient = await PyreClient.create({
  schema: schemaMetadata,
  cacheNamespace: bootstrap.userId,
  debug: true,
  server: {
    baseUrl: 'http://localhost:3000',
    endpoints: {
      catchup: '/sync',
      events: '/sync/events',
      query: '/db',
    },
  },
});

// Set `debug: true` to enable verbose runtime logging. By default,
// `@pyre/client` does not emit its internal `console.log` diagnostics.

// Or resolve async setup during create. Auth is supplied through normal HTTP
// configuration; Pyre does not treat live-sync ids as credentials.
const factoryClient = await PyreClient.create({
  schema: schemaMetadata,
  connect: async () => {
    const response = await fetch('http://localhost:3000/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      credentials: 'include',
      body: JSON.stringify({ userId: 1 }),
    });

    const { userId } = await response.json();

    return {
      server: {
        baseUrl: 'http://localhost:3000',
        credentials: 'include',
        endpoints: {
          catchup: '/sync',
          events: '/sync/events',
          query: '/db',
        },
      },
      cacheNamespace: userId,
      session: { userId },
    };
  },
});

await factoryClient.setSyncedDatabases([bootstrap.mainDatabaseId]);

const unsubscribeSync = client.onSyncState((syncState) => {
  console.log(syncState.status); // "not_started" | "catching_up" | "live"
  // "live" means initial catchup is complete and current queries have been fulfilled
  console.log(syncState.tables); // Record<tableName, "waiting" | "catching_up" | "live">
});

// Optional legacy callback (derived from sync state)
client.onSyncProgress((progress) => {
  console.log(progress.complete);
});

// Refresh active queries when session-backed filters change
client.setSession({ userId: 2 });
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

Bearer tokens, API keys, and CSRF headers can use static headers:

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

Custom headers are applied to HTTP catchup and mutation requests. Native browser `EventSource` does not support custom headers, so SSE live sync can only use cookie credentials via `credentials: 'include'`.

### Registering Queries

```typescript
const subscription = client.run(
  'main',
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

Use `onEntityChanges` when you want table rows directly instead of a query-shaped result tree. The first callback is always an `indexeddb-initial` batch, even when no persisted rows match. Later authoritative callbacks follow committed catchup pages, including pages requested by live notifications.

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
        if (change.op === 'delete') {
          posts.delete(change.id);
        } else {
          posts.set(change.id, change.row);
        }
      }
    }

    renderPosts([...posts.values()]);
  }
);

unsubscribe();
```

Entity streams emit current rows and hard deletes:

- `source: 'indexeddb-initial'` for the initial persisted snapshot
- `source: 'catchup'` for persisted server pages, including live-triggered catchup
- `op: 'row'` with the current row, or `op: 'delete'` with only the typed ID
- delete IDs reach all subscriptions for that table, regardless of row filters; deleting an unknown key is a no-op
- no previous values, field-level diffs, or permission-revocation/membership-left events

Generated Elm streams expose typed deletion constructors such as `PostDeleted Posts.Id`.
Sync protocol 2 requires upgrading server/WASM/client artifacts together. Its IndexedDB
upgrade clears legacy row-only caches once; it does not rotate the server epoch.

If filter inputs change, unsubscribe and create a new subscription.

`QueryShape` supports:

- selected fields
- `@where`
- `@sort`
- `@limit`

Generated query shapes can contain placeholders in `@where`:

- `{"$var":"fieldName"}` for query input values
- `{"$session":"fieldName"}` for client session values

`PyreClient` resolves those placeholders before sending the query to the internal Elm query engine.

### Updating Query Input

```typescript
subscription?.update({});
```

### Attaching An Elm Bridge

If your app already has Elm ports for Pyre messages, you can let `@pyre/client` own the bridge wiring:

```typescript
const client = await PyreClient.create({
  schema: schemaMetadata,
  connect: async () => {
    const response = await fetch('http://localhost:3000/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      credentials: 'include',
      body: JSON.stringify({ userId: 1 }),
    });

    const { mainDatabaseId, userId } = await response.json();

    return {
      server: {
        baseUrl: 'http://localhost:3000',
        credentials: 'include',
        liveSyncTransport: 'sse',
        endpoints: {
          catchup: '/sync',
          events: '/sync/events',
          query: '/db',
        },
      },
      cacheNamespace: userId,
      session: { userId },
    };
  },
  elm: {
    app,
    onError: (error, context) => {
      console.error(context.phase, error);
    },
  },
});

await client.setSyncedDatabases([mainDatabaseId]);
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

```typescript
client.run('main', CreatePost, { title: 'Hello' }, (result) => {
  console.log('Mutation result:', result);
});
```

For generated Elm mutation modules, send the generated request payload through your outbound port:

```elm
port pyreStoreOut : Encode.Value -> Cmd msg


sendCreatePost : Cmd msg
sendCreatePost =
    pyreStoreOut
        (Query.CreatePost.mutationRequest "main" "create-post-1"
            { title = "Hello" }
        )
```

`PyreClient` will POST that mutation to the configured query endpoint and publish the result to `pyre_receiveMutationResult`.

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
- `mutationRequest : String -> String -> Input -> Encode.Value`
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
