# Local Edit Contract (MEC-107)

Status: Implementation contract for the approved Local Edit API in MEC-106.
The protocol details below define required behavior, not a release-conformance claim.
`tests/fixtures/local-edits/protocol.json` and `tests/local_edit_contract.rs`
execute a bounded reference model. They do not test production conformance.

Implementation checkpoint: the Rust/TypeScript batch executors and complete
replacement server protocol are implemented. Replacement is a single complete
transactional response with flat fences, `type: "replacement"`, `serverRevision`,
`scope: "database"`, `complete: true`, and `tables[name].rows`. It echoes the
captured `requestId` and `target`; row-free live hints use `type: "syncRequired"`
and the accepted response's reconciliation shape. Libraries remain route-independent.
Client storage, tracking, entity streams and persistence now use schema-declared
integer/UUID identity, including non-`id` keys. The opt-in browser runtime installs
fenced replacement and replays ordered pending intent, with receipts, quarantine,
observable failures and atomic query/entity publication. Schema-branded TypeScript
builders, generated Elm opaque patches and model/effect/receipt APIs, and the
TypeScript explicit-session server binding are implemented. See the
[usage guide](../usage/local-edits.md) for current imports and fixture-backed examples.
Production conformance now exercises generated TS and compiled Elm submissions
through both SQLite executors, including namespace-isolated replacement. The
optional native Chromium suite additionally exercises the built client, IndexedDB
and EventSource against the TypeScript executor. See the
[conformance fixture](../../tests/fixtures/elm-local-edits/README.md) for commands
and coverage boundaries; the reference model remains a separate bounded check.

Client metadata must be regenerated with the required `primaryKey` name/kind.
Records whose primary keys are neither integer nor UUID remain available to
ordinary generation, but local-edit CRUD/descriptors are omitted with an explicit
generation warning rather than being treated as UUIDs.
Browser cache version 3 resets prior rows, cursors, revision and epoch together;
it does not migrate old flattened `id` caches. Invalid current-version caches
fail initialization. Query publication conservatively sends changed full results,
not identity-based diffs inferred from arbitrary projections. Typed Elm/TS schema
IDs are generated; TS query codecs retain their primitive result representation.
Production worker/service and generated-surface conformance tests cover
rejected-create/no-ghost and delete lifecycle transitions.
UUID inputs, sessions, generated result codecs and replacement ingestion require
36-character hyphenated hexadecimal strings, preserving case without imposing a
UUID version. Symbolic IDs previously accepted by server string codecs are now
rejected. Noncanonical stored values require correction before rollout; validators
do not coerce or rewrite existing identities.

## Boundary

Generate pure schema-derived TypeScript and Elm create/update/delete builders.
Edits are opaque, namespace-scoped values, not bound to a database until explicit
submission. Elm patches are opaque and record-specific; TypeScript updates take
schema-derived partial objects. Query results stay read-only;
there are no mutable handles, draft callbacks, application cache patches, or
inferred domain commands. JSON and unions are whole-value replacements: concurrent
member changes can be overwritten. Existing named commands and their result and
cardinality behavior remain available, including in batches, without inferred
optimism.

Generate a server manifest mapping stable compiled-operation IDs to namespace,
input/result codecs, generated-edit kind, identity, writable fields, and static
prediction capability. The server resolves only allowlisted compiled operations;
the client sends neither SQL nor query text for runtime compilation. Schema types
are not a security boundary: revalidate inputs, manifest version, namespace,
protected fields, session, permissions, and database on the server. Managed fields,
integer generated IDs, immutable update fields, and primary-key updates have no
setters. Handwritten commands can change a primary key under existing rules.
The Rust server constructs one immutable `BoundManifest` from the manifest and
trusted database-loaded schema context, caches its fingerprint and authenticated
namespace scope, and requires that bound capability for batch execution. Named
command results are validated recursively against manifest result metadata before
revision allocation and commit. Generated integer result identities must fit
JavaScript's safe integer range.

Elm duplicate setters resolve left to right, last setter wins (also for null). Omission
means unchanged on update, default/nullable omission on create. Empty updates reject
as `InvalidEdit`; they do not become reads. Empty batches confirm with `[]` in
TypeScript or the applicative `Batch.succeed` value in Elm, with no transport,
revision, or row-state publication. Invalid members reject the entire batch.
TypeScript edit and batch builders are pure and capture inputs (including nested
JSON and the operation array) by value, not by reference; submission also
validates and takes an immutable transport snapshot. Mutation of an input after a
builder or submission must not alter its intent. Elm values are already immutable.

## Public Surface

These signatures reflect the implemented generated surface. The TypeScript example
uses the User/Audit schema in `src/generate/typescript/local_edits.rs`; `UserId` is
a client UUID and `AuditId` a server integer. Generated edits/batches are opaque
and namespace-scoped; application code does not construct their internals.

```ts
import { Main, Records, Commands, batch, type UserId, type AuditId }
  from "./generated/typescript/edits/Main";

declare const userId: UserId;
const rename = Records.User.update(userId, { name: "final", note: null });
const create = Records.User.create({ key: crypto.randomUUID(), name: "New", fixed: "x" });
const remove = Records.User.delete(userId);
const db = await client.localEdits("main", Main); // Database<Main>
db.onEditFailure(event => showWriteError(event)); // install once, independent of receipts
const receipt = db.submit(rename); // EditReceipt<Updated<UserId>>; no await needed
const plan = batch([create, Records.Audit.create({ message: "created" }),
  Commands.rename({ key: userId, name: "final" })] as const);
const batchReceipt = db.submit(plan); // construction alone does not submit
const outcome = await batchReceipt.confirmed; // resolves, never an unhandled rejection
if (outcome.kind === "confirmed") {
  const createdId: UserId = outcome.result[0].id;
  const auditId: AuditId = outcome.result[1].id;
  const renamed = outcome.result[2]; // declared Rename result
}
```

`submit<R>(Edit<N,R> | Batch<N,R>): EditReceipt<R>` is the explicit database execution
boundary for both single edits and batch plans. The generated namespace-scoped
`batch` helper preserves the readonly input tuple's operation order and result
types, not a union array:

```ts
type EditResult<E> = E extends Edit<Main, infer R> ? R : never;
declare function batch<const E extends readonly Edit<Main, unknown>[]>(
  edits: E
): Batch<Main, { readonly [K in keyof E]: EditResult<E[K]> }>;
```

`batch([])` builds an empty plan; only `db.submit(plan)` creates its confirmed
receipt. Neither edit nor batch construction selects a database, reserves order,
publishes state, or starts I/O. TypeScript object updates have one captured value
per property, not an API of ordered setters. `EditReceipt<R>.confirmed` resolves
once to `EditOutcome<R>`: `confirmed`, `rejected`, `outcomeUnknown`, or
`acceptedUnreconciled` (the last case terminates an accepted receipt on disposal).
`Created<Id>`, `Updated<Id>`, `Deleted<Id>` contain `id`, not a mandatory readable
row. The generated successful-write result contract authorizes returning the
affected identity, including server integers, independently of read visibility.
No unreadable row payload is disclosed. A schema/runtime that cannot authorize
that identity result must reject the generated operation rather than fabricate an
ID or violate the typed result contract.
Receipt state subscriptions can later report resolution of an unknown outcome;
the already-resolved promise does not change. Named-command results keep their
declared codecs. Wire results are `{index, operation, value}` in input order and
must match the submitted manifest IDs, count, and codecs before typed access.

```elm
import Db.Database as Database
import Db.Default.Edit.Issue as Issue
import Db.Default.Edit.Audit as Audit
import Pyre
import Pyre.Batch as Batch

-- Fixture: tests/fixtures/elm-local-edits/Test.elm
-- Issue.update : Db.EditIds.DefaultIssue -> List Issue.Patch
--     -> Edit Database.Default (Updated Db.EditIds.DefaultIssue)
rename issueId =
    Issue.update issueId [ Issue.setTitle "final", Issue.setAssignee Nothing ]
-- Omit assignee: unchanged; Nothing: SQL null; Just id: set. No clear builder.
-- Non-nullable setters accept plain values, never Maybe.

-- Pyre.submit : DatabaseId n -> Edit n a -> Pyre.Model
--     -> ( Pyre.Model, Pyre.Effect, Receipt a )
submitRename issueId pyreModel =
    Pyre.submit (Database.fromString "main") (rename issueId) pyreModel

-- Typed applicative batch, internally one ordered heterogeneous operation list:
-- Batch.succeed : a -> Batch n a
-- Batch.and : Edit n a -> Batch n (a -> b) -> Batch n b
plan newUuid =
    Batch.succeed (\issue audit -> { issue = issue, audit = audit })
        |> Batch.and (Issue.createWith { id = newUuid, title = "New", owner = "me" } [ Issue.withAssignee Nothing ])
        |> Batch.and (Audit.create { message = "created" })
submitBatch newUuid pyreModel =
    Pyre.batch (Database.fromString "main") (plan newUuid) pyreModel
-- Pyre.outcome batchReceipt batchPyre decodes the typed record result.
-- Pyre.failures : Pyre.Model -> List EditFailure (delivered with incoming updates)
```

`Pyre.batch` has the same model/effect/receipt shape as submit. Elm stores only
request correlation and decoded lifecycle/results, not another row cache. Returning
an effect does not edit rows: the host must store the returned model and forward
`Send` through the existing bridge. Worker receipt of that message reserves order,
validates/captures, and atomically publishes local state to all query/entity readers.
This occurs before network preparation, not synchronously inside Elm `update`.
The bridge forwards effects in production order; do not use unordered independent
`Cmd.batch` sends for ordered edits. Reusing an unperformed effect is not a new
submission; request IDs prevent duplicate local enqueue, not server idempotency.
Callbacks/outcome messages arrive through the existing model/effect bridge.
`Pyre.init : String -> Model` requires a caller-provided incarnation token. Every
logical model reset must use a fresh token; request IDs include it plus the local
counter so stale effects cannot correlate with a new model incarnation.

Elm `create` accepts a required-field record; `createWith` adds opaque optional
`with<Field>` setters. `Batch.succeed value` with no edits confirms `value` without
an effect (TypeScript's empty tuple result is not an Elm restriction). The compiler's
`_default` namespace maps to Elm `Database.Default` and TypeScript `Main`; neither
brand grants database authorization. `DatabaseId namespace` selects an explicit
instance. Consume `Pyre.failures` after each update; it is not a retained error log.

```ts
// Server seeds use the same compiled executor, no cache, worker, or SSE required.
import { localEdits } from "@pyre/server/local-edits";
// Main/Records/batch as above; manifest from generated/typescript/server.
const seeds = localEdits.bind({ database: mainConnection, databaseId: "main",
  namespace: Main, manifest, session: {} }); // explicit sessionless fixture
const result = await seeds.submit(batch([
  Records.User.create({ key: crypto.randomUUID(), name: "Seed", fixed: "x" }),
  Records.Audit.create({ message: "created" }),
] as const));
// Confirmed carries the typed tuple; confirmed means committed plus
// authoritative result materialized in this execution (no browser cache to catch up).
```

The server executor's `submit<R>(Edit<N,R> | Batch<N,R>): Promise<Outcome<R>>`
directly returns the outcome, unlike browser receipts. Seed binding requires an
explicit validated effective session; no implicit admin
or legacy permission-bypassing seed helper. These are inserts, not rerunnable
upserts. Integer creates have no temporary IDs, result binding, or references to
earlier operation results. A later submit after confirmation may use the real ID.

Server outcomes are `confirmed`, `rejected`, `outcomeUnknown`, or
`acceptedUnreconciled`. A committed result-codec failure returns the last with
`code: "InvalidResult"`, `commitRevision` and optional `index`, but no `result`.
It must not masquerade as noncommit or invite replay. Invalid bind/session options
throw synchronously. See the [runnable explicit-session seed](../../packages/server/fixtures/local-edits/seed.ts).

## Execution And Identity

Reserve per-database invocation sequence synchronously at TS submit / worker
ingress, before async token lookup, encoding, or transport preparation. Publish
local batches atomically in that order. Dispatch at most one unresolved batch per
database; an accepted response releases the dispatch slot even while catching up.
An unknown outcome blocks further dispatch until resolved or a new fenced lifetime
is explicitly established. Different databases are independent, never one batch.

Use one authenticated effective session and one database transaction for the whole
ordered batch. Later operations observe earlier writes. Each generated edit must
actually affect exactly one target row, even if its returned read projection is
empty. Use write cardinality inside the transaction, not read-filtered row counts
or total trigger-side effects. Setting an existing row to its current values still
targets one row. Zero or more than one rejects with non-disclosing
`TargetNotWritable` (missing and forbidden indistinguishable), rolling back all
operations. Named commands keep declared zero/many semantics. Commit the revision
increment and change metadata in the same transaction as the writes, once per
nonempty committed batch, including zero-visible-row/named no-op results. Rejection
allocates no revision and emits no successful prefix. Publish only after commit.

Fence every request, response, catchup page and live message with database ID,
client instance/lifetime token, auth generation, namespace/manifest version, and
server database epoch. A revision is monotonic only within its epoch. Old instance,
auth or epoch traffic cannot settle receipts or modify current state. Epoch
mismatch enters reset: invalidate old coverage and pending overlays, resolve
unsent work as rejected and dispatched unresolved work as unknown, then establish
new epoch via authenticated replacement, never by silently adopting a response.

## Optimism

Generated CRUD enforces schema permissions, not checks or side effects implemented
only by named commands. Choosing CRUD can bypass those domain invariants; keep
such writes on their named command. MEC-117 tracks the enforcement/discoverability
follow-up; this API must not imply it is already solved.

| Operation | Capability and fallback |
| --- | --- |
| Existing update | Patch known visible row only when manifest proves predictable values and visibility; never fabricate missing fields/rows. |
| Complete client-UUID create | Optimistic only with stable identity and every materialized field and visibility known safe. UUID alone is insufficient. |
| Default/server-owned/permission-dependent create | Non-optimistic unless all resulting values and visibility are provably known; no invented timestamps/defaults. |
| Server-integer create | Non-optimistic; authoritative integer identity only. |
| Delete | Optimistic removal of known visible target when predictable; server still checks cardinality. |
| Named command | Non-optimistic, even when its name resembles CRUD. |
| Mixed batch | Whole batch non-optimistic if any operation is unpredictable. No partially optimistic batch. |

Evaluate predictability in ordered simulated state (so complete UUID create then
update can qualify). Missing targets during replay suppress that whole batch's
overlay, not resurrect rows or reject server work. All readers use one model:
`visible = replay(authoritative, ordered eligible pending intent)`; never replay
whole-row forward snapshots or restore inverse snapshots on rejection. Recompute
and publish once after each transition. Server corrections to untouched fields
survive later intent. Local atomicity does not assert eventual server acceptance.

## Lifecycle

| State/event | Meaning and action |
| --- | --- |
| queued / locallyApplied | Memory-only captured intent; locallyApplied only if entire overlay eligible. |
| sent, quarantined | Still awaiting response; replacement suppresses its overlay without changing outcome state or emitting a failure. |
| accepted | Definitive commit evidence, operation-indexed typed results and commit revision; not yet confirmation. |
| reconciled / confirmed | Accepted AND authoritative coverage includes commit; retire overlay atomically with replacement and replay later intent. |
| rejected | Definitive noncommit: validation, permission/cardinality, transaction failure; remove only this intent and replay. |
| outcomeUnknown | Dispatch may have committed: timeout, disconnect after dispatch, lost/malformed commit response; never label rejection or automatically retry. |
| disconnected before dispatch | Keep queue in memory, resume in order; timeout/cancel before dispatch can definitely reject. |
| accepted, catchup fails | Stay accepted, retain overlay only against still-precommit base; emit reconciliation failure, retry read-only catchup. |
| disposal / auth change / epoch reset | Clear overlays and subscriptions; unsent rejects as disposed/fenced, dispatched unknown, accepted stays known committed but not confirmed. Deliver final lifecycle events before detaching. |
| late/duplicate response | Ignore if fenced; same-lifetime valid evidence may resolve unknown. Duplicate completion emits no duplicate failure/confirmation. |

A `status: "rejected"` envelope that also contains `commitRevision`, `results`, or
`reconciliation` is contradictory and therefore malformed/outcome-unknown, even
when the contradictory field is null or otherwise fails decoding.

Standard failure events contain request ID, database/lifetime, phase, code,
operation index when safely known, and certainty (`rejected`, `unknown`, or
`acceptedUnreconciled`), never private row data. Validation failures, unknown
outcomes, disposal and reconciliation errors are observable through the database
failure stream even if nobody uses receipts. Rate-limit repeated catchup errors
without losing the first failure. A receipt is optional to consume, not necessary
for error reporting. Timeout after known acceptance cannot turn it into rejection
or erase commit knowledge.

Pending writes are memory-only, not a durable offline outbox. Request IDs correlate;
they do not authorize retry. Recovery uses a late definitive response or a future
authoritative outcome lookup if supported. Without that evidence, a snapshot cannot
prove which request committed. An already-unknown outcome stays unknown; a sent
request stays sent until definitive response or actual timeout/transport failure.
Withdraw unresolved dispatched overlays without inventing failure events. For an
actual unknown outcome, require explicit application recovery/new lifetime before
sending more writes.
Never resubmit automatically, even a UUID create or apparently idempotent setter.
A new client lifetime does not cancel an old server transaction or prove noncommit.
Recovery must account for that transaction possibly committing later; a snapshot
alone is not a write-order barrier. Without definitive settlement or a server-side
execution barrier, do not promise ordering relative to old unknown work. Starting
independent work is an explicit application choice, not transparent queue resume.

## Revision Protocol

V1 chooses conservative full replacement rather than predictive deltas. The
authoritative base is the complete permission-filtered sync scope for this database
and auth generation, not only queried rows. Every accepted response carries
`commitRevision` and `reconciliation: { kind: "replaceRequired", atLeast: r }`,
or an equivalent complete replacement. This is required without an SSE connection,
origin registration, visible result rows, or subscribers. The authenticated request
session suffices. SSE is a catchup hint, not an acknowledgement requirement.
`replaceRequired` also carries `invalidate: true` when visibility/removal safety
is uncertain and an authenticated `minimumSafeRevision` covering the permission
change; clients then invalidate the entire scope immediately. Missing safety
metadata is treated as uncertainty, not as permission to retain old visible rows.

Track `coveredRevision` separately from `requiredRevision`. Partial/duplicate/
out-of-order notifications can raise required revision but never modify the base
or advance coverage. In particular, revision 12 containing row B cannot suppress
revision 11 containing previously unseen row A. At catchup dispatch capture a fixed
`target = max(requiredRevision, coveredRevision, minimumSafeRevision)` with request
ID and auth/epoch fence. A snapshot may advance coverage only when complete,
same-fence, consistent at a single revision, and at least that captured target,
current coverage, and the current security minimum. Validate the target against
the stored request, not an untrusted response field. New ordinary hints may raise
`requiredRevision` while the request is in flight; they do not move its target or
invalidate an otherwise safe intermediate snapshot. Keep the newer requirement
outstanding and schedule another catchup when `coveredRevision < requiredRevision`.
For example, a fetch targeting 10 can install revision 10 after a hint for 12,
then catch up to 12. Security invalidation at 12 is different: it forbids exposing
revision 10 even though that snapshot satisfies its original request.
Do not use a paginated timestamp merge as replacement. Stage pages with snapshot
ID, epoch, auth generation, revision, scope and terminal completeness token; validate
all pages before one atomic install. If the server cannot pin a consistent snapshot,
restart rather than publish a mixed-revision result. Duplicate/older complete
snapshots are ignored. Persist coverage only with the corresponding complete base.

Replacement deletes every absent row in its declared complete scope, including
permission removals, physical deletes and old keys from handwritten primary-key
changes. An empty snapshot is a meaningful complete replacement. Do not return
tombstones revealing never-visible IDs. On permission uncertainty, immediately
invalidate the old visible scope and suppress overlays until authenticated
replacement at or above a monotonic `minimumSafeRevision` in the current auth/epoch
fence. Record the permission-change revision as that minimum, even when an older
catchup is in flight; never clear invalidation just because a response is complete
or satisfies its old target. If no safe revision is known, remain invalid until an
authenticated security barrier establishes one; do not default the minimum to an
old coverage value. Auth changes require a new auth fence and authenticated scope
replacement; no revision, however high, makes an old-auth response acceptable.
Do not leave revoked rows visible during catchup. Delta
optimization is deferred until it can prove completeness, removal safety and
per-change coverage; correctness uncertainty always selects replacement.

Before installing any replacement, quarantine every dispatched request without
definitive outcome: that snapshot might already include its commit. Such intent
must not be replayed, even for simple assignments (it could overwrite a later
server change). Quarantine is a separate boolean, not an outcome state. An ordinary
SSE-before-HTTP race leaves the request sent, emits no failure and does not resolve
its receipt; only an actual timeout/disconnect or other transport failure makes
its outcome unknown. Unsent intent is safe to replay. Known accepted intent retires if
snapshot revision >= commit revision; if below, it can replay only on a proven
precommit base. Unknown intent remains unknown after replacement, never inferred
accepted/rejected from row equality. This applies to SSE-before-HTTP races too.
Later same-fence accepted evidence clears quarantine and can confirm against
coverage already installed, or restore eligible replay on a proven precommit base;
later rejection removes quarantine and intent. A read-only catchup retry is safe; a write
retry is not. Publication of replacement, retirement, and eligible replay is atomic.

Example request/response (abstract transport envelope; endpoint routing remains
the generic mutation boundary):

```json
{"version":1,"databaseId":"main","instance":"tab-7","authGeneration":2,"databaseEpoch":"e1","namespace":"Main","manifest":"m1","requestId":"q1","sequence":1,"operations":[{"operation":"IssueUpdate@hash","input":{"id":"uuid-a","title":"Ready"}}]}
```

```json
{"requestId":"q1","databaseId":"main","instance":"tab-7","authGeneration":2,"databaseEpoch":"e1","namespace":"Main","manifest":"m1","status":"accepted","commitRevision":9,"results":[{"index":0,"operation":"IssueUpdate@hash","value":{"id":"uuid-a"}}],"reconciliation":{"kind":"replaceRequired","atLeast":9,"invalidate":false}}
```

All envelopes require the same fence; a rejection instead carries a sanitized
code and optional failing index, no successful results. Transport failures are
local unknown events, not invented rejection envelopes.

## Executable Scope And Audit

The JSON trace format is language-neutral: initial row maps, ordered stimuli and
explicit expected observations. Row keys abbreviate `(table, typed primary key)`;
`Issue:u1` is a symbolic UUID, `Audit:1` an integer. Internal setter pairs preserve
Elm duplicate order; they do not specify the public TypeScript update shape.
Null is explicit. `safe` abbreviates manifest prediction, `actual` and
`readable` supply database adapter observations for server cardinality tests.
The mixed-create trace's integer key denotes the eventual server identity, not a
client-supplied create input or temporary ID; it is never locally materialized.
`submit`, `dispatch`, `accept`, `reject`, `unknown`, `partial`, `catchup`, `replace`,
`invalidate`, and `fence` are protocol stimuli; `expect` checks derived state.
`catchup` captures one request target; `replace.target` must match it and its
revision must satisfy target, coverage and security minimum. `invalidate.minimum`
supplies an authenticated safe revision; establishing an unknown security barrier
is outside the model. Omitted fence fields abbreviate the current fence, not an
optional production validation. `unknown` represents an actual timeout/transport
failure, not receipt of a snapshot. Expectations observe quarantine separately.
The bounded model uses one database, scalar/JSON rows, one complete snapshot per
replacement, one captured catchup target and string fence tuples. It executes
replay, quarantine, lifecycle, atomic server staging and revision coverage, but not
SQL, codecs, real manifests,
network scheduling, pagination, generated TS/Elm compilation, or permission
inference. Downstream runtime tests must cover those, immutable async capture,
effect ordering, failure subscriptions, typed result codecs and transaction races.

Original source audit (superseded by the implementation checkpoint above):
`src/generated_queries.rs` (CRUD and writable-field filtering),
`src/server/sync.rs` (origin registration dependency, result envelope preserved,
revision stamping separate from execution), `packages/client/src/Main.elm`
(whole-row forward/inverse optimism and global high-water check),
`packages/client/src/Db.elm` (integer `id` assumption), and
`docs/usage/elm-sync.md` (model/effect/bridge and existing nullable Updates API).
The existing `src/server/seed.rs::seed` has no session parameter and is not this
executor. Existing named APIs are retained; these new builders do not change the
legacy nullable API in place. Production conformance across both server runtimes
and both generated surfaces remains MEC-119's release gate.
