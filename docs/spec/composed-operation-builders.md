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

This is a checkpoint in the full MEC-106 feature PR. Update prediction uses the
existing worker. UUID row identity in that worker, create/delete prediction and
incremental removals, broader submission/bridge conformance, and complete feature
examples remain release work in this same PR.

Generated compile-positive/negative and wire checks:

```sh
npm exec --yes --package=elm@0.19.1-6 --package=bun@latest -- cargo test --test crud_builders --test elm_client --test typescript_db
```
