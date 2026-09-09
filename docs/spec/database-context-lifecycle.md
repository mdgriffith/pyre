# Authorized Database Context Lifecycle

## Status

MEC-128, contract slice MEC-132 and initial internal server slice MEC-129. This
document specifies the target lifecycle. Version 1 negotiation/control wire types,
decoders, and shared fixtures are public; an internal Rust resolver/registry now
exercises the server authority boundary. Existing sync routes are **not**
context-aware, and no client context installation is implemented.

Server integration is MEC-129, client installation is MEC-130, and end-to-end
conformance/Lore adoption is MEC-131. The API names below describe the intended
boundary, not methods currently exported by Pyre.

### Internal Rust Checkpoint

`src/server/context/runtime.rs` is crate-private until generated local-session
dependency metadata, schema fingerprinting, and transport/live integration are
complete. It currently provides:

- An application resolver for exactly the requested database, with trusted
  identity, credential identifier, and credential deadline inputs.
- Validated effective sessions and explicitly configured projections; required
  client fields must be supplied and included. These dependencies and the schema
  ID are currently trusted configuration, not generated or independently verified.
- A coherent schema/manifest artifact shared by allocation identity, and database
  handles whose clones serialize complete operations on one libSQL connection.
  Applications must transfer exclusive connection use to the handle, not retain
  raw connection clones or wrap them in independent handles.
- Random 256-bit context IDs and leases bounded from before resolution by maximum
  age, credential expiration, and application deadline. Both wall and monotonic
  clocks enforce expiration; millisecond wire deadlines round down. A backwards
  wall-clock adjustment cannot extend an existing monotonic lease, while a forward
  adjustment can expire it early. Authoritative-read staleness and clock error at
  negotiation still affect the deployment revocation bound.
- Credential, identity+database, and database invalidation under a local registry
  lock. A retained allocation token prevents old resolutions from registering.
  Invalidation conservatively fences **all** pending resolutions, even unrelated
  ones, while withdrawing only matching installed contexts.
- Authenticated request scopes with fixed operation exposure, existing query and
  catchup permission enforcement, and checks before dispatch (including after
  waiting for the connection lock) and after completion. Results retain their
  original database/context binding. Invalidated dispatched mutations produce an
  outcome-unknown error, not a retry-safe context rejection.

This checkpoint does not implement live connection ownership, queued/delivery-time
checks, stream expiry, publication, refresh/disposal, routes, or public runtime
configuration. Ordinary mutations execute without the eventual live publication
path; the internal scopes must not be exposed as a completed sync service. Returning
a checked result is not an atomic delivery barrier. Expired entries are pruned on
negotiation; idle-registry cleanup and resource limits also remain integration work.
No cluster-wide invalidation or deployment freshness guarantee is claimed.

## Ownership

The application persists authentication and authorization facts: credentials,
membership, roles, access deadlines, and database routing. Pyre manages the
derived database scopes. No second durable login-session store is introduced.

A scope binds an authenticated identity **and credential** to one requested
database. Different identities, or the same identity in different databases, may
have different session values. Multiple connections may have equivalent scopes;
they must not share a mutable current-database selection in the login session.

The client selects database IDs using its configured authentication transport.
The resolver authorizes exactly the requested database; it neither discovers
databases nor subscribes to every accessible database. Existing explicit query
routing and independent active-sync selection remain intact.

## Schema and Projection

Reuse the schema's existing `session { ... }` contract and runtime validation.
One compiled schema context has one session shape, shared by its database
instances, not one set of values. An empty session contract is valid. This work
does not introduce independent sessions per namespace or arbitrary schema-family
multiplexing.

The resolver returns a validated effective server session and an explicitly
selected browser projection. The runtime must validate that every projected
field is declared and equals its effective server-session value. Required fields
are determined by session references in generated local query plans; missing
dependencies prevent installation rather than silently becoming null. Server
integration must supply this dependency metadata before enabling the API.

Projection selection is server configuration, never browser input. Authentication
objects, credential hashes, cookies, and database handles are not serializable
parts of the context. The wire decoder checks only that `session` is a JSON
object; schema, dependency, and authority validation belong to the runtime.

## Server Boundary

Conceptual configuration:

```text
runtime = PyreRuntime(schema, resolver, clientSessionFields, maxContextAge)
resolver(authenticatedRequest, requestedDatabaseId)
    -> authorized database + effective session + cache scope
       + authority revision + optional application access deadline
    | unauthenticated | denied | unavailable

runtime.routes()
runtime.authorize(authenticatedRequest, databaseId, contextId)
    -> authorized execution scope | context error

runtime.invalidate(credential)
runtime.invalidate(identity, databaseId)
runtime.invalidate(databaseId)
```

The application hosts this runtime and supplies trusted authenticated-request
metadata, including identity/credential associations and credential expiration.
The authorized database handle binds its canonical ID, schema, and manifest;
execution must not take an unrelated session and connection afterward. Version 1
requires the returned ID to equal the requested ID exactly; alias resolution
belongs before this protocol, not in client cache routing.

The runtime checks schema compatibility and validates the server session before
returning a context. It creates a fresh opaque context ID, reads the source
database epoch, and bounds the lease by the minimum of `maxContextAge`, credential
expiration, and application access deadline. `maxContextAge` is required and
finite; there is no indefinitely valid live context.

An authorized scope preserves existing query permissions, operation exposure,
and same-database publication rules. A context or connection ID is **not a bearer
credential**. Every request still authenticates and binds to the registered
identity, credential, database, and valid authority. Untrusted origin connection
IDs must never select another session's mutation-response visibility.

## Version 1 Wire Contract

The future opt-in route adapter negotiates through `POST /context` beneath its
configured mount. Existing routes do not gain this behavior merely by upgrading
the wire package. Authentication stays in the existing transport, not JSON.

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

Invalidation sent to an established connection:

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
only to the matching installed context; a delayed invalidation for an older ID
must not invalidate its replacement.

Request failure:

```json
{
  "type": "context_error",
  "protocolVersion": 1,
  "databaseId": "campaign:123",
  "code": "denied"
}
```

Error codes are `unauthenticated` (HTTP 401), `denied` (403), `unavailable` (503),
and `context_mismatch` (409). Unknown or inaccessible databases use the same
non-disclosing denial. Malformed protocol input is HTTP 400, not a resolver
invocation. Error bodies do not echo sessions or internal authorization details.
Request errors are associated with the local request generation, not broadcast
as unscoped errors on a live connection.

All fields are required; unknown envelope fields, versions, tags, and enum values
are rejected. Identifiers are nonblank strings and are preserved exactly, never
trimmed or sanitized into another identity. Blank means only Unicode White_Space
characters or U+FEFF. Context IDs additionally use only `[A-Za-z0-9_-]` so they
roundtrip through HTTP headers unchanged; the server runtime must generate them
with cryptographically strong uniqueness, not derive them from user identifiers.
All strings and object keys must contain Unicode scalar values, not unpaired
surrogates. `expiresAt` is an integer Unix timestamp in milliseconds in
the JavaScript-safe range 0 through 9007199254740991. Decoding checks shape, not
whether a lease is currently valid. Nested session values are JSON values, with
finite numbers and integer values restricted to the JavaScript-safe range
(-9007199254740991 through 9007199254740991). Exact larger values require a
schema-compatible string representation; never silently round or coerce IDs.
Fractional values use ordinary JSON/IEEE-754 semantics, not exact decimal math.
Schema validation remains separate. Transport adapters must bound body size/depth before
decoding; these codecs are not resource-limit or authentication middleware.

The codec boundary is the parsed JSON data model: `JSON.parse` followed by the
TypeScript parser, or `serde_json::Value` followed by typed deserialization in
Rust. Duplicate members use the last value at that boundary. Producers must emit
unique members, and adapters must authenticate/route using that same parsed
value, not an independent first-member parser. Direct Rust struct deserialization
from raw JSON can reject duplicates more strictly. Shared raw JSON fixtures test
the data-model path, numeric notation, safe-integer limits, and surrogate handling.

### Subsequent Requests and Data

The opt-in context-aware adapter must carry `databaseId` and `contextId` on every
catchup, execution, and live establishment request. Use a `Pyre-Context-Id` header
for HTTP requests and `contextId` URL routing metadata for native EventSource,
which cannot set custom headers. Neither location carries a secret or replaces
authentication. The exact existing sync-envelope integration is MEC-129 work.

All resulting data/control deliveries must carry the originating context binding
in addition to existing database/epoch metadata. Never attach a newer context ID
to a result authorized under an older context. Client request closures also
capture a local generation, including for HTTP errors without a context ID.
Legacy unbound responses cannot be accepted by a context-aware runtime.

A missing/expired/restarted binding or incompatible fresh authority returns
`context_mismatch` before execution. The client renegotiates and rebuilds if
needed. A transport failure after a write might have committed is not a definite
context rejection and must retain the existing outcome-unknown distinction.

## Invalidation and Freshness

Invalidate after the application commits its authority change. Invalidation
immediately removes affected local contexts from delivery eligibility, fences
in-flight resolver attempts, discards queued unauthorized payloads, and notifies
or closes subscriptions. Check eligibility at delivery as well as enqueue time.
A new context may only be registered from a fresh successful resolution.

The registry's invalidation generation is captured **before** resolution begins
and checked when installing its result. Retain sufficient generation information
until older work finishes; do not reset counters on disconnect or restart within
the same live registry. Refresh must withdraw stale eligibility before replacing
the context. Concurrent resolution cannot resurrect invalidated authority.

Revalidate authentication at each request and check registered lease/binding
validity. Refresh authority through the resolver on negotiation, invalidation,
and lease renewal, rather than indefinitely extending a cached grant. Expiry
checks must stop already-open streams without waiting for reconnect. Background
timers are an optimization; dispatch checks are mandatory.

`invalidate().await` acknowledges the local runtime barrier only. Cross-instance
invalidation requires application-delivered shared events or another documented
backend; publish local invalidation to all instances holding affected contexts.
Commit-to-event failures require durable retry or acceptance of the lease bound.

Even if an invalidation event is lost, a lease must expire within the configured
maximum age. State the effective revocation bound including authoritative-read
staleness and supported clock tolerance. Fail closed when freshness cannot be
established. Deployments requiring stronger immediate guarantees must supply
durable invalidation/authoritative checks; do not advertise an in-process map as
cluster-wide revocation. Clock/deadline enforcement and deployment tests are
release gates for MEC-129/MEC-131, not implemented by the timestamp decoder.

## Client Installation

Each selected database has an independent lifecycle:

```text
resolving -> installing -> catching_up -> ready
     |             |             |          |
     +-------------+-------------+----------+-> denied / error / stopped
ready -> resolving (refresh/reconnect/invalidation)
```

Selection/replacement increments a non-reused local generation before any async
work. Failures and stale attempts cannot make another database appear ready.
Subscriptions may register while waiting, but publish no local query/entity
results until projection installation and compatible cache hydration/catchup
complete. Ready means coherent query-visible data at a completed catchup boundary
for a valid installed context with live recovery established, not just an open
socket, a completed handshake, or durable disk flush.

An invalidated/expired context withdraws all affected query and entity views,
including generated application-side result storage, before new results appear.
Pyre cannot erase arbitrary data copied by application code or a hostile client.
Offline/reconnecting state is not implicit authorization to expose cached data
as ready. Automatic offline authorization is out of scope for version 1.

All resolver, worker, registration, query/entity, catchup, transport, mutation,
and persistence completions check their originating generation/context. Disposal
must invalidate callbacks immediately, close pending and established transports,
detach ports/listeners, and prevent stale writes to replacement storage. Aborting
requests is useful cleanup, not sufficient race protection.

### Cache Compatibility

Reuse requires a successful fresh negotiation and equality of the configured
server/deployment boundary, canonical database ID, schema ID, cache scope,
authority revision, and database epoch. Also compare the normalized projected
session structurally; a changed projection with a reused revision must never
silently reuse the cache. The resolver must change the authority revision when
effective permissions or their inputs change, even if projected values do not.

Context ID, connection ID, lease expiry, and local generation are not cache
identity. Encode the identity tuple losslessly or with collision-resistant
canonical hashing, not lossy name sanitization. Store verified compatibility
metadata alongside the cache. Existing caches without this metadata are not
trusted by the new lifecycle and require rebuild; legacy APIs are not silently
reinterpreted.

On incompatibility, withdraw old views and isolate or clear the entire affected
cache before fresh catchup. Merely merging new rows does not remove old secrets.
Rows, cursors, revisions, worker state, query/entity results, and optimistic
overlays all belong to the transition. Prevent incomplete rebuild metadata from
advertising a reusable complete snapshot after a crash.

### Pending Writes

Capture database, context, and local sequence before asynchronous preparation.
Never retarget or automatically retry a write on a different or renewed context.
Locally cancelled work not dispatched is distinct from dispatched work with an
unknown server outcome. Late replies cannot update a replacement context/cache;
callbacks still require explicit settlement. Reconnect/catchup can restore data
without proving the outcome of a particular lost response. Coordinate receipt
semantics with MEC-107/MEC-111; durable retries/idempotency remain MEC-116.

## Restart

Contexts and connection IDs are ephemeral. Reconnect may land on another server
instance, authenticate through the application's persisted session, and resolve
current authority for the requested database. Revoked access is denied rather
than reconstructed as its former grant. An unknown context ID prompts fresh
negotiation, not a fallback to trusting its browser projection.

Cache scope/authority revision must derive from durable authority or otherwise
force conservative rebuilding. A process restart alone does not change the
database epoch. A fresh context ID with compatible metadata can reuse a cache
after negotiation and catchup; source replacement changes the epoch separately.

## Verification and Implementation Boundaries

`tests/fixtures/database-context.json` is shared by Rust and TypeScript. Valid
wire cases roundtrip without changing identifiers or projected values; malformed
requests, injected authority fields, unknown variants, and unsafe timestamps are
rejected. Fixture scenario names describe envelopes, not proven runtime behavior.

```sh
cargo test --test database_context
cargo test --locked --lib server::context::runtime
bun test packages/core/database-context.test.ts
tsc --noEmit --strict --target ES2020 --module ESNext --moduleResolution bundler packages/core/index.ts
```

Internal runtime tests now cover separate database roles/tabs, authenticated scope
ownership, generated query/catchup permission parity, projection rejection,
invalidation during resolution and connection-lock waits, leases, restart mismatch,
and post-dispatch outcome classification. These do not exercise HTTP or live streams.

Remaining component/integration gates include forged connection ownership,
A-B-A/out-of-order completion,
invalidation during resolution, permission contraction across every reader and
cache layer, expiry on open streams, distributed freshness bounds, restart with
valid/revoked credentials, and late mutation/persistence outcomes. Passing wire
fixtures is not evidence these lifecycle guarantees are implemented.
