# Ephemeral state

## Status

This document records the implemented contract for
[MEC-158](https://linear.app/mechanical-elephant/issue/MEC-158/add-typed-ephemeral-state-scoped-to-a-database-runtime).
The schema, validation, runtime, transport, and client implementation uses the existing
Pyre type, authorization, database-routing, and live-connection paths. The
database-runtime ownership decision below is the implemented V1 boundary.

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

`GET /sync/events` accepts `ephemeralWrite=true|false`, defaulting to `true`.
The value declares participation intent at subscription creation. Read-only and
writable subscribers both receive state; writable intent grants no authority
unless participant writes are enabled and the trusted owner and lease also match.

`pyre serve` uses these authenticated routes:

- `PATCH /ephemeral/connection` patches the caller's `Connection` value.
- `PATCH /ephemeral/shared` patches `Shared` when participant writes are enabled.
- `POST /ephemeral/lease` revalidates the request session, refreshes derived fields,
  and renews the lease atomically.
- `POST /ephemeral/resnapshot` replaces the subscription ordering boundary and
  returns a complete snapshot.

Each request includes `databaseId`, `ephemeralEpoch`, `connectionId`, and
`clientRequestSequence`; patch requests also include `patch`. The server echoes the
sequence in `ephemeralAccepted` or `ephemeralRejected`. The live stream uses the
explicit envelope types `ephemeralSnapshot`, `ephemeralChanges`, and
`ephemeralResyncRequired`, with payloads under fields of the same name. Durable
`databaseEpoch` and ephemeral `ephemeralEpoch` remain separate.
`clientRequestSequence` must be a non-negative JavaScript-safe integer.

Signed session payloads use `{ session, exp, sessionKey }`. When ephemeral state
is active, `sessionKey` is required and is the stable server-authenticated owner
identity across token expiration or signature refresh; Pyre stores only its hash.
Unsigned trusted headers derive identity from the complete credential, so changing
session JSON changes the ephemeral owner. Static development sessions share one
process-wide owner for the configured database.

Participant `Shared` writes are disabled by default. `pyre serve` enables them only
with `--participant-shared-writes`; trusted server code retains the runtime's
server-owned `Shared` API regardless of this transport policy.

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

## Executable evidence

Run the complete Rust integration set with `cargo test --locked`; run TypeScript
client/server coverage with `bun test packages/client/src-ts` and
`bun test packages/server`; run the real browser proof after building the client
worker with `bun packages/client/scripts/ephemeral-serve-proof.ts` from the
repository root (or `bun scripts/ephemeral-serve-proof.ts` from
`packages/client`). CI executes it after the existing field-edit proof.

| Contract | Executable evidence |
| --- | --- |
| Parser, formatter, reserved names, defaults, derivations, and invalid declarations | `tests/state_schema.rs` (`parses_state_as_distinct_ast_with_writable_and_derived_fields`, `formats_state_declarations_and_derivations_idempotently`, `valid_state_types_defaults_and_derivations_typecheck_without_tables`, and rejection tests) |
| Generated TypeScript/Rust complete and patch types | `tests/state_generation.rs` (`generated_typescript_state_surface_enforces_complete_values_and_writable_patches`, `generated_rust_state_surface_compiles_and_preserves_patch_null_semantics`); `packages/client/src-ts/ephemeral-state.typecheck.ts` |
| Defaults, nullability, nested/custom validation, normalization, and atomic patch rejection | `tests/ephemeral.rs` (`initializes_defaults_nulls_and_trusted_derivations_canonically`, `validates_recursive_custom_list_dict_and_typed_json_values_strictly`, `patches_are_atomic_top_level_replacements_and_reject_derived_or_unknown_fields`) |
| Contract serialization and Rust/WASM boundary behavior | `tests/ephemeral.rs::serialized_contract_has_the_same_results_as_the_native_contract`; `wasm/src/database_runtime.rs` private-handle, stable-error, and expiry tests; `packages/server/ephemeral.test.ts` |
| Database isolation, same-owner connections, read-only policy, trusted owner checks, refresh, revocation, expiry, close, fresh epoch/defaults | `tests/ephemeral_runtime.rs` tests from `runtimes_isolate_databases_and_same_owner_connections` through `authorization_loss_removes_the_connection`, including `close_fences_handles_and_reopen_has_new_epoch_and_defaults` |
| Snapshot ordering, coalescing, removals, bounds, overflow/resnapshot, reconnect identity | `tests/ephemeral_runtime.rs::subscription_snapshot_has_no_concurrent_update_gap`, `coalesces_complete_entries_and_delivers_on_trailing_cadence`, `removals_dominate_older_values_and_a_later_rejoin_wins`, `entry_overflow_requests_immediate_resync_and_suppresses_deltas`, `payload_and_transport_bounds_fail_explicitly`, `reconnect_has_a_fresh_identity_snapshot_and_no_replay` |
| `pyre serve` route authorization, fencing, read-only subscription, signed-session requirements | `tests/commands.rs::test_serve_ephemeral_http_sse_lifecycle_and_fencing` and adjacent serve tests; run `cargo test --locked --test commands test_serve_ephemeral` |
| Client desired/authoritative separation, stale replies, observable failures, reconnect Connection republish without Shared replay | `packages/client/src-ts/service/ephemeral-state.test.ts`; run `bun test packages/client/src-ts/service/ephemeral-state.test.ts` |
| Real public-client Chromium lifecycle over actual `pyre serve`, including two same-user identities, propagation, rejection, removals, disconnect/reconnect, restart, durable database survival, and SQLite/IndexedDB non-persistence scans | `packages/client/scripts/ephemeral-serve-proof.ts` and `packages/client/scripts/ephemeral-serve-browser.ts`; run `bun scripts/ephemeral-serve-proof.ts` from `packages/client` |
| Documentation CLI/MCP discovery | `tests/commands.rs::docs_lists_topics`, `tests/commands.rs::docs_prints_requested_topic`, `tests/mcp.rs::docs_are_exposed_as_resources`, `tests/mcp.rs::sync_and_optional_guides_are_discoverable_and_retrievable` |

## Remaining limitations

- V1 requires one authoritative serving process per concrete database. It has no
  distributed owner election, replication, or cross-process convergence.
- There is no separate public Elm ephemeral API. The browser TypeScript client is
  the application-facing surface; generated Elm output remains unchanged.
- Custom servers must supply their own authenticated HTTP/WebSocket/SSE adapter,
  runtime registry, polling, lease scheduling, and retirement. The helper does not
  install routes or infer database residency.
- Delivery is latest-value convergence, not an event log. Intermediate values may
  be coalesced and state cannot be recovered after runtime replacement.
