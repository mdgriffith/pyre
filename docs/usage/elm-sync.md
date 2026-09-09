# Elm + Sync Runtime Setup

Continue here after [Sync Setup](./sync.md) to wire an Elm app to `PyreClient` through TypeScript ports. The server authentication, database selection, and transport model are the same; this guide focuses on generated Elm APIs and the bridge.

## Mental model

There are three layers:

1. **Elm app**
   - Owns UI state.
   - Owns concrete database ID construction, such as `"main"` or `"campaign:123"`.
   - Registers/unregisters queries.
   - Receives query results/deltas.

2. **TypeScript bridge**
   - Hosts `PyreClient` from `@pyre/client`.
   - Usually provides a single `connect` bootstrap hook.
   - Lets `PyreClient` attach the Elm bridge.
   - Usually does not need a custom mutation handler.

3. **Server sync runtime**
   - `@pyre/server/sync` routes (`/sync`, `/sync/events`, query route).
   - Computes catchup and live deltas.

Elm should not reimplement sync transport details. Keep transport/stateful runtime concerns in the TS bridge.

## Client runtime setup

Create one `PyreClient` per schema family in the browser app, with separate generated artifacts and runtime wiring for independently generated schemas. `databaseId` selects a database within that family, not a schema. See [Sync Setup](./sync.md#4-run-a-pyre-backed-server) for shared server requirements.

With `app` as the initialized Elm application and the user already authenticated, use one application-owned bootstrap to configure the client and attach its built-in bridge:

```ts
let mainDatabaseId = "main"

const client = await PyreClient.create({
  schema: schemaMetadata,
  indexedDbName: "my-app-pyre",
  debug: true,
  connect: async () => {
    const response = await fetch("http://localhost:3000/bootstrap", {
      credentials: "include",
    })

    const bootstrap = await response.json()
    mainDatabaseId = bootstrap.mainDatabaseId
    const { userId } = bootstrap

    return {
      server: {
        baseUrl: "http://localhost:3000",
        credentials: "include",
        liveSyncTransport: "sse",
        endpoints: {
          catchup: "/sync",
          events: "/sync/events",
          query: "/db",
        },
      },
      cacheNamespace: userId,
    }
  },
  onError: (error) => console.error(error),
  elm: {
    app,
    onError: (error) => console.error(error),
  },
});

await client.setSyncedDatabases([mainDatabaseId])
```

Set `debug: true` if you want verbose runtime logging while debugging sync behavior. Leave it off in normal app usage.

The client does not hold the effective server session. Update ordinary query inputs through the generated Elm query API; see [Local Queries And Session](./query.md#local-queries-and-session) for the local execution boundary.

Bootstrap is application-owned. Accessible IDs and the active sync set are independent; awaiting selection schedules sync rather than waiting for data. See [Select Databases To Sync](./sync.md#select-databases-to-sync) for additive and replacement selection.

Use `client.run(databaseId, queryModule, input, callback)` for TypeScript-native consumers. For generated Elm clients, prefer `PyreClient.create({ connect, elm: { ... } })` so the runtime owns the port bridge.

## Elm database IDs

Pyre generates `Db.Database.elm` with an opaque typed database ID:

```elm
type DatabaseId namespace
    = DatabaseId String

fromString : String -> DatabaseId namespace
toString : DatabaseId namespace -> String
```

Pyre also generates namespace marker types from schema namespaces. For example, schemas named `Main` and `Campaign` produce `Db.Database.Main` and `Db.Database.Campaign`.

The app should define concrete constructors in one place:

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

Generated query and mutation constructors require the matching namespace:

```elm
Pyre.QueryUpdate
    (Pyre.GameKeystone (App.Database.campaign gameId) queryId input)

Query.GameUpdate.mutationRequest
    (App.Database.campaign gameId)
    requestId
    input
```

The JSON sent through ports still contains a plain string `databaseId`. The type parameter only prevents accidentally sending a `Main` database ID to a `Campaign` query, or vice versa.

## Sync State And Bridge Ports

For cookies, headers, SSE limitations, and CORS, use the shared [Transport Authentication](./sync.md#transport-authentication) guidance.

Use `client.onSyncState(...)` for high-level sync lifecycle updates:

```ts
const unsubscribeSync = client.onSyncState((syncState) => {
  // "not_started" | "catching_up" | "live"
  if (syncState.status === "live") {
    // Initial catchup is complete, live sync is active,
    // and currently registered queries have been fulfilled
  }

  // Per-table status: "waiting" | "catching_up" | "live"
  console.log(syncState.tables)
})
```

`SyncState.error` is optional and reported separately from lifecycle transitions.

Default Elm bridge ports:

- outbound from Elm: `pyreStoreOut`
- inbound query results: `pyre_receiveQueryDelta`
- inbound sync state: `pyre_receiveSyncState`
- inbound mutation results: `pyre_receiveMutationResult`

Override port names only if your app uses different names.

## Elm port contract (recommended)

Elm → TS:

- `register`
- `update-input`
- `unregister`
- `mutate`

Generated `Pyre` returns effects as data:

```elm
type Effect
    = NoEffect
    | Send Encode.Value
    | LogError Encode.Value
```

The host app should map `Send`/`LogError` to its own outgoing ports.

For standard writes, prefer the generated mutation modules in `Query.*`.

Pyre generates default CRUD mutations for writable tables:

- `{Table}Create`
- `{Table}Update`
- `{Table}Delete`

That means Elm app code can usually initiate writes through generated modules like `Query.DocumentCreate`, `Query.DocumentUpdate`, and `Query.DocumentDelete` without authoring custom mutation queries first.

Reach for a handwritten mutation query only when the write is not simple CRUD, such as nested inserts or other custom write behavior.

Generated update mutation modules use `Db.Updates` for nullable update fields so Elm can distinguish:

- set a value
- leave the field unchanged
- set the field to `null`

`Db.Updates` exposes:

```elm
type Update a
    = Set a
    | Unchanged
    | SetToNull


set : a -> Update a
skip : Update a
null : Update a
object : List ( String, Update Encode.Value ) -> Encode.Value
```

Example update input for a generated `DocumentUpdate` mutation:

```elm
import Db.Updates


{ id = documentId
, description = Db.Updates.set "Updated description"
}
```

For generated update inputs:

- `Db.Updates.set value` sends the field with that value
- `Db.Updates.skip` omits the field from the encoded mutation input
- `Db.Updates.null` sends the field as JSON `null`

This is what allows single-column updates from Elm without conflating `null` and "unchanged".

Notes:

- Use generated `Pyre.elm` and `Query.*` modules as the Elm API surface.
- Let `PyreClient` handle the bridge protocol; app code should not construct register or mutate payloads by hand.
- Generated query shapes preserve filters, sorting, and limits automatically.

TS → Elm:

- Forward incoming query data to the generated `Pyre.decodeIncomingDelta` path.
- Forward incoming mutation results to the generated mutation module decoders.

Generated mutation modules expose `mutationRequest databaseId requestId input` and `decodeMutationResult`, so Elm can initiate mutations and handle results without needing to know the bridge payload format.

The `elm` configuration in the client setup above attaches the built-in bridge and executes standard mutations without custom host handlers.

## Things that are easy to miss

1. **CORS headers for custom headers**
   - If you send custom request headers, include them in `Access-Control-Allow-Headers`.
   - If you use `credentials: "include"`, configure CORS to allow credentials and use an explicit allowed origin.

2. **Cache authorization lifecycle**
   - The app owns cache policy; sync selection does not guarantee permission-contraction cleanup. See [Sync Setup](./sync.md#select-databases-to-sync).

3. **Fail loudly on decode/contract mismatches**
   - Log query id/source and decode error details. Silent drops make sync debugging very hard.

4. **One source of truth for query identity**
   - Use generated `Pyre.elm` and `Query.*` modules. The host app should not look up TS metadata by query name manually.

## Troubleshooting checklist

- `/sync` returns 500
  - Check server logs first.
  - Verify `Sync.init()` and `Sync.loadSchemaFromDatabase(db)` ran.
  - Verify schema/migrations are current.

- Query route works but sync/catchup fails
  - Usually schema cache/runtime setup issue on server side.

- Elm shows default/empty state forever
  - Confirm TS bridge receives runtime callback.
  - Confirm bridge sends to Elm inbound port.
  - Confirm Elm decoder accepts payload shape.

- Live events never arrive
  - Confirm `/sync/events` connection stays open.
  - Confirm connected clients map is populated and used by `Sync.run(...).sync(...)`.
