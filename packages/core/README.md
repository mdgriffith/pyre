# @pyre/core

Shared TypeScript contracts for Pyre packages.

This package provides common types used by generated code and runtime packages (for example schema metadata and query-shape types).

## Install

```bash
bun add @pyre/core
```

## Local Edits

`@pyre/core/local-edits` contains browser-independent compiled-operation descriptors.
Importing generated builders does not load a client, worker, database, or transport.
The compiler writes `typescript/edits/<Namespace>.ts`; `typescript/edits.ts`
re-exports a single namespace directly, or exports namespace modules when there are
several. The compiler's `_default` namespace is exposed as `Main`; its runtime
namespace string remains `_default`.

```ts
// User/Audit schema from src/generate/typescript/local_edits.rs.
import { Main, User, Audit, Commands, batch, operations, type UserId }
  from './generated/typescript/edits/Main';

declare const userId: UserId;
// Supply `operations` to the browser's LocalEditsConfig. Its fence must match
// Main.name and Main.manifest. Binding selects and checks the database lifetime.
const db = await client.localEdits('main', Main);
const stopFailures = db.onEditFailure(showWriteFailure);
const plan = batch([
  User.create({ key: crypto.randomUUID(), name: 'New', fixed: 'x' }),
  Audit.create({ message: 'created' }),
  User.update(userId, { note: null }),
  Commands.rename({ key: userId, name: 'Ready' }),
] as const);
const outcome = await db.submit(plan).confirmed;
```

Generated `Edit<N, R>` and `Batch<N, R>` are opaque and namespace-scoped. Batch
results retain tuple order and each operation's result type. CRUD results contain
the branded affected identity; named commands retain their compiled result codecs.
Existing target IDs and foreign keys use the schema's branded ID types, while
UUID creates accept fresh UUID strings. Integer create identities are server-owned.

Create omission follows nullable/default rules. Update omission means unchanged;
`null` is accepted only for nullable columns. JSON and union inputs replace whole
values, not individual members; concurrent member changes can be overwritten.
Inputs are captured by value and must be JSON
values; DateTime inputs use strings or numbers, not `Date` objects. Empty updates
reject on submission. Empty batches submit without transport.

Prediction is deliberately conservative: named commands and integer creates never
predict, and UUID identity alone is insufficient. Server/default-owned materialized
values or unproven visibility disable create prediction. The compiler currently
proves complete primitive-valued public rows and public deletes; more complex
schemas fall back to authoritative reconciliation.

Browser acceptance proves commit; confirmation additionally requires authoritative
coverage. Pending writes are memory-only and never automatically retried. Subscribe
to failures even if ignoring receipts. Generated CRUD can bypass invariants held
only in named commands; keep those writes on the command (MEC-117 follow-up).

See [local-edit usage](../../docs/usage/local-edits.md) for configuration, named-call
migration, Elm patches and the explicit-session server binding. The server returns
an outcome directly; committed `InvalidResult` is `acceptedUnreconciled` without a
fabricated result. Cross-runtime release conformance is still a verification gate.

Runtime implementers can consume `EditPlan` via `planKey` and reuse its captured
operations and result assembler. This low-level interface is not an authorization
boundary: server execution must independently validate the manifest, namespace,
effective session, inputs, cardinality, and result codecs. The existing client
`edit`, `batch`, and unbound `localEdits(databaseId)` APIs remain available for
current low-level consumers.
