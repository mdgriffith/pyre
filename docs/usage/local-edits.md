# Browser Local Edits

The TypeScript runtime is opt-in through `ServerConfig.localEdits(databaseId)`.
Without it, the existing `baseUrl`, endpoint, cache, live transport and named
operation routes remain available. Legacy named calls are deliberately
nonoptimistic for both query and entity readers, including calls carrying legacy
`optimistic` metadata; reader changes come from authoritative server data only.
Fenced mode does not use those routes or
legacy delta persistence. Its mutation and complete replacement adapters own all
routing and authentication; the client does not invent endpoints.

```ts
import { PyreClient, edit, batch, type LocalEditsConfig } from '@pyre/client';

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
          namespace: 'Main',
          manifest: manifestVersion,
          databaseEpoch: authenticatedEpoch,
        },
        minimumSafeRevision: authenticatedSecurityRevision,
        operations: generatedManifest,
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
const db = await client.localEdits('main');
const unsubscribeFailures = db.onEditFailure(showWriteFailure);
const unsubscribeLifecycle = db.onLifecycle(showEditState);
const receipt = db.submit(batch([
  edit(updateIssueOperation, { issueId, title: 'Ready' }),
  edit(closeSprintOperation, { sprintId }),
] as const));
const unsubscribeReceipt = receipt.subscribe(showReceiptState);
const outcome = await receipt.confirmed;
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
prediction. Inputs and decoded results must be JSON values.

`edit(operation, input): EditPlan<R>` and tuple-preserving `batch(plans)` are the
low-level pure builder boundary for downstream generated modules. They snapshot
inputs and operation order. Submission revalidates and snapshots again against the
bound manifest. These helpers are not yet the schema/namespace-branded generated
CRUD surface described in the normative contract. Generated modules can keep their
operation entries private and expose their own schema-specific constructors.

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

`receipt.confirmed` resolves once, never rejects, to `confirmed`, `rejected`,
`outcomeUnknown`, or `acceptedUnreconciled`. Acceptance is commit evidence, not
confirmation. Receipt and database lifecycle subscriptions can subsequently
report definitive settlement of an unknown outcome. `quarantined: true` on a
lifecycle notification is separate from its state and is not a failure.
`receipt.cancel()` only cancels unsent work. Validation and transport failures are
also delivered through `onEditFailure`, independent of receipt consumption.
This stream's `LocalEditsFailure` includes standard write failures with certainty,
plus read/cache failures without certainty (they do not assert a write outcome).

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

The production conformance suite is
`npm exec --yes --package=bun -- bun test ./packages/client/src-ts/`.
It compiles the current worker into `target/` and exercises real TS services through
the existing ports. Native IndexedDB browser coverage remains opt-in via the
existing `PYRE_PLAYWRIGHT_MODULE` test configuration.
