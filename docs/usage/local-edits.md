# Local Edits

Generated TypeScript and Elm builders describe immutable, namespace-scoped writes.
Construction does not select a database or perform I/O. Browser submission returns
a receipt; [server submission](#server-seeds) returns an outcome directly.

## Generated TypeScript

The compiler writes `typescript/edits/<Namespace>.ts`. With one namespace,
`typescript/edits.ts` re-exports it directly; with several, import the individual
module. The compiler's `_default` namespace is `Main` in TypeScript, but
`Main.name` is still `_default`. Use the generated name and manifest, not literals.

This example uses the User/Audit schema and Rename command from the
[generator test](../../src/generate/typescript/local_edits.rs):

```ts
import { Main, User, Audit, Commands, batch, operations, type UserId }
  from './generated/typescript/edits/Main';

// client has the fenced configuration below; id is a validated existing UserId.
declare const id: UserId;
const db = await client.localEdits('main', Main);
const stopFailures = db.onEditFailure(showWriteFailure);
const rename = User.update(id, { name: 'Ready' }); // note is unchanged
const clearNote = User.update(id, { note: null }); // SQL NULL
const plan = batch([
  User.create({ key: crypto.randomUUID(), name: 'New', fixed: 'x' }),
  Audit.create({ message: 'created' }), // server-generated integer ID
  rename,
  clearNote,
  Commands.rename({ key: id, name: 'Final' }),
] as const);
const receipt = db.submit(plan); // synchronous, after asynchronous binding
const stopReceipt = receipt.subscribe(showReceiptState);
const outcome = await receipt.confirmed;
if (outcome.kind === 'confirmed') {
  const userId: UserId = outcome.result[0].id;
  const auditId = outcome.result[1].id; // AuditId, not UserId
  const namedResult = outcome.result[4]; // declared Rename result
}
// User.delete(id) is also a pure Edit<Main, Deleted<UserId>>.
```

CRUD results contain `{ id }`, not a readable row, even for a primary key named
`key`. Existing targets and foreign keys use branded IDs; fresh UUID creates accept
UUID strings. Integer create IDs are server-owned. Required create fields must be
supplied; nullable/default fields may be omitted. Updates omit unchanged fields and
accept `null` only for nullable fields. Managed fields, immutable update fields,
and primary-key updates have no setters. Empty updates reject as `InvalidEdit` on
submission; `batch([])` confirms an empty tuple without transport.

Inputs are captured by value, including nested JSON. Use JSON values, not `Date`
or class instances; DateTime inputs use strings or numbers. Omit optional properties
rather than setting `undefined`. JSON and tagged unions replace the whole value:
updating one member can overwrite another writer's concurrent member change.

Generated CRUD does **not** execute invariants encoded only in named commands.
Schema permissions still apply, but using a generated update instead of a domain
command can bypass that command's checks or side effects. Keep invariant-bearing
writes on named commands; enforcement/discoverability follow-up remains MEC-117.

Optimism is conservative: named commands and integer creates do not predict.
UUID identity alone is insufficient; defaults, server-owned fields, complex values
or unproven visibility can disable prediction. Updates require a known visible row;
if any member cannot predict, the entire batch is nonoptimistic. Batches preserve
operation/result order and run atomically in one database; they cannot reference
earlier results. Submit later after confirmation to use a generated integer ID.

## Browser Configuration

The TypeScript runtime is opt-in through `ServerConfig.localEdits(databaseId)`.
Without it, the existing `baseUrl`, endpoint, cache, live transport and named
operation routes remain available. Legacy named calls are deliberately
nonoptimistic for both query and entity readers, including calls carrying legacy
`optimistic` metadata; reader changes come from authoritative server data only.
Fenced mode does not use those routes or
legacy delta persistence. Its mutation and complete replacement adapters own all
routing and authentication; the client does not invent endpoints.

```ts
import { PyreClient, type LocalEditsConfig } from '@pyre/client';
import { Main, operations } from './generated/typescript/edits/Main';

const client = await PyreClient.create({
  schema,
  cacheNamespace: sessionCacheNamespace,
  server: {
    baseUrl: legacyBaseUrl,
    localEdits(databaseId): LocalEditsConfig {
      return {
        fence: {
          databaseId,
          instance: crypto.randomUUID(),
          authGeneration,
          namespace: Main.name,
          manifest: Main.manifest,
          databaseEpoch: authenticatedEpoch,
        },
        minimumSafeRevision: authenticatedSecurityRevision,
        operations,
        async prepare(request, signal) {
          const credentials = await acquireCredentials(signal);
          // Encoding/credential preparation only. Do not send the mutation here.
          const body = JSON.stringify(request);
          return {
            dispatch: signal => mutationAdapter({ body, credentials, signal }),
            // Optional release of resources allocated during preparation.
            dispose() {},
          };
        },
        replacement: (request, signal) => replacementAdapter(request, signal),
        subscribeHints: receive => authenticatedHints.subscribe(receive),
        timeoutMs: 30_000,
      };
    },
  },
});

// Bind once before mixing submissions and client.run calls. Submission on this
// handle is synchronous; binding is asynchronous because it initializes storage.
const db = await client.localEdits('main', Main);
const unsubscribeFailures = db.onEditFailure(showWriteFailure);
// Low-level lifetime controls are on the runtime, not Database<Main>.
const runtime = await client.localEdits('main');
const unsubscribeLifecycle = runtime.onLifecycle(showEditState);
```

`mutationAdapter` returns the parsed, original accepted/rejected server envelope,
not a `Response`, unwrapped result, or fabricated rejection. `replacementAdapter`
receives the server's V1 request shape, including `version: 1`, and
returns the original complete `type: "replacement"` envelope with flat fences,
captured `requestId`/`target`, `serverRevision`, `scope: "database"`,
`complete: true`, and every schema table as `{ rows: [...] }`. Empty tables matter.
The worker validates coverage, identities, completeness and the security barrier.
Live hints are authenticated, fenced `syncRequired` envelopes, not row deltas.

## Generated Adapter Boundary

`EditOperation<I,R>` is trusted generated manifest metadata: a stable `id`, pure
throwing `parseInput(unknown): I` and `decodeResult(unknown): R` codecs, and optional
pure `predict(input)` metadata. Register the same codecs/prediction functions used
by the builders. The runtime does not infer prediction from operation names or
application input. Named operations omit `predict`. Generated predictions must
declare the actual schema identity, writable fields and the complete materialized
field set; generated integer/default/permission-dependent creates normally omit
prediction. Captured inputs and wire results must be JSON values. Named-command
decoders retain their declared application types, including decoded DateTime
values; the Elm bridge forwards validated wire values for Elm's own codecs.

`edit(operation, input): EditPlan<R>` and tuple-preserving `batch(plans)` are the
low-level pure builder boundary for downstream generated modules. They snapshot
inputs and operation order. Submission revalidates and snapshots again against the
bound manifest. These remain low-level, unbranded helpers; applications should use
the generated record builders, `Commands`, namespace-scoped `batch`, and
`client.localEdits(databaseId, Main)` for the typed boundary. Generated `operations`
is the browser codec registry, not the server SQL manifest.

## Migrating Named Calls

Both `client.run(databaseId, compiledMutation, input, callback)` and bridge named
mutations use this same worker queue in fenced mode, without inferred optimism.
Their operation IDs must exist in the configured manifest with result codecs;
the named entry path suppresses prediction even for a generated edit operation ID.
Their legacy callback receives `{ ok: true, value }` only at confirmation; other
outcomes have `ok: false`, an outcome-kind `error`, and the structured `outcome`.
Use receipts for accepted state and late settlement. The manifest-aware runtime
does not accept legacy optimistic metadata as proof of prediction safety.
Public named calls capture input at invocation, including during lazy binding.
Invalid JSON produces an observable `InvalidEdit` rejection through both the
callback and the runtime's failure stream, with no mutation transport. Changing
the original input after invocation cannot repair or alter the captured intent.

## Lifetimes And Readers

In this section, `db` denotes the low-level runtime returned by
`await client.localEdits(databaseId)`; the branded handle exposes only `submit`
and `onEditFailure`.

Pending writes are memory-only, not a durable offline outbox. Reloading loses
pending intent and receipt correlation; it does not prove that a sent write failed.

`receipt.confirmed` resolves once, never rejects, to `confirmed`, `rejected`,
`outcomeUnknown`, or `acceptedUnreconciled`. Acceptance is commit evidence, not
confirmation: browser confirmation also requires authoritative replacement coverage
at or beyond the commit revision. Receipt and database lifecycle subscriptions
can subsequently report definitive settlement of an unknown outcome. `quarantined: true` on a
lifecycle notification is separate from its state and is not a failure.
`receipt.cancel()` only cancels unsent work. Validation and transport failures are
also delivered through `onEditFailure`, independent of receipt consumption.
This stream's `LocalEditsFailure` includes standard write failures with certainty,
plus read/cache failures without certainty (they do not assert a write outcome).
On disposal, an accepted browser receipt becomes `acceptedUnreconciled` with its
validated result and commit revision. Unlike the server's `InvalidResult` variant
below, this means missing browser reconciliation, not a failed result codec.

Preparation reserves the worker's single dispatch slot before credential lookup.
Only the worker's subsequent dispatch event invokes `PreparedEditTransport.dispatch`.
A preparation failure/timeout is a definite rejection. A failure/timeout after
authorization is unknown and blocks later writes. Writes are never automatically
retried. `db.receiveResponse(requestId, envelope)` accepts late same-lifetime
evidence, including evidence from an application-provided outcome lookup.

`db.setConnected(false)` keeps unsent work queued, cancels preparation/read resources,
and marks dispatched unresolved work unknown. Browser online/offline events call
this automatically; custom transports should also report their connectivity.
Reconnect can retry read-only catchup, never unknown writes. `db.retryCatchup()`
explicitly retries reconciliation after read failure.

`db.dispose()` or `client.disconnect()` delivers final lifecycle/query/entity
events, clears overlays, aborts transports, removes hint/online/offline listeners,
and detaches worker query subscriptions. Epoch mismatch ends the old lifetime.
`await db.dispose()` (or `await db.ended`) waits for final delivery and detachment;
it does not wait for outstanding IndexedDB persistence transactions.
For auth/epoch changes or explicit unknown-outcome recovery, dispose the old client
and bind a fresh authenticated configuration with a fresh instance token. This
does not cancel or establish an ordering barrier against old server transactions.

Queries and entities install the worker's single publication before notifying any
reader or starting network preparation. Fenced entity subscriptions initialize from
that visible state, never from IndexedDB. `EntityChange` now also supports
`{ tableName, id, op: "remove" }` for rejected creates, deletes, filter exits and
replacement omissions. Consumers must handle removals; they contain no row payload.

Only `replacementInstalled` is persisted. Rows, epoch, full fence and covered
revision replace the old scope in one IndexedDB transaction, evicting legacy
cursors. Invalidation/disposal evicts the entire persisted scope atomically.
A transaction-level lifetime owner prevents delayed old-lifetime writes from
overwriting a newly claimed cache. Optimistic visible tables are never persisted.
Startup always requires authenticated replacement, not restoration of old-auth
cache rows. Same cache names across concurrent tabs may suppress persistence by
the older owner; they do not coordinate server write order across tabs.

Malformed security evidence with no revision bound keeps the scope invalid until
a fresh authenticated lifetime is configured. An uncorrelated hint, even one with
a safety minimum, could predate that uncertainty and cannot establish recovery.

## Generated Elm

These imports and fields match the executable
[Elm fixture](../../tests/fixtures/elm-local-edits/Test.elm), whose default namespace
is `Db.Database.Default` (not TypeScript's `Main`). Explicit named namespaces use
their generated module, for example `Db.Archive.Edit.ArchiveEntry`.

```elm
import Db.Database as Database
import Db.Default.Edit.Audit as Audit
import Db.Default.Edit.Command.NamedAudit as NamedAudit
import Db.Default.Edit.Issue as Issue
import Db.Id
import Pyre
import Pyre.Batch as Batch
import Pyre.LocalEdits as LocalEdits


database : Database.DatabaseId Database.Default
database =
    Database.fromString "one"


submission model =
    let
        id =
            Db.Id.uuid "00000000-0000-4000-8000-000000000001"

        create =
            Issue.createWith { id = id, title = "New", owner = "me" }
                [ Issue.withAssignee Nothing ]

        rename =
            Issue.update id [ Issue.title "Ready", Issue.assignee Nothing ]

        plan =
            Batch.succeed (\issue audit command -> ( issue, audit, command ))
                |> Batch.and create
                |> Batch.and (Audit.create { message = "created" })
                |> Batch.and (NamedAudit.run { message = "named" })
    in
    Pyre.batch database plan model
    -- For a single edit: Pyre.submit database rename model
```

`Issue.Patch` and `Issue.CreateOption` are opaque and record-specific. Update
setters take plain values for non-nullable fields and `Maybe` for nullable ones:
omit a setter to leave unchanged, `Nothing` to clear, `Just value` to set. Duplicate
setters resolve left to right, last wins, including null. `create` takes only the
required record; `createWith required options` adds optional `with<Field>` setters
for nullable/default fields. There are no primary-key or immutable update setters.
`Batch.succeed`/`Batch.and` is applicative, not dependent result binding. An empty
`Batch.succeed value` confirms that value with `Pyre.NoEffect`.

Both submit functions return `( Pyre.Model, Pyre.Effect, Pyre.Receipt a )`.
Store the returned model and receipt. Forward `Pyre.Send` through the existing
`pyreStoreOut` port, in production order; do not use independent unordered
`Cmd.batch` sends for ordered edits. The built-in `PyreClient` bridge handles
`elm-local-edits` using the configured manifest and worker queue, and sends
lifecycle/failure messages to `pyre_receiveQueryDelta`. Feed those through
`Pyre.decodeIncomingDelta` and `Pyre.update`. Effects do not change rows inside
Elm `update`; worker ingress reserves order and publishes local state before
network preparation. Replaying an effect is not a fresh submission or server
idempotency mechanism. Elm keeps receipt state, not a second row cache.

Read `Pyre.outcome receipt nextModel` for `Maybe (Pyre.Outcome a)`; constructors
are exposed by `Pyre.LocalEdits` (`Confirmed`, `Rejected`, `OutcomeUnknown`,
`AcceptedUnreconciled`). Acceptance alone returns `Nothing`; use `Pyre.lifecycle`
or `Pyre.editState` to observe it and quarantine. Consume `Pyre.failures nextModel`
after each incoming update, even when ignoring receipts: failures are per-update,
not a durable log. Late settlement can change an unknown Elm outcome.

## Server Seeds

Use `localEdits.bind` from `@pyre/server/local-edits` with generated builders and
the generated `typescript/server` manifest. Authorize the database first, then
bind its connection, database ID, namespace, and actual effective session.
`session` is required, including an explicit `{}` for sessionless schemas.

The runnable [seed example](../../packages/server/fixtures/local-edits/seed.ts)
uses this exact binding and generated Project/Audit schema:

```ts
import { localEdits } from '@pyre/server/local-edits';
import { Main, Project, Audit, batch } from './generated/typescript/edits';
import { manifest } from './generated/typescript/server';

const seeds = localEdits.bind({
  database, databaseId: 'seed-example', namespace: Main, manifest,
  session: { userId: 7 },
});
const outcome = await seeds.submit(batch([
  Project.create({ id: crypto.randomUUID(), name: 'Seed project', owner: 7 }),
  Audit.create({ message: 'created' }),
] as const));
if (outcome.kind === 'confirmed') {
  console.log(outcome.result[0].id, outcome.result[1].id);
}
```

Server submit returns `Promise<Outcome<R>>`, not a browser receipt:

| Outcome | Meaning |
| --- | --- |
| `confirmed` | Committed and typed result materialized; no browser cache to reconcile. Empty batches have no commit revision. |
| `rejected` | Definite noncommit, with `code` and optional operation `index`. |
| `outcomeUnknown` | Execution may have committed; `code: "OutcomeUnknown"`. Never automatically replay. |
| `acceptedUnreconciled` | Known committed but result decoding failed: `code: "InvalidResult"`, commit revision, optional index, **no fabricated `result`**. |

Invalid binding/session configuration throws synchronously. No worker, IndexedDB
or SSE registration is needed. Writes are never automatically retried, and creates
are inserts, not rerunnable upserts. This permission-checked, revisioned executor
is distinct from the [legacy `seed` helper](seeding.md), which bypasses query
permissions and does not maintain sync metadata.

## Verification Status

Generated signature/behavior checks live in `src/generate/typescript/local_edits.rs`
and `tests/elm_local_edits.rs`, with an Elm-to-worker bridge fixture. Browser
runtime checks run with `bun test ./packages/client/src-ts/`; native IndexedDB
coverage remains opt-in via `PYRE_PLAYWRIGHT_MODULE`. Server seed checks and runnable
example commands are in [seeding](seeding.md#explicit-session-local-edits).
These checks are not a claim of completed cross-runtime release conformance;
the remaining MEC-119 release gate must be verified separately.
For the historical claim that sync execution discarded typed results, see the
[current correction](../dev/sync-mutation-response-handoff.md).
