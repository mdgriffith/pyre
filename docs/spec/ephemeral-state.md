# Ephemeral state

## Status

This document defines the proposed contract for
[MEC-158](https://linear.app/mechanical-elephant/issue/MEC-158/add-typed-ephemeral-state-scoped-to-a-database-runtime).
The schema, validation, runtime, transport, and client work must use the existing
Pyre type, authorization, database-routing, and live-connection paths. The
database-runtime ownership decision below must be approved before runtime code is
implemented.

## Scope

Ephemeral state is typed, in-memory state scoped to one concrete database ID. It
supports live presence and transient shared values such as cursors, selections,
menus, highlights, and presentation state. Every participant authorized for that
database can subscribe to its published ephemeral state.

Ephemeral state is not relational or durable. It is never written to application
tables, Pyre sync tables, mutation outboxes, browser IndexedDB, or durable sync
cursors. Intermediate updates may be coalesced; healthy participants converge on
the latest complete value rather than receiving every write.

V1 has two reserved declarations:

- `state Connection` defines one value for each live server-assigned connection.
- `state Shared` defines one value for the authoritative database runtime.

Arbitrary state declarations, per-user state, state-specific permission
expressions, durable replay, CRDT merging, and distributed state replication are
out of scope.

## Database scope and authority

The concrete database ID is the room boundary. Participation uses the same
application authentication and database authorization as normal Pyre operations.
Authorization to one database does not expose another database's ephemeral state.

All authorized participants may read published state. Each connection may write
only its own `Connection` value. Multiple connections for one user remain distinct.
`Shared` writes are configured as server-only or participant-writable; server-only
is the default. A participant may subscribe without write access.

Connection IDs are assigned by the authoritative server and are not credentials.
Every write must be bound to the authenticated session, database runtime,
participation generation, and current ephemeral epoch. A client-supplied
connection ID cannot confer authority.

## Schema contract

State fields reuse Pyre's existing scalar, nullable, nested JSON, collection, and
custom tagged-union types. State declarations do not become records or enter the
SQL table context. They do not support links, joins, relational queries, indexes,
record permissions, or persistence directives.

Every writable field must be nullable or have an explicit default. A nullable
field without a default initializes to `null`. Non-nullable writable fields must
have defaults. Complete `Connection` and `Shared` values are validated before
publication; partially initialized values are never visible.

`Connection` may derive explicitly declared fields directly from trusted
`Session` fields. Derived fields are populated by the server, omitted from writable
patch types, and recomputed when authorization is refreshed. `Shared` cannot
derive fields from `Session`.

The parser and typechecker remain the authority for declarations. A resolved value
contract produced from the typechecked AST drives native Rust validation, the WASM
boundary, manifests, and generated client types. TypeScript must not implement an
independent state schema or validator.

## Values and patches

Clients patch writable top-level fields:

- omitted fields remain unchanged;
- explicit `null` clears nullable fields;
- nested values replace the whole top-level field;
- derived fields are unwritable.

The server validates a patch against a temporary complete value and applies it
atomically. Invalid patches change nothing and return an observable rejection.
Concurrent accepted `Shared` writes are server ordered and use last-accepted-write
wins independently for each top-level field.

Clients expose authoritative subscribed state separately from local desired state.
Pending patches merge by top-level field and repeated writes collapse to the most
recent desired value. An old success or rejection cannot discard a newer desired
write.

## Runtime ownership decision

The current server context manager is an authorization cache keyed by application
session and database ID. Its documented boundary explicitly excludes database
handles, live connections, subscriptions, publication queues, and runtime
retirement. Ephemeral state must not be attached to that cache or introduce a
second database-indexed registry beside an application's existing database owner.

The proposed direction is one canonical application-owned `DatabaseRuntime` per
resident concrete database. It composes the application database handle with:

- one ephemeral epoch and ordered revision sequence;
- one `Shared` value;
- `Connection` values and authenticated participation metadata;
- bounded live subscriber outboxes;
- explicit close or eviction.

The existing context manager continues to authorize joins, writes, and lease
renewals. It does not become the runtime owner. `pyre serve` owns one runtime for
its configured database until process shutdown. Custom Rust and TypeScript servers
obtain or create runtimes through an application-owned registry and explicitly
retire them with their database resources.

This direction preserves the approved context boundary while giving ephemeral
state one owner. An alternative that expands `ContextManager` into a database and
connection runtime would reverse that boundary and requires separate approval.

## Runtime lifecycle

A newly opened database runtime creates a fresh opaque ephemeral epoch and
initializes `Shared` from schema defaults. Joining creates a fresh connection ID
and complete `Connection` value. `Shared` survives when the last participant leaves
as long as the database runtime remains resident.

Closing or evicting the database runtime removes participants and discards all
ephemeral values. Reopening creates a new epoch and default `Shared` value. Pyre
does not infer runtime lifetime from the persisted database file and does not add a
separate presence idle timer.

Connection participation ends on explicit leave, detected transport close,
authorization loss, or lease expiry. Lease renewal is independent of application
updates. Session refresh recomputes derived fields atomically while preserving
writable fields; lost authority removes the connection.

V1 requires exactly one authoritative serving process for a database runtime.
Ordinary load-balancer stickiness is insufficient. Distributed ownership and
replication are follow-up work.

## Ordered delivery

Ephemeral epochs and revisions are separate from durable database epochs,
revisions, and catchup cursors. A subscription begins with a complete snapshot
captured at the same ordering boundary at which the subscriber is registered.
Later messages contain complete changed entries and explicit removals.

Upstream send cadence and downstream delivery cadence are independently bounded
and configurable. Slow-subscriber pending state coalesces by `Shared` or connection
entry. Payload size, participant count, and queued control messages are bounded.
Overflow produces an explicit resnapshot requirement instead of silent divergence.
Unchanged values are not periodically resent. A trailing flush guarantees eventual
delivery of the latest value while the connection remains healthy.

The existing HTTP/SSE route family is the V1 integration target: authenticated
HTTP handles upstream patches and lease renewal, while the existing live stream
delivers snapshots, changes, removals, and recovery instructions. The live
connection outbox must be bounded rather than adding an ephemeral queue beside the
current unbounded sender.

## Reconnection

Reconnection creates a fresh connection identity and begins with a fresh ephemeral
snapshot. While disconnected, clients clear remote state or mark it stale. After
the snapshot, a client republishes its current local `Connection` activity.

Clients do not replay historical patches and do not automatically republish old
`Shared` desired state. Ephemeral reconnection remains separate from durable cache
recovery: it neither clears durable query readers nor changes durable pending-write
outcomes.

## Public surfaces

Generated Rust and TypeScript code provides:

- complete readable `Connection` and `Shared` value types;
- patch types containing only writable top-level fields;
- complete-value codecs and validators;
- database-scoped subscription and update APIs;
- observable rejection and recovery events.

The browser implementation belongs to the existing per-database internal client,
not a parallel public client or cache. Ephemeral values never enter IndexedDB. Elm
worker and generated Elm regression coverage remains required, but V1 does not add
a separate public Elm state API unless product scope changes.

## Delivery order

1. Add schema syntax, generated types, one resolved Rust value contract, and native/WASM conformance fixtures.
2. Add the authoritative database-runtime state engine, authorization integration, explicit retirement, and lifecycle tests.
3. Add snapshot ordering, revisions, removals, cadence, bounded coalescing, leases, and explicit overflow recovery.
4. Integrate Rust and TypeScript APIs, `pyre serve`, reconnect behavior, documentation, and two-client browser conformance.

Each slice must extend existing ownership paths. If implementation requires a
second database registry, connection map, state validator, transport, or client
cache, work stops for architectural review.

## Acceptance

Executable coverage must establish:

- parser, formatter, and typechecker behavior for both reserved declarations;
- defaults, nullability, nested/custom values, derived fields, and atomic rejection;
- matching native Rust and WASM state transitions;
- database isolation, same-user multiple connections, read-only participation,
  trusted ownership, refresh, revocation, lease expiry, and runtime reset;
- race-free snapshot/update/removal ordering and bounded coalescing recovery;
- stale-response protection and reconnect without historical or `Shared` replay;
- a real two-client HTTP/SSE browser lifecycle through `pyre serve`;
- no state values in SQL, sync metadata, durable outboxes, or IndexedDB.
