# Authorized Database Context Lifecycle

## Status and Scope

MEC-128 and its contract/server/client integration slices describe a convenient
way to plug an application's existing TypeScript or Rust server session system
into Pyre context management. The application remains the authentication and
authorization authority. This is not a replacement session system or a new sync
service.

Version 1 negotiation/control wire types, decoders, and shared fixtures are
public. TypeScript currently has the context codec only, not a context manager.
`src/server/context/runtime.rs` contains a crate-private Rust context-manager
prototype; there is no TypeScript/Rust lifecycle parity claim. Existing sync
routes are not context-aware, and client context installation is not implemented.

The release scope stays narrow: context resolution, validation, scoped execution,
leases, and local invalidation integrated with existing application authority.
No new HTTP/SSE routes, prescribed transport, live connection registry, or
publication queue subsystem belongs to this change. Public manager integration
is not complete. MEC-130 client installation and MEC-131 end-to-end conformance
and Lore adoption remain future work, not guarantees established by the codec
or private prototype.

## Component Ownership

The application persists credentials, membership, roles, access deadlines, and
database routing. It authenticates requests using its existing session system
and supplies trusted identity, credential identifier, and credential expiration
to the manager. No second durable login-session store is introduced.

The manager resolves exactly one requested database and derives a context binding
the authenticated identity **and credential** to that database, effective server
session, schema, and bounded lease. Different identities, or one identity in
different databases, may have different session values. Tabs and connections
must not share a mutable current-database selection in the login session.

The high-level client should know the accessible database list supplied by
application authority and independently select its active sync set through the
existing explicit sync selection. Accessibility is not subscription: neither
the list nor context resolution activates sync for every accessible database.
This work adds no database enumeration implementation or API. How the application
supplies and refreshes that list belongs to its existing integration. Every
requested database still requires current server authorization; a client-held
list is not a grant. Explicit query/mutation routing remains independent of sync
selection.

The application also owns handlers, transport, live subscriptions, publication,
buffering, and delivery. It must integrate context invalidation and eligibility
checks with those systems. The manager does not send control frames, close
streams, discard application queues, or revoke bytes already returned to a caller
or handed to transport.

## Current Rust Prototype

The private manager currently provides:

- An application resolver for exactly the requested database. The returned
  canonical ID must equal the requested ID; alias resolution belongs before this
  boundary. A resolution supplies an authorized database handle, effective session,
  cache scope, authority revision, and optional application access deadline.
- A coherent schema artifact compiled from schema and queries together, with a
  generated manifest, local-plan dependencies, and versioned SHA-256 fingerprint.
  The manager requires the same artifact allocation for its database handles.
  Generated `manifest.json` and TypeScript schema/artifact metadata must be
  regenerated and deployed together. Independently loaded legacy manifests are
  not authority-runtime artifacts.
- Validation of the effective session and an explicitly configured browser
  projection that includes required local-plan session fields. Operation exposure
  is fixed application configuration, not a projection override.
- Database handles whose clones serialize complete operations on one libSQL
  connection. Applications must transfer exclusive connection use to the handle,
  not retain raw clones or wrap them in independent handles.
- Random 256-bit context IDs, source database epoch lookup, and leases bounded
  from before resolution by maximum age, credential expiration, and application
  deadline. Wall and monotonic clocks both enforce expiration; wire deadlines
  round down to milliseconds. A backwards wall-clock adjustment cannot extend an
  existing monotonic lease; a forward adjustment may expire it early.
- Credential, identity-plus-database, and database invalidation under a local
  context registry lock. An allocation token captured before resolution prevents
  stale results from installing. Invalidation conservatively fences **all** pending
  resolutions, including unrelated ones, while removing only matching installed
  contexts.
- Authenticated scopes for existing query and catchup permission enforcement,
  checked before dispatch, after waiting for the connection lock, and after
  completion. Results retain their original database/context binding. A mutation
  invalidated after dispatch reports outcome unknown, not retry-safe rejection.
- Authenticated context disposal that rejects foreign/stale scopes, removes the
  context, and fences all pending resolutions. Shutdown/drop clears contexts and
  fences retained scopes and pending work. Already dispatched SQL is not cancelled
  or rolled back.
- Fixed private bounds of 1,024 installed contexts and 64 pending resolutions.
  Installation rechecks capacity. Pending reservations release on completion,
  error, or cancellation; invalidated unresolved futures retain their slots until
  completion or cancellation.
- One weak-reference native OS-thread poller per manager that prunes expired
  contexts every 25 ms and exits after shutdown/drop. Cleanup scheduling does not
  replace lease checks at scoped operations. This is context cleanup, not a stream
  expiry or delivery mechanism. The database-gated module is excluded from the
  WASM crate check; that check does not establish WASM manager support.

These bounds do not bound total application memory, resolver/session sizes,
retained scopes, concurrent SQL waiters, or transport buffers. Applications need
their own admission, body, and time limits. No per-identity fairness, cluster-wide
invalidation, or deployment freshness guarantee is established by this prototype.
Scoped mutations use ordinary execution without live publication.

## Schema and Projection

Reuse the existing `session { ... }` schema contract and runtime validation. One
compiled schema context has one session shape shared by its database instances,
not one set of values. An empty contract is valid. Independent sessions per
namespace and arbitrary schema-family multiplexing are outside this scope.

The application resolver supplies the effective server session; the manager
selects its browser projection from server configuration, never browser input.
Every projected field must be declared and equal its effective server value.
Required fields come from session references in generated local query plans,
including nested predicates/values and `$session` operands. All shipped local
plans contribute dependencies regardless of server operation exposure. Server
permission-only `session_args` do not require browser disclosure. Missing required
projection fields prevent installation rather than silently becoming null.

The schema fingerprint covers structural schema/session/permission metadata,
namespace sync modes, and generated execution/local plans. Sorted maps omit
source paths/locations; field order and plan text are retained, so compatible
edits may conservatively change the ID. Compiler semantic changes not represented
in these inputs require a fingerprint-domain version bump. Duplicate operation
IDs/names fail typechecking rather than silently dropping plans.

Authentication objects, credential hashes, cookies, and database handles are not
serializable context fields. The codec validates the `session` JSON object and
interoperable values, not schema, projection dependencies, or authority.

## Version 1 Wire Contract

These envelopes define a transport-neutral codec contract. The application
chooses how to exchange them through its existing authenticated integration;
this specification introduces no endpoint, header, URL parameter, or transport
requirement. Authentication stays outside the context JSON.

Request:

```json
{ "protocolVersion": 1, "databaseId": "campaign:123" }
```

Success:

```json
{
  "type": "context",
  "protocolVersion": 1,
  "databaseId": "campaign:123",
  "contextId": "opaque-ephemeral-binding",
  "schemaId": "compiled-schema-and-session-contract-fingerprint",
  "cacheScope": "opaque-service-and-actor-cache-boundary",
  "authorityRevision": "durable-effective-authority-version",
  "databaseEpoch": "source-database-incarnation",
  "session": { "userId": 42, "role": "Player" },
  "expiresAt": 1790000000000
}
```

Invalidation control envelope, for future application/client integration:

```json
{
  "type": "context_invalidated",
  "protocolVersion": 1,
  "databaseId": "campaign:123",
  "contextId": "opaque-ephemeral-binding",
  "reason": "authority_changed"
}
```

Reasons are `authority_changed`, `expired`, or `revoked`. Invalidation applies
only to the matching installed context; a delayed message for an older ID must
not invalidate its replacement. The manager does not emit this envelope.

Request failure:

```json
{
  "type": "context_error",
  "protocolVersion": 1,
  "databaseId": "campaign:123",
  "code": "denied"
}
```

Error codes are `unauthenticated`, `denied`, `unavailable`, and
`context_mismatch`. Transport status mapping belongs to the application. Unknown
or inaccessible databases use the same non-disclosing denial. Malformed protocol
input must be rejected before invoking the resolver. Errors must not echo
sessions or internal authorization details. Request errors belong to the local
request generation, not an unscoped broadcast.

All fields are required; unknown envelope fields, versions, tags, and enum values
are rejected. Identifiers are nonblank strings preserved exactly, never trimmed
or sanitized into another identity. Blank means only Unicode White_Space
characters or U+FEFF. Context IDs additionally use only `[A-Za-z0-9_-]`; this
alphabet does not prescribe where an application carries them. Servers must
generate cryptographically strong unique IDs, not derive them from user IDs.

All strings and object keys must contain Unicode scalar values, not unpaired
surrogates. `expiresAt` is an integer Unix timestamp in milliseconds in the
JavaScript-safe range 0 through 9007199254740991. Decoding checks shape, not current
lease validity. Nested session values are JSON values with finite numbers;
integers must be in -9007199254740991 through 9007199254740991. Exact larger values
require a schema-compatible string representation, never silent rounding or
coercion. Fractions use ordinary JSON/IEEE-754 semantics, not exact decimal math.
Adapters must bound input size/depth before decoding; codecs are not resource-limit
or authentication middleware.

The codec boundary is the parsed JSON data model: `JSON.parse` followed by the
TypeScript parser, or `serde_json::Value` followed by typed deserialization in
Rust. Duplicate members use the last value at that boundary. Producers must emit
unique members, and adapters must authenticate/route using that same parsed
value, not an independent first-member parser. Direct Rust struct deserialization
from raw JSON can reject duplicates more strictly. Shared raw JSON fixtures test
the data-model path, numeric notation, safe-integer limits, and surrogate handling.

## Application Integration Requirements

The following requirements guide integration; they are not implemented transport
behavior. Every context-aware operation must authenticate again and bind to the
registered identity, credential, requested database, and valid lease. A context
ID is **not a bearer credential**. Execution must not substitute an unrelated
session or connection after authorization. Application-owned live connection IDs
likewise must not let callers select another session's mutation visibility.

Results and control deliveries must retain their originating context binding
alongside database/epoch metadata. Never attach a replacement context ID to data
authorized under an older context. Future clients must capture a local generation
for completions, including errors lacking a context ID, and reject unbound or
stale results. Existing envelopes do not gain this behavior by upgrading the codec.

A missing, expired, restarted, or incompatible binding requires fresh resolution
before execution. A failure after a write may have committed is outcome unknown,
not a definite context rejection and not permission to retry automatically.

### Invalidation and Freshness

After committing an authority change, the application must invalidate affected
manager contexts and coordinate invalidation with its own handlers, subscriptions,
and delivery paths. The manager's synchronous invalidation is a local context
barrier: it removes matching entries and fences pending allocations. It does not
revoke returned results, discard queued bytes, notify consumers, or close streams.

The application must check eligibility at delivery, not just at initial
authorization or preparation, and coordinate that check with invalidation to
avoid a check-to-send race. Any application-owned buffering must preserve the
original binding and prevent stale delivery; bytes already handed off cannot be
recalled. Manager operation completion checks alone do not establish this
transport barrier. No queue or handoff API is prescribed here.

Refresh must withdraw stale eligibility before replacement and obtain fresh
authority from the resolver rather than indefinitely extending a cached grant.
The retained pre-resolution allocation token prevents pending work from
resurrecting invalidated contexts. Application stream integration must enforce
expiry without waiting for reconnect; the manager's cleanup poller does not stop
open streams.

Cross-instance invalidation requires application-delivered events or authoritative
checks on every instance holding affected contexts. Commit-to-event failures need
durable retry or explicit acceptance of the lease bound. State the effective
revocation bound including authoritative-read staleness and supported clock error.
Fail closed when freshness cannot be established. An in-process map is not
cluster-wide revocation; deployment guarantees require integration tests.

## Future Client Lifecycle

This section specifies future client work, not implemented context installation,
cache isolation upgrades, or readiness behavior. The high-level client should
retain application-supplied accessible databases separately from its independently
selected active sync set. Each selected database has an independent lifecycle:

```text
resolving -> installing -> catching_up -> ready
     |             |             |          |
     +-------------+-------------+----------+-> denied / error / stopped
ready -> resolving (refresh/reconnect/invalidation)
```

Selection/replacement increments a non-reused local generation before async work.
Stale attempts or failures cannot make another database ready. Subscriptions may
register while waiting, but must not expose local query/entity results until
projection installation and compatible hydration/catchup complete. Ready means
coherent query-visible data at a completed catchup boundary for a valid context
with live recovery established, not merely a handshake or durable disk flush.

Invalidation/expiry must withdraw affected query/entity views, including generated
application-side result storage, before new results appear. Pyre cannot erase
data copied by application code or a hostile client. Offline/reconnecting state
is not implicit authorization to expose cached data as ready; automatic offline
authorization is outside version 1.

Resolver, worker, query/entity, catchup, transport, mutation, and persistence
completions must check their originating generation/context. Client disposal must
invalidate callbacks immediately, close owned transports, detach ports/listeners,
and prevent stale writes to replacement storage. Aborting requests alone is not
sufficient race protection.

### Cache Compatibility

Reuse requires successful fresh negotiation and equality of the configured
server/deployment boundary, canonical database ID, schema ID, cache scope,
authority revision, and database epoch. Also compare the normalized projection
structurally: a changed projection with a reused revision must not silently reuse
the cache. Application authority must change the revision when effective
permissions or their inputs change, even if projected values do not.

Context ID, application connection ID, lease expiry, and local generation are not
cache identity. Encode the identity tuple losslessly or with collision-resistant
canonical hashing, not lossy name sanitization. Store verified compatibility
metadata alongside the cache. Existing caches without that metadata require
rebuilding for the new lifecycle; legacy APIs are not silently reinterpreted.

On incompatibility, withdraw views and isolate or clear the entire affected cache
before catchup. Merging rows does not remove old secrets. Rows, cursors, revisions,
worker state, query/entity results, and optimistic overlays all participate in
the transition. Incomplete rebuild metadata must not advertise a reusable complete
snapshot after a crash.

### Pending Writes and Restart

Capture database, context, and local sequence before async preparation. Never
retarget or automatically retry a write on a different or renewed context. Work
cancelled before dispatch differs from a dispatched write with unknown outcome.
Late replies cannot update replacement storage, but callbacks still need explicit
settlement. Catchup can restore data without proving a lost write's outcome.
Receipt semantics remain MEC-107/MEC-111 work; durable retries/idempotency remain
MEC-116.

Contexts are ephemeral. Reconnect may reach another server, authenticate through
the application's persisted session, and resolve current authority for the
requested database. Unknown context IDs require fresh negotiation, never trust
in the browser projection. Revoked access must be denied.

Cache scope and authority revision must derive from durable authority or force
conservative rebuilding. Restart alone does not change the database epoch. A
fresh context with compatible metadata may reuse a cache after negotiation and
catchup; source replacement changes the epoch separately.

## Verification Boundaries

`tests/fixtures/database-context.json` is shared by Rust and TypeScript. Valid
envelopes roundtrip without changing identifiers or projections; malformed input,
injected authority fields, unknown variants, and unsafe timestamps are rejected.
Fixture scenario names describe envelopes, not proven lifecycle behavior.

Relevant checks:

```sh
cargo test --test database_context
cargo test --locked --lib server::context::runtime
cargo test --locked --no-default-features --features database,json --lib server::context::runtime
cargo fmt --check
bun test packages/core/database-context.test.ts
tsc --noEmit --strict --target ES2020 --module ESNext --moduleResolution bundler packages/core/index.ts
```

Private Rust tests cover database/credential scope ownership, generated query and
catchup permissions, projection rejection, invalidation during resolution and
connection-lock waits, leases, context/pending bounds, disposal, cancellation,
shutdown, restart mismatch, and post-dispatch outcome classification. These are
manager tests, not network delivery or TypeScript lifecycle conformance tests.

Remaining work includes the public application integration boundary, client
installation and accessible-list integration, stale/A-B-A completion handling,
permission contraction across every cache/reader, application delivery fencing,
expiry on open streams, distributed freshness, and restart/late-write behavior.
Those are separate integration and release gates. Passing codec or private
manager tests does not establish a completed end-to-end session/sync service.
