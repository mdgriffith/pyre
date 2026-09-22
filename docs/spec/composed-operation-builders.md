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

## Execution and current feature work

Compiler-owned CRUD definitions emit strict input validators and a direct-write
statement index. The existing executor checks cardinality immediately after that
write and rolls back the batch if it did not affect exactly one row. UUID create
metadata additionally requires canonical lowercase UUIDv7. Named commands retain
their existing behavior; recognition compares the full generated definition.

The existing worker now uses UUID row keys throughout its database, indices,
query tracking, optimistic intents, row revision stamps and catchup cursors.
Imported UUIDs may use any version; only generated creates require UUIDv7.
IndexedDB version 3 clears older authoritative caches and their cursors/revisions
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
identity-only tombstones (`id`, `_pyre_removed`) in the existing delta format. The
worker removes rows and index entries, publishes removals to query/entity readers,
and persists deletion plus its revision stamp atomically. Old upserts cannot restore
a tombstoned row, including after cache reload.

Rust manifests now carry generated-create UUIDv7 validation and normal/sync direct
write indices. Native execution validates the captured identity before starting a
transaction and rolls back zero-row generated writes. Named commands retain their
existing explicit-identity behavior. Native permission delivery also compares
original and final visibility before emitting removals.

This is a checkpoint in the full MEC-106 feature PR. Custom-primary-key client
consistency, broader submission/bridge conformance, complete feature examples and
the final cross-runtime release review remain work in this same PR.

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
schema, server and client upgrade together. IndexedDB v3 discards legacy caches;
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
