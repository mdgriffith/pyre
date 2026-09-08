# @pyre/server

Server runtime helpers for executing generated Pyre queries.

Typical usage:

- import generated `queries` map from `pyre/generated/typescript/server`
- initialize schema-specific databases through the generated `databases` map
- execute with `run` from `@pyre/server/query`
- seed fixture data with the generated `seed` helper from `pyre/generated/typescript/seed`
- use sync helpers from `@pyre/server/sync` and `@pyre/server/query-sync`

## Database Provisioning

Generated output embeds each namespaced schema and binds it to the transactional
`ensureDatabase` runtime helper:

```ts
import { createClient } from "@libsql/client";
import { init } from "@pyre/server/wasm";
import { databases } from "./pyre/generated/typescript/server";

await init();

const main = createClient({ url: "file:main.db" });
const campaign = createClient({ url: "file:campaign-123.db" });

await databases.Main.ensureDatabase(main);
await databases.Campaign.ensureDatabase(campaign);
```

The call returns `"created"`, `"migrated"`, or `"up-to-date"`. It is safe to
call whenever a database is opened: introspection, planning, DDL, and migration
recording happen in one write transaction. Pyre rejects non-empty databases
that do not already contain Pyre migration metadata.

## Seed Data

Generated server output includes a schema-bound `seed` helper for server-side fixtures and imports:

```ts
import { createClient } from "@libsql/client";
import { seed } from "./pyre/generated/typescript/seed";

const db = createClient({ url: "file:test.db" });

const result = await seed(db, {
  users: [
    {
      name: "Fred",
      posts: [
        { title: "example post", content: "My content!" },
        { title: "example post2", content: "My content!" },
      ],
    },
  ],
});
```

Top-level keys are table names. Nested keys must be links declared on the parent table; Pyre derives foreign keys from the link metadata. You can also seed flattened layers by setting foreign key columns directly.

The seed call is atomic: if any row fails validation or insertion, Pyre rolls back the transaction. The returned data contains the full inserted rows, including nested rows.

Seed bypasses Pyre query permissions. Installed SQLite sync triggers maintain
durable revision metadata, but the host must publish a scoped `syncRequired`
notification if connected clients should refresh immediately after an import.

## Durable Sync

Install/migrate the database with the current generated schema, rebuild WASM,
and regenerate clients together. Sync requests require
`syncCursor: { version: 2, tables: ... }`, plus `databaseEpoch` when resuming a
persisted cursor. Older clients are rejected rather than silently missing deletes.

Use `run` from `@pyre/server/sync` (or `runWithSync` from
`@pyre/server/query-sync`) for sync mutations, then call
`result.sync(sendToSession)`. Authenticate and authorize live connections before
registering them, and give each entry a server-selected `databaseId`. Broadcasts
exclude unscoped connections and connections for another physical database.

Ordinary mutations send direct `delta` payloads, including durable deletion IDs
and permission-filtered upserts. They do not require an HTTP catchup round trip.
`syncRequired` is reserved for oversized payloads, imports, and explicit refresh.
Catchup uses `updatedAt`/primary-key pagination and a separate tombstone sequence.
The client registers its stream before catchup, retains the round-start boundary
for pending wakes, and replays tombstones to restore absent-key observations.
Persisted timestamps are capped by a writer-barrier clock fence, separately from
page cursors. Buffered deltas reconcile against keys each page actually observed.
No current-row revision table or upsert history is retained.

`rotateDatabaseEpoch(db)` is an explicit admin retention operation. It rotates
the epoch, clears tombstones, and resets the global revision counter in one
write transaction. Choose its cadence yourself; Pyre never rotates automatically.
Broadcast a scoped wake afterward for immediate refresh of connected clients.

See [the protocol and migration notes](../../docs/durable-deletion-sync.md).

## Install

```bash
bun add @pyre/server zod@^4
```

Pyre-generated TypeScript and `@pyre/server` support Zod 4. Zod 3 is not supported.

## Sync Lifecycle Profiling

Run a local in-memory profile:

```bash
bun run profile:sync
```

Run the same profile against Turso:

```bash
TURSO_DATABASE_URL=libsql://... \
TURSO_AUTH_TOKEN=... \
SYNC_PROFILE_ALLOW_REMOTE_WRITES=1 \
bun run profile:sync
```

Useful knobs:

- `SYNC_PROFILE_ROWS`, default `1000`
- `SYNC_PROFILE_PAGE_SIZE`, default `1000`
- `SYNC_PROFILE_ITERATIONS`, default `10`
- `SYNC_PROFILE_SESSIONS`, default `25`
- `SYNC_PROFILE_MIMIC_RTT_MS`, default `20`
- `SYNC_PROFILE_MIMIC_BANDWIDTH_MBPS`, default `25`

The profile creates an isolated `pyre_sync_profile_notes` table and reports total time, average time, and percentage by phase for catch-up and mutation-to-delta sync.
It also compares row-materialized catch-up with a SQLite aggregate JSON catch-up shape and prints a simple remote mimic estimate from measured DB payload bytes.
