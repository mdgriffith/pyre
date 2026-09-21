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

This milestone proves existing one-field updates with integer IDs. General
client composition, atomic batches, insert/delete prediction, and a general
incremental deletion protocol remain subsequent work.

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
