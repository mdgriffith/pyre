# Sync Setup

This guide assumes you already know the basic Pyre workflow from [Getting Started](./getting-started.md): schema, database, queries, `pyre check`, and `pyre generate`.

Use this guide when you want:

- a Pyre-backed server
- live sync on the client
- generated Elm query modules that work with `@pyre/client`

If your main goal is Elm port wiring, also see [Elm + Sync Runtime Setup](./elm-sync.md).

## Quick Start

The shortest sync path looks like this:

1. Add a `session { ... }` block and at least one session-aware query.
2. Apply the schema to a database with `pyre migrate db/app.db --push`.
3. Run `pyre generate`.
4. Start a Pyre-backed server that exposes `/sync`, `/sync/events`, and `/db`.
5. Create a `PyreClient` in your browser app.
6. Register queries and keep session state current.

The rest of this guide walks through those steps.

For browser sync, add `@pyre/client` from the same GitHub Release used for `@pyre/core` and `@pyre/server` in [Getting Started](./getting-started.md):

```json
{
  "dependencies": {
    "@pyre/client": "https://github.com/mdgriffith/pyre/releases/download/version-0.1.5/pyre-client-0.1.5.tgz"
  },
  "overrides": {
    "@pyre/core": "https://github.com/mdgriffith/pyre/releases/download/version-0.1.5/pyre-core-0.1.5.tgz"
  }
}
```

## 1. Define Session-Aware Schema And Queries

Create `pyre/schema.pyre` and a query file such as `pyre/query.pyre`:

```pyre
session {
    userId Int
}

record User {
    @public

    id Int @id
    ownerId Int
    name String
}

query GetUser($id: Int) {
    user {
        @where { id == $id && ownerId == Session.userId }

        id
        name
    }
}
```

Why the session matters:

- Pyre validates session values against the schema
- session values can participate in query filters
- sync visibility and query results can depend on session data

## 2. Apply The Schema To A Database

For a local sync prototype, use direct push:

```bash
pyre migrate db/app.db --push
```

For checked-in migration files instead of direct push, see [Migration Guide](./migrations.md).

## 3. Generate Artifacts

```bash
pyre generate
```

Generated output includes:

```text
pyre/generated/
├── client/
│   └── elm/
│       ├── Pyre.elm
│       └── Query/
└── typescript/
    ├── core/
    ├── server.ts
    └── run.ts
```

The important pieces for sync are:

- `typescript/core/`: schema metadata and query metadata
- `client/elm/`: generated Elm sync/query surface
- `typescript/server.ts`: query metadata consumed by the server sync runtime

## 4. Run A Pyre-Backed Server

You need a server that exposes the standard Pyre endpoints:

```text
POST /sync
GET  /sync/events
POST /db/:queryId
```

You have two common options:

### Option A: Use `pyre serve`

This is the fastest way to get a working server:

```bash
pyre serve db/app.db --dev-session '{"userId":1}'
```

See [pyre-serve.md](./pyre-serve.md) for operational details.

### Option B: Use Your Own Server

Use the generated server target to run queries against your database inside your own app server:

```typescript
import { createClient } from '@libsql/client';
import * as Sync from '@pyre/server/sync';
import { queries } from './pyre/generated/typescript/server';

const db = createClient({
  url: 'file:./db/app.db',
});

await Sync.init();
await Sync.loadSchemaFromDatabase(db);

// These values normally come from the authenticated request and route.
const queryId = request.params.queryId;
const args = await request.json();
const session = { userId: authenticatedUser.id };
const databaseId = 'main';

const result = await Sync.run(
  db,
  queries,
  queryId,
  args,
  session,
  connectionsForDatabase(databaseId),
  databaseId,
);

if (result.kind === 'error') {
  throw new Error(result.error?.message ?? 'Query execution failed');
}

await result.sync((sessionId, message) => {
  sendPyreSyncMessage(databaseId, sessionId, message);
});

return result.response;
```

`queryId` is the generated interface ID sent by the client, not a source-level name such as `GetUser`. If you are building your own sync server, it must authenticate requests normally, construct the Pyre session object, and keep live-sync connections partitioned by database.

## 5. Create A `PyreClient`

`PyreClient` manages:

- IndexedDB-backed local cache
- catchup sync
- live sync transport
- query registration and refresh
- session-aware query re-evaluation

Typical setup:

```typescript
import { PyreClient } from '@pyre/client';
import { schemaMetadata } from './pyre/generated/typescript/core/schema';

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
  },
  session: {
    userId: bootstrap.userId,
  },
  cacheNamespace: bootstrap.userId,
});

await client.setSyncedDatabases([bootstrap.mainDatabaseId]);
```

If session values change later, refresh them so active queries are re-evaluated correctly:

```typescript
client.setSession({ userId: 2 });
```

### Optional Devtools

The browser devtools UI is exposed from a separate entry point so production bundles can avoid including it:

```typescript
if (import.meta.env.DEV) {
  const { mountPyreDevtools } = await import('@pyre/client/devtools');
  mountPyreDevtools(client);
}
```

## 6. Connect Elm

Your Elm app owns the generated `Pyre.Model` and routes generated effects through ports.

Typical model shape:

```elm
type alias Model =
    { pyre : Pyre.Model
    }
```

Initialize it with:

```elm
init : flags -> ( Model, Cmd Msg )
init _ =
    ( { pyre = Pyre.init }
    , Cmd.none
    )
```

Use generated queries through `Pyre.QueryUpdate`:

```elm
type Msg
    = PyreMsg Pyre.Msg
    | PyreEffectHandled Encode.Value


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        PyreMsg pyreMsg ->
            let
                ( newPyre, effect ) =
                    Pyre.update pyreMsg model.pyre
            in
            ( { model | pyre = newPyre }
            , handlePyreEffect effect
            )

        PyreEffectHandled _ ->
            ( model, Cmd.none )
```

Register or update a query with:

```elm
PyreMsg
    (Pyre.QueryUpdate
        (Pyre.GetUser databaseId "user-1" { id = 1 })
    )
```

Read the current result with:

```elm
Pyre.getResult "user-1" model.pyre.getUser
```

For the full Elm bridge model, typed database IDs, and port wiring details, continue with [Elm + Sync Runtime Setup](./elm-sync.md).

## 7. Mental Model

Information moves through the system in three main paths:

- Startup: `PyreClient` restores cached state from IndexedDB, then performs server catchup.
- Live sync: the server pushes deltas over `/sync/events`, and `PyreClient` applies and persists them.
- Query and mutation flow: the app registers queries or sends mutations, the server responds, and sync updates the local read model.

## 8. Sync Data Flow

```mermaid
flowchart TD
    Elm[Elm app\nUI state + generated Pyre module]
    Bridge[TypeScript host / bridge\nports + app session]
    Cache[(IndexedDB\nlocal persistence)]
    Catchup[/POST /sync\ncatchup/]
    Events[/GET /sync/events\nSSE live stream/]
    Query[/POST /db\nqueries + mutations/]
    Server[Pyre server sync runtime\npermissions + delta generation]

    subgraph Client [PyreClient]
        QueryClient[Query client\nquery shape + session resolution]
        QueryManager[Query manager\ncallbacks + query state bridge]
        ElmRuntime[Internal Elm runtime\nDb + QueryManager + Catchup + LiveSync]
        IndexedDbService[IndexedDB service\nport bridge to browser storage]
        Transport[Live sync transport\nSSE / WebSocket manager]

        QueryClient -->|register / update query| QueryManager
        QueryManager -->|query manager ports| ElmRuntime
        ElmRuntime -->|request initial data / write delta| IndexedDbService
        IndexedDbService -->|initial cached rows| ElmRuntime
        ElmRuntime -->|connect / receive live messages| Transport
    end

    Elm -->|register / update / unregister query| Bridge
    Bridge -->|forward generated payloads| QueryClient
    IndexedDbService -->|read / write rows| Cache
    Cache -->|stored rows| IndexedDbService
    ElmRuntime -->|catchup request + session + cursor| Catchup
    Transport -->|open live connection| Events
    ElmRuntime -->|run query / mutation| Query

    Catchup --> Server
    Events --> Server
    Query --> Server

    Server -->|catchup rows| Catchup
    Server -->|live deltas| Events
    Server -->|query / mutation result| Query

    Catchup -->|sync payload| ElmRuntime
    Events -->|delta stream| Transport
    Query -->|result| ElmRuntime

    QueryManager -->|updated query results| Bridge
    ElmRuntime -->|sync state| Bridge
    Bridge -->|ports into Elm| Elm
```

## 9. Notes

- Generated Elm query modules expose `queryShape`.
- Generated Elm mutation modules expose `id`, `name`, `mutationRequest`, and `decodeMutationResult`.
- Generated Elm query and mutation constructors require typed database IDs by schema namespace.
- `Pyre.elm` uses generated `queryShape` values automatically.
- `@where`, `@sort`, and `@limit` are preserved in generated query shapes.
- Session-aware filters require keeping `PyreClient` session state current via `setSession`.
