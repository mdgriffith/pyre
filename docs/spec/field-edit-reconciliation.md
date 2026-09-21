# Existing-engine field-edit milestone (MEC-107)

The first milestone upgrades the existing generated/precompiled operation path.
Client code continues to run generated query and mutation modules:

```ts
await client.syncDatabase(databaseId);
await client.run(databaseId, NotesQuery, {}, renderNotes);
await client.run(databaseId, UpdateTitle, { id: noteId, title }, reportResult);
```

`UpdateTitle` carries the existing generated field-intent metadata. The client
predicts only rows already present in its engine. It does not invent absent rows
or persist predictions. Request/response execution through `run` remains available;
`runWithSync` supplies the incremental authority used by synced clients.

```text
generated operation + input
          │
          ├── ordered field intent ──────────────────┐
          ▼                                         ▼
existing HTTP / runWithSync                  existing Elm engine
          │                                 authoritative rows + intent
          ▼                                         │
precompiled SQL + revision in one transaction        ├── query readers
          │                                         └── entity readers
          ├── origin HTTP delta ────────┐                    ▲
          └── other clients' SSE delta ├── per-row reconcile ┘
                                      └── authoritative IndexedDB writes
initial IndexedDB / paged catchup ────────────▲
```

## Reconciliation and recovery

- Responses, live deltas, and catchup share per-row revision filtering. A newer
  update to one row cannot suppress a delayed valid update to another row.
  Revisions follow transaction commit order, not later publication order.
- Rejecting one edit removes its intent and replays later intent over authority.
  Confirmation uses the server's normalized values. Later acknowledged fields
  shield those values while earlier requests settle.
- Ordinary visible field updates send affected rows, not a replacement scope.
  The origin receives authority in its HTTP response even without an SSE origin
  connection. Fanout is idempotent for a returned execution result.
- If permission filtering omits affected rows, the server cannot safely infer
  which omitted identities a recipient previously knew. It sends an
  identity-free `invalidate` message instead. That recipient clears its cache,
  advances its generation, and catches up from an empty cursor. Permission
  evaluation failures also invalidate. Payload/fanout caps use ordinary catchup.
- Mutation and catchup responses captured before invalidation cannot restore
  state. Live deltas are fenced during reset. The invalidation revision floor
  survives reload, as do each row's revision stamps. IndexedDB commits row data
  and stamps together. Server catchup reads rows and its revision in one read
  transaction. Live row maxima do not advance the catchup cursor.

## Reader/protocol compatibility

The engine publishes its visible state to the TypeScript entity projection,
including initialization, late subscription snapshots, optimism, rejection,
catchup, and reset. This local port currently carries a visible snapshot; network
updates remain incremental. The projection does not reconcile mutations itself.

Entity batches now include `{ op: 'remove', tableName, id, row: { id } }` when a
row leaves a subscription, including local filter exit or reset. Consumers must
remove that identity. Regenerate Elm streams and handle `EntityRemoved table id`.
Deploy the client support for `invalidate` with the server change; older clients
cannot perform this permission-loss recovery.

The proof now uses UUID identities throughout the existing worker and its
persistence path. Ordered field batches use the same engine. Insert/delete
prediction and a general incremental deletion protocol remain subsequent work
in this feature PR.

## Composed server execution (MEC-108)

`run` and `runWithSync` also accept an ordered array in place of the query ID:

```ts
const operations = [
  { queryId: UpdateTitle.id, input: { id: firstId, title: "First" } },
  { queryId: UpdateTitle.id, input: { id: secondId, title: "Second" } },
];
const result = await runWithSync(db, queries, operations, undefined, session,
  connectedSessions, databaseId, originConnectionId);
await result.sync(sendToSession);
```

The server manifest supplies validators, session bindings, database namespace,
and precompiled SQL. All operations execute in order in one write transaction.
Results retain `{ index, queryId, result }` for each entry, including repeated
operations. Named commands retain their own cardinality semantics. Compiler-owned
`generatedEdit.writeStatement` metadata requires exactly one direct affected row;
zero/multiple writes or any operation failure rolls back the whole batch.

One revision is allocated and validated before commit. Only final affected row
versions are permission-filtered and published through the existing sync path;
publication remains an explicit, memoized post-commit action. A publication error
does not imply rollback. A lost commit acknowledgement returns `OutcomeUnknown`,
which must not be automatically replayed. Empty batches return an empty result
without database I/O. Local interactive execution requires a file-backed libsql
database because its adapter detaches the connection for a transaction.

## Explicit client composition (MEC-111)

```ts
import { database, operation } from '@pyre/client/operations';

const edits = [
  operation(UpdateTitle, { id: firstId, title: 'First' }),
  operation(UpdateTitle, { id: secondId, title: 'Second' }),
  operation(UpdateTitle, { id: firstId, title: 'Final' }),
];
const result = await client.submit(database(UpdateTitle.primary_db, databaseId), edits);
```

Construction captures JSON values and prediction metadata without starting a
worker or executing a request. The browser-independent operations entrypoint can
also supply descriptors to request/response adapters. Submission returns a
discriminated result with ordered indexed results on success. Generated builders
retain tuple result types and decode through the compiled validators. Empty
batches return immediately without opening a database client.

The existing mutation transport sends one POST to the query endpoint's `$batch`
identifier. Its JSON body is the ordered `{ queryId, input }` array. The application
adapter passes that array to `run` or `runWithSync` instead of a named query ID;
it returns execution errors as non-success HTTP statuses, as for named mutations.
Prediction metadata never goes to the server. Existing named calls and the Elm
`mutate` bridge remain supported, including `$batch` with a descriptor-array input.

The worker captures every field intent in order against the preceding operation's
visible result, then publishes once. The entire list belongs to one request and
settles or rejects together. Later requests survive rejection, and out-of-order
acknowledged batches shield their fields with authoritative normalized values.
Predictions remain memory-only; only accepted authoritative rows are persisted.

Generated record-specific builders, typed results, and compiler metadata are
documented in `composed-operation-builders.md`. Schema-level UUID enforcement,
fixture migration and CRUD removal delivery remain work in the same feature PR.

## Reproduce the native proof

Requires Rust's `wasm32-unknown-unknown` target, installed workspace dependencies,
and Playwright with Chromium on `PATH`.

From `wasm`:

```sh
npm exec --yes --package=wasm-pack -- wasm-pack build --target web --out-dir ../packages/server/wasm
```

From `packages/client`:

```sh
npm exec --yes --package=elm@0.19.1-6 -- bash scripts/build.sh
npm exec --yes --package=bun@latest -- bun test src-ts
npm run typecheck
npm exec --yes --package=bun@latest -- bun scripts/field-edit-proof.ts
```

The native proof uses the public client, two browser pages, actual IndexedDB,
real libsql, real WASM permissions, and the server's HTTP/SSE execution path. It
checks normalization, optimistic visibility, response-only origin confirmation,
incremental peer delivery without catchup, rejection, late readers, permission
removal, a held pre-reset response, and reload. Its one-field SQL is a fixed
precompiled fixture; the test does not claim to prove general operation generation.
