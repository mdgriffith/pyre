# Seeding And Server-Owned Writes

Choose the path based on the write contract:

- **Application-authorized writes:** use generated builders with `executeOperations` under an explicit session. These obey compiled validation/permissions and can publish normal sync authority.
- **Trusted initial fixtures/imports:** use the generated `seed` helper. It connects nested records but bypasses query permissions and does not publish sync metadata.

## Explicit-Session Composed Operations

For a writable `Document` with UUID identity, `title String`, and `summary String?`:

```ts
import { executeOperations } from '@pyre/server/operations';
import { queries } from './pyre/generated/typescript/server';
import { Document, database } from './pyre/generated/typescript/core/edits';

const outcome = await executeOperations(
  authorizedConnection,
  queries,
  database('_default', databaseId),
  [Document.create({ title: 'Initial document', summary: null })],
  authenticatedSession,
  { mode: 'sync', sessions: connectedSessions, publish: sendToSession },
);
if (!outcome.ok) throw new Error(outcome.error.message);
const createdId = outcome.value[0].result.document[0].id;
```

The application must authorize `databaseId`, resolve it to `authorizedConnection`, and construct the explicit session (use `{}` only for a schema without session fields). A typed target is not an access token. Keep connected sessions and publication scoped to that database. Sync mode allocates transaction-time revisions even without active subscribers; `sessions` is optional, but supply a `publish` callback.

Use `{ mode: 'normal' }` for simple request/response execution without live publication. Both modes run one ordered transaction, enforce generated-write cardinality, and return typed indexed results. Construction is browser-independent; no worker, IndexedDB, or SSE connection is required. Local TypeScript execution requires file-backed libSQL.

Generated UUID creates capture UUIDv7 once and omit identity from input. Use the committed `createdId` in a later submission for dependent records; there is no intra-batch result binding. A definite execution rejection rolls back the batch. `OutcomeUnknown`, a publication exception, or a post-commit decoding exception must not trigger automatic replay. See [Operation Semantics](./query.md#generated-crud-and-composed-operations).

Rust applications use manifest descriptors with `query::run_operations`, or `$batch` through `query::run` / `query::run_sync`; see [Rust Server](./rust-server.md#composed-operations).

## Import-Oriented Seed Helper

The remaining examples describe the separate server-side `seed` helper. Their integer IDs assume a query-only schema declared with `@syncable(false)`. Use UUID primary keys and matching foreign-key values for synced schemas.

Seed data is shaped like the database schema. Top-level keys are table names, and nested keys are links declared in the Pyre schema.

```ts
import { createClient } from "@libsql/client";
import { init } from "@pyre/server/wasm";
import { seed } from "./pyre/generated/typescript/seed";

await init();
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

## How It Works

`seed` inserts each row and uses schema link metadata to connect nested records.

For example, if `users.posts` links `users.id` to `posts.authorId`, Pyre will:

1. Insert the user.
2. Read the inserted user's `id` from `RETURNING *`.
3. Insert each nested post with `authorId` set to that user id.
4. Return the inserted user and nested posts.

The returned shape follows the input shape and includes full inserted rows:

```ts
{
  kind: "success",
  response: {
    users: [
      {
        id: 1,
        name: "Fred",
        posts: [
          { id: 1, authorId: 1, title: "example post", content: "My content!" },
        ],
      },
    ],
  },
}
```

## Composing Fixture Facets

A facet is an ordinary fixture value that you define in code or JSON. For example, suppose `User` uses `@tablename("accounts")` and `Post` uses the physical table `posts`. A people facet can provide an explicit identity and a demo facet can refer to it directly in a foreign-key column:

```ts
import { seed, type SeedInput } from "./pyre/generated/typescript/seed";

const authorId = 41;
const people = { accounts: [{ id: authorId, name: "Fred" }] } satisfies SeedInput;
const demo = {
  accounts: [{ id: 42, name: "Mina" }],
  posts: [{ authorId, title: "example post", content: "My content!" }],
} satisfies SeedInput;
```

To insert both facets atomically, concatenate rows for every shared table key. A shallow spread alone would replace `people.accounts` with `demo.accounts`. If any row fails, such as when the second facet uses an ID already present in the first, the whole combined call rolls back on a confirmed failure.

```ts
const combined = { accounts: [...people.accounts, ...demo.accounts], posts: demo.posts };
const result = await seed(db, combined);
if (result.kind === "error") throw new Error(result.error?.message);
```

Alternatively, on a fresh target, call `seed` in dependency order. Each successful call commits independently. If `demo` fails, its own inserts roll back on a confirmed failure, and `people` stays committed.

```ts
const first = await seed(db, people);
if (first.kind === "error") throw new Error(first.error?.message);
const second = await seed(db, demo);
if (second.kind === "error") throw new Error(second.error?.message);
```

Rust uses the same approach: extend a table's `Vec` before a combined call, or await each `seed(&conn, facet)` in order. Use an explicit UUID value for UUID-backed identities and copy it directly to the corresponding foreign-key fields. Pyre does not discover facets, resolve symbolic references, or automatically order inter-facet dependencies; use separate calls when the order of flat inserts matters. Nested links are inserted in the order required to propagate returned keys.

## JSON And Custom Types

Seed input should use the normal serialized JavaScript shape for JSON and custom type fields. Constructed Pyre values use an `_type` discriminator. Pyre serializes or flattens the value for SQLite internally.

For a JSON column:

```pyre
@syncable(false)

record Game {
    id    Id.Int @id
    state Json<GameState>
}
```

Provide the JSON value directly:

```ts
await seed(db, {
  games: [
    {
      name: "Token game",
      state: {
        _type: "GameState",
        groups: [],
        clocks: [],
      },
    },
  ],
});
```

Do not pre-stringify JSON values. Pyre handles that before insert.

For a custom type stored across multiple SQLite columns:

```pyre
@syncable(false)

record MapEntity {
    id        Id.Int @id
    placement MapEntityPlacement
}

type MapEntityPlacement
   = MapEntityGridPlacement { x Int, y Int }
   | MapEntityWorldPlacement { x Int, y Int, scale Int }
```

Provide the constructed value:

```ts
await seed(db, {
  mapEntities: [
    {
      placement: {
        _type: "MapEntityWorldPlacement",
        x: 10,
        y: 20,
        scale: 100,
      },
    },
  ],
});
```

Pyre writes the discriminator and flattened backing columns behind the scenes, and reconstructs the returned row into the constructed shape.

## Constraints

- `seed` is server-side only.
- Top-level keys must be table names, not record names.
- Nested keys must be links declared on the parent table.
- If a nested row provides a foreign key that conflicts with the value derived from the parent link, `seed` fails.
- The whole seed call is atomic. Pyre starts a transaction, commits on success, and rolls back on validation or insert failure.
- Inserts are sequential for now: Pyre inserts one row at a time so it can read generated parent ids and report path-specific errors.
- `seed` bypasses Pyre query permissions.
- `seed` does not currently update Pyre sync metadata. Use it for setup/import workflows before synced clients rely on live deltas.

## Runtime API

Generated server output binds the helper to a database namespace. Inputs are JSON-compatible values: use a string or Unix seconds for `DateTime`, omit fields to let defaults apply, and supply `null` explicitly for nullable fields.

```ts
import { seed } from "./pyre/generated/typescript/seed";

await seed(db, data);
```

`server.ts` also re-exports this helper for compatibility with existing imports.

For a project with multiple schema namespaces, import `fixtures` and select the destination explicitly, for example `fixtures.Main.seed(mainDb, data)` with `SeedInput<"Main">`. The runtime checks the installed namespace before inserting. Cross-database references use explicit foreign-key values; generated inputs don't offer cross-database nested writes.

The lower-level runtime helper reads the installed schema directly from the target. Initialize the server WASM runtime before calling it:

```ts
import { seed } from "@pyre/server/query";
import { init } from "@pyre/server/wasm";

await init();
const result = await seed(db, data, { namespace: "Main" }); // namespace check is optional
```

The legacy `(db, schemaMetadata, data, validators?, options?)` signature still works; supplied metadata and validators no longer control ingestion. `batchSize` has no effect, and `transactionMode: "batch"` or `"none"` is rejected. Seed requires interactive write transactions and a file-backed database for local libSQL clients. For 64-bit integer keys outside JavaScript’s safe integer range, configure the client with `intMode: "bigint"`; the JSON-compatible response still uses JavaScript numbers. Treat an `OutcomeUnknown` result as requiring inspection before retrying.

## CLI

```bash
pyre db seed ./app.db --fixtures ./fixtures.json
```

The command prints the inserted rows as JSON. It only reads the specified fixture file and only writes to an existing, initialized database. Use `--namespace` in a multi-schema project and `--auth` for a remote target; database and auth values can be supplied as quoted `$VARIABLE` references.

## Database Policy

Policy belongs to the physical database. `pyre db policy ./app.db` reads it without writing; an unmarked database is preserved. Use `pyre db policy ./app.db --set disposable` to opt an existing database into disposable workflows without changing application rows or installing a schema. An unreadable or malformed marker never authorizes a destructive operation.

When copying or restoring a database, establish the destination policy explicitly before use. A copied marker reflects the source's policy and is not a decision about the destination. If your copy tool preserves internal tables, verify the destination policy before any lifecycle operation and avoid carrying the source's disposable marker into a preserved destination.

### Disposable Setup and Reset

```bash
pyre db init ./demo.db --disposable
pyre db seed ./demo.db --fixtures ./fixtures.json
pyre db reset ./demo.db
```

Disposable init requires an empty target and installs the selected working schema. Reset requires an existing disposable target and replaces its entire user schema and Pyre metadata in one write transaction. The physical database, endpoint, and credentials stay the same. Reset rejects FK cycles when the connection enforces foreign keys and cannot safely drop the tables in dependency order. A successful reset restores the disposable marker and installs a new sync epoch at revision zero; it records the working schema for runtimes without claiming any migration files ran.

Drain application traffic and connections before reset. After success, load fresh schema and authorization contexts before admitting clients. Old clients see the new epoch on reconnect and must replace their cached authority; this does not update their client code. If the transaction outcome is unknown, keep the target out of service until you inspect its state.

Rust exposes `pyre::db::lifecycle::{init_disposable, reset}`; TypeScript exposes `initDisposableDatabase` and `resetDatabase` from `@pyre/server/schema` after WASM initialization. Both take a database connection, the namespace and current standalone schema, and any external identities required by that schema.

## Rust API

`pyre generate` emits a fixed-schema Rust helper at `pyre/generated/rust/seed.rs`:

```rust
mod generated_seed {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/pyre/generated/rust/seed.rs"
    ));
}

let input = generated_seed::SeedInput {
    users: Some(vec![generated_seed::SeedUsersRow {
        name: Some("Fred".to_string()),
        posts: Some(vec![generated_seed::SeedPostsRow {
            title: Some("example post".to_string()),
            content: Some("My content!".to_string()),
            ..Default::default()
        }]),
        ..Default::default()
    }]),
    ..Default::default()
};

let result = generated_seed::seed(&conn, input).await?;
```

Writable columns are omittable so SQLite defaults and generated ids can apply. Server-managed sequences are excluded. Nullable columns use `SeedField<T>` to distinguish `Omitted`, `Null`, and `Value(T)`. `serde_json::to_value(&input)` produces the same fixture object accepted as a JSON file by the CLI, including physical table keys such as overrides from `@tablename`.

The Rust runtime loads the Pyre schema recorded in the database and performs the complete seed call in one immediate transaction. As with the TypeScript API, it bypasses query permissions and does not update sync metadata.

For projects with multiple schema namespaces, `seed.rs` emits one module per physical database, such as `generated_seed::main::seed` and `generated_seed::campaign::seed`. Cross-database links remain available through their foreign-key columns but are not exposed as nested seed fields because one SQLite transaction cannot span separate connections.

Composite links are not currently supported by nested seeding and return an explicit error. Inserts are sequential within the transaction; large imports should be measured against their expected row counts.
