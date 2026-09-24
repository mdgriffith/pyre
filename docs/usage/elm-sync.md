# Elm + Sync Runtime Setup

Continue here after [Sync Setup](./sync.md) to wire an Elm app to `PyreClient` through TypeScript ports. The server authentication, database selection, and transport model are the same; this guide focuses on generated Elm APIs and the bridge.

## Mental model

There are three layers:

1. **Elm app**
   - Owns UI state.
   - Owns concrete database ID construction, such as `"main"` or `"campaign:123"`.
   - Registers/unregisters queries.
   - Receives query results/deltas.
   - Constructs opaque edits, explicitly submits them, and handles typed receipts.

2. **TypeScript bridge**
   - Hosts `PyreClient` from `@pyre/client`.
   - Usually provides a single `connect` bootstrap hook.
   - Lets `PyreClient` attach the Elm bridge.
   - Usually does not need a custom mutation handler.

3. **Server sync runtime**
   - `@pyre/server/sync` routes (`/sync`, `/sync/events`, query route).
   - Computes catchup and live deltas.

The bridge connects the app to one worker engine that owns authoritative rows, optimistic intent, and reconciliation. Query and entity readers share its visible state. Elm should not reimplement transport or maintain a separate optimistic cache.

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
- `submit`

Generated `Pyre` returns effects as data:

```elm
type Effect
    = NoEffect
    | Send Encode.Value
    | QueryUpdated String QueryId
    | LogError Encode.Value
```

The host app should send `Send` through `pyreStoreOut`, handle `LogError`, and react to `QueryUpdated databaseId queryId` when needed. Generated query storage is keyed by database instance and query ID. Read it with `Pyre.getResult databaseId queryId model.<queryField>`; custom bridges must preserve `databaseId` on incoming results. Equal query IDs in different database instances are independent.

## Composed Writes And Typed Receipts

For standard CRUD, prefer `Db.Edit.<Record>` builders. For example, given a writable `Document` record with `id Id.Uuid @id`, `title String`, and `summary String?`:

```elm
import Db.Edit
import Db.Edit.Document as Document


save databaseId requestId documentId =
    Db.Edit.submit databaseId requestId
        [ Document.update documentId
            [ Document.title "New title"
            , Document.summary Nothing
            ]
        ]


create databaseId requestId =
    Db.Edit.submit databaseId requestId
        [ Document.create
            { title = "New document" }
            [ Document.withSummary (Just "Summary") ]
        ]
```

Send the returned value with `pyreStoreOut (save databaseId requestId documentId)` from the app's update function. Keep a unique request ID for each in-flight submission and retain its expected database ID and operation order in your model. Use the schema-derived `Db.Id` value for `documentId` (for an external UUID, `Db.Id.uuid uuidString`). Do not invent raw wire descriptors.

Builders are pure and record-specific. Required create inputs are a record; optional create fields use `withField` builders. For nullable updates, omission leaves a field unchanged, `Nothing` clears it, and `Just value` sets it. `Patch` and `CreateOption` constructors are opaque; generated names may gain underscore suffixes to avoid collisions, so consult the generated module. JSON/union fields replace whole logical values.

All edits in one `Db.Edit.submit` execute in order in one server transaction, targeting one typed namespace and concrete database. The bridge captures a UUIDv7 for each create once before worker dispatch; do not provide a UUID primary key in create input. There are no intra-batch generated-ID references. Use a successful create result for a later dependent submission.

Subscribe to the mutation-result port and decode a completion against the database and request retained in your model:

```elm
-- In a port module:
port pyre_receiveMutationResult : (Decode.Value -> msg) -> Sub msg


decodeSave databaseId requestId wire =
    Db.Edit.receive databaseId requestId wire
        |> Result.andThen (Document.updateResult 0)
```

Import `Json.Decode as Decode` for the port type. Wire the subscription to an application message (for example `pyre_receiveMutationResult Received`). In its update branch, handle `Ok returnData` and `Err message`; a mismatched database/request is a decode error, not another request's completion. For a create use `Document.createResult index`; for a delete use `Document.deleteResult index`. Accessors check the operation index and compiled query identity, and decode its generated return type.

Submission installs supported prediction in the existing worker; ordinary query publications update the UI. Do not apply the receipt as a second cache patch. Handle bridge dispatch errors through the configured `elm.onError` callback as well as mutation-result failures. Pending edits are memory-only, and unknown outcomes must not be automatically retried. See [Sync Outcomes And Recovery](./sync.md#shared-visible-state-and-outcomes).

The repository's `tests/fixtures/ComposedExample.elm` demonstrates the complete model/effect/port path and database-scoped query storage; its generated-browser lifecycle is exercised by `cargo test --test crud_builders`.

## Named Mutation Compatibility

Generated mutation modules in `Query.*` remain available for individual commands and existing integrations.

Pyre generates default CRUD mutations for writable tables:

- `{Table}Create`
- `{Table}Update`
- `{Table}Delete`

These are the compiled operations used by the CRUD builders. Existing code can still call `Query.DocumentCreate`, `Query.DocumentUpdate`, and `Query.DocumentDelete` directly; unlike the builder path, direct create inputs must include any required identity, and generated UUID creates require canonical UUIDv7.

Reach for a handwritten mutation query only when the write is not simple CRUD, such as nested inserts or other custom write behavior.

The lower-level generated `Query.*` update inputs use `Db.Updates` for omittable fields so Elm can distinguish:

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

- Use generated `Pyre.elm`, `Db.Edit`, `Db.Edit.<Record>`, and `Query.*` modules as the Elm API surface. Do not import `Db.Internal.Edit` in application code.
- Let `PyreClient` handle the bridge protocol; app code should not construct register or mutate payloads by hand.
- Generated query shapes preserve filters, sorting, and limits automatically.

TS → Elm:

- Forward incoming query data to the generated `Pyre.decodeIncomingDelta` path.
- Decode composed completions through `Db.Edit.receive` and record-specific result accessors; use generated mutation module decoders for individual `Query.*` commands.

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
