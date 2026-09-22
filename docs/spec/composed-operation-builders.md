# Generated operation builders

The generated CRUD queries remain the execution unit. Builders capture inputs for
those precompiled query IDs; the server owns SQL, validation, and permissions.

## TypeScript

```ts
import { Document, batch, database, documentId } from './generated/typescript/core/edits';

const target = database('_default', 'tenant:1'); // schema namespace, database instance
const id = documentId('01900000-0000-7000-8000-000000000000');
const edit = Document.update(id, { title: 'New title', summary: null });
await client.submit(target, batch([edit]));

const create = Document.create({ title: 'Title', owner: 'Owner', tags: [] });
const receipt = await client.submit(target, [create]);
if (receipt.ok) {
  const created = receipt.value[0].result.document[0];
  await client.submit(target, [Document.update(created.id, { title: 'Created' })]);
}
```

Construction does not execute. Inputs are copied at construction and transport
captures are isolated copies. A UUID create allocates its canonical UUIDv7 once
at construction; callers do not supply the primary key. `batch` preserves order
and rejects mixed known namespaces. `@pyre/client/operations` has no worker,
IndexedDB, or browser-executor import. Request/response adapters can obtain
descriptors using `captureOperations` and pass only `{ queryId, input }` to the
existing server executor under the authenticated session.

Record and foreign-key IDs are branded by schema namespace and record. The
generated `documentId` boundary converts a validated external identity; IDs in
typed CRUD results already carry their brand. Submission requires namespace
evidence for generated operations and checks it against captured metadata at
runtime. Results preserve tuple order and `{ index, queryId, result }` entries;
compiled validators decode result values (including dates) and reject mismatched
counts or identities. `decodeOperationResults` provides the same decoding for
simple request/response adapters.

## Elm

```elm
import Db.Edit
import Db.Edit.Document as Document

editRequest databaseId documentId =
    Db.Edit.submit databaseId "edit-document"
        [ Document.update documentId
            [ Document.title "New title", Document.summary Nothing ]
        ]

createRequest databaseId =
    Db.Edit.submit databaseId "create-document"
        [ Document.create
            { title = "Title", owner = "Owner", tags = [] }
            [ Document.withSummary (Just "Summary") ]
        ]
```

Send these requests through the normal `pyreStoreOut` bridge port. Record-specific
`Patch` and `CreateOption` constructors are opaque. Omitted fields stay unchanged;
`Nothing` encodes null and `Just` sets nullable values. Required create fields are
a record; optional fields are builders. Optional-create names use `withField`;
collisions receive underscore suffixes. The bridge allocates create UUIDv7 IDs
before sending the existing `$batch` mutation and its prediction list to the
worker. Submission does not patch an application-side cache.

Use `Db.Edit.receive databaseId requestId message` on the mutation-result port to
obtain an opaque `Receipt namespace`. It rejects a different database instance or
request. `Document.createResult 0 receipt`, `updateResult`, and `deleteResult`
return the corresponding generated `Result String ReturnData`, checking both the
index and compiled query identity. Rejections remain ordinary `Err` values.

## Execution

Compiler-owned CRUD definitions emit strict input validators and a direct-write
statement index. The existing executor checks cardinality immediately after that
write and rolls back the batch if it did not affect exactly one row. UUID create
metadata additionally requires canonical lowercase UUIDv7. Named commands retain
their existing behavior; recognition compares the full generated definition.

The existing worker now uses UUID row keys throughout its database, indices,
query tracking, optimistic intents, row revision stamps and catchup cursors.
Imported UUIDs may use any version; only generated creates require UUIDv7.
IndexedDB version 4 clears older authoritative caches and their cursors/revisions
atomically, then reloads authority from the server. Deploy the UUID server/schema
migration with this client upgrade; integer-keyed synced caches are unsupported.

The existing engine also captures delete intent and complete scalar generated-create
intent. A create is predicted only when every selected scalar value comes from a
captured input and every value is present, including explicit nullable values. An
omitted default or server-managed value keeps that create server-only. Rejection
replays the remaining operations; a newer authoritative removal suppresses pending
create replay.

Updates capture permission preimages before writing. Across an ordered batch, the
executor retains each row's first observation and its final value; rows created in
the batch have no committed preimage. Original and final visibility are authorized
separately. Intermediate grants cannot reveal an identity, and former readers get
incremental removals rather than a full invalidation. Deleted preimages pass through query-permission filtering before becoming
identity-only tombstones (the schema primary-key field and `_pyre_removed`) in the existing delta format. The
worker removes rows and index entries, publishes removals to query/entity readers,
and persists deletion plus its revision stamp atomically. Old upserts cannot restore
a tombstoned row, including after cache reload.

Rust manifests now carry generated-create UUIDv7 validation and normal/sync direct
write indices. Native execution validates the captured identity before starting a
transaction and rolls back zero-row generated writes. Named commands retain their
existing explicit-identity behavior. Native permission delivery also compares
original and final visibility before emitting removals.

Primary-key field names come from schema indices throughout row storage, query
tracking, relationships, optimistic CRUD, revision fences and entity streams.
An ordinary field named `id` is not treated as identity when another field is the
primary key. IndexedDB stores an envelope separate from application columns;
reload preserves both custom keys and per-row tombstones. Entity changes retain
the generic `change.id` identity, while their `row` uses the actual schema field.

## Runnable integration examples

`tests/fixtures/ComposedExample.elm` is a complete model/effect/port example.
`composed-browser.ts` attaches its bridge and submits generated TypeScript edits;
`composed-conformance.ts` supplies the real HTTP adapter and explicit-session seed
execution. `cargo test --test crud_builders` generates their schema modules,
compiles the Elm application, and runs them in Chromium against SQLite/WASM.
It verifies typed receipts, optimistic updates, atomic rejection, nullable/JSON
replacement and equal query IDs in two database instances. Install Playwright and
build the client worker and server WASM first (see the field-edit proof guide).

For application-side query storage, regenerated `Pyre.getResult` now takes
`databaseId` before `queryId`. `QueryUpdated` carries both instance and query ID;
incoming bridge messages include `databaseId`. Keep that discriminator when
forwarding messages through custom bridges. Ordinary `run` callbacks still work;
move to `submit` when composing operations and consume its typed indexed results.

## Explicit-session server execution and seeds

```ts
import { executeOperations } from '@pyre/server/operations';
import { queries } from './generated/typescript/server';
import { Document, database } from './generated/typescript/core/edits';

const result = await executeOperations(
  authorizedConnection, queries, database('_default', databaseId),
  [Document.create({ title: 'Seed', owner: 'Owner', tags: [], summary: null })],
  authenticatedSession,
  { mode: 'sync', sessions: connectedSessions, publish: sendToSession },
);
if (!result.ok) throw new Error(result.error.message);
const createdId = result.value[0].result.document[0].id;
```

The application authorizes and resolves the database connection and session; a
typed target is not an access token. Sync execution works with no connected
clients and preserves normal publication when readers exist. Use `{ mode:
'normal' }` for simple request/response/server-owned execution. The import-oriented
legacy `seed` helper bypasses query permissions; `executeOperations` instead uses
the exact compiled validators, permissions and atomic transaction executor used
for client submissions. Never automatically retry `OutcomeUnknown`; publication
or result-decoding errors after commit do not mean rollback.

Rust applications can pass `$batch` and the descriptor-array input to existing
`query::run` / `query::run_sync`, or use `query::run_operations`. Results preserve
the same indexed operation list. `run_sync` allocates its revision in the write
transaction; `SyncServer::calculate_deltas` publishes that captured revision.
Include the authenticated origin in the logical session map when requesting HTTP
origin authority, even if it has no SSE connection. Catchup uses a read transaction.

## Outcomes and limits

Pending intent is memory-only. A success confirms the server transaction, not
durable offline delivery or the continued visibility of every written row.
Rejections replay later pending intent. Unknown transport/commit outcomes clear
and reload authority with generation/revision fencing rather than replaying the
write; this also recovers deletions missed with the response. Ordinary updates
and removals stay incremental. Permission-evaluation failure or an oversized
removal batch uses exceptional invalidation.

Callers may ignore returned success values, but should handle rejected promises
and `ok: false` completions. The client emits mutation failure events even for
fire-and-forget calls; Elm receives failures on `pyre_receiveMutationResult` and
bridge dispatch errors through the configured error callback. No application-side
optimistic cache is required. Creates requiring omitted defaults or managed
`updatedAt` remain server-only; complete scalar creates and existing-row updates/
deletes can be predicted. JSON and tagged unions replace whole logical values,
so concurrent replacements are last-authority wins, not field merges.

Generated CRUD obeys schema permissions but can bypass business invariants present
only in named commands. Exclusive command ownership is a separate write-policy
follow-up (MEC-117), not a guarantee of the generic route. Local TS interactive
transactions require file-backed libSQL. Remote libSQL/deployed application auth
is not part of the verified local SQLite/browser matrix.

## Schema identity migration

Syncability defaults to true. Every record in a synced namespace must have exactly
one non-null `Id.Uuid @id` column; its field name need not be `id`. Type checking
rejects integer, plain string, nullable, missing and multiple primary keys before
generating clients or executing queries. The requirement applies across all schema
files in that namespace.

For a server-owned database used through request/response queries, declare
`@syncable(false)` at namespace scope. Integer and other primary-key types remain
available there. This is an execution-mode choice, not a way to synchronize integer
rows with the UUID-only worker.

For an existing synced database, migrate primary keys and their foreign-key values
together using an explicit old-to-new ID mapping; update stored session IDs and
external references as well. A schema type change alone does not convert existing
data or preserve its relationships. Regenerate clients/manifests and deploy the
schema, server and client upgrade together. IndexedDB v4 discards legacy caches;
the next initialization loads migrated authority.

Trusted imports and named commands can supply explicit UUIDs of any version.
Generated client creates allocate canonical lowercase UUIDv7 once, and their
trusted manifest metadata requires UUIDv7 at execution. UUID ordering is an index
locality optimization, not business ordering. A newly created ID cannot be used by
another operation in the same batch without a future reference/placeholder API.

Generated compile-positive/negative and wire checks:

```sh
npm exec --yes --package=elm@0.19.1-6 --package=bun@latest -- cargo test --test crud_builders --test elm_client --test typescript_db
```

## Conformance evidence

| Contract | Executable coverage |
| --- | --- |
| Generated TS/Elm types, protected fields, custom keys, nullable inputs and receipts | `tests/crud_builders.rs`, `tests/elm_client.rs`, `tests/typescript_db.rs` |
| Actual generated bridge, two database instances, optimistic updates and typed completion | `tests/fixtures/ComposedExample.elm` + Chromium `composed-conformance.ts` |
| Compiled seed permissions, normal/sync results, JSON replacement and operation-N rollback | `composed-conformance.ts`, `packages/server/query.test.ts` |
| Native Rust ordered execution, UUIDv7/cardinality, atomic rollback and commit-order revisions | `tests/query_server.rs` |
| Original/final visibility, mixed writes, hidden grants, custom-key removals and caps | `tests/sync_server.rs`, `tests/query_server.rs`, `packages/client/scripts/field-edit-proof.ts`, `packages/server/query-sync.test.ts` |
| Overlapping/reordered intent, normalization, rejection, duplicate authority, catchup and unknown-outcome recovery | `packages/client/src-ts/optimistic-reconciliation.test.ts` |
| Native browser HTTP/SSE, late readers, held responses, persistence/migration/reload | `packages/client/scripts/field-edit-proof.ts` |
| Synced UUID namespace invariant and query-only integer compatibility | `tests/typecheck.rs`, `tests/id_types.rs`, full Rust fixture suite |
