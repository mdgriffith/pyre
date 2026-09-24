# Ephemeral State

Ephemeral state is typed, live, in-memory state scoped to one concrete Pyre
database. Use it for presence, cursors, selections, presentation state, and other
values that must converge between connected clients but must not be persisted.

Ephemeral state is separate from durable sync. It is never written to application
tables, Pyre sync metadata, mutation outboxes, durable cursors, or browser
IndexedDB. A server restart or database-runtime replacement intentionally resets
it.

## Declare State

Pyre recognizes two reserved declarations:

```pyre
session {
    userId Int
}

state Connection {
    userId Int = Session.userId
    cursor Json<Dict<Int>>?
    status String @default("online")
}

state Shared {
    slide Int @default(0)
    selection Json<List<String>>?
}
```

`state Connection` is one complete value per live connection. A connection may
patch only its own writable fields. Session-derived fields such as `userId` are
trusted server values: clients can read them but they are omitted from generated
patch types. Two tabs for one user have different server-assigned Connection IDs
and different values.

`state Shared` is one complete value owned by the database runtime. Trusted server
code can patch it. Participant writes are disabled by default and must be enabled
explicitly with `pyre serve --participant-shared-writes` or the corresponding
custom-runtime policy.

Writable fields must be nullable or have a default. Nullable fields without a
default start as `null`. Patches operate on top-level fields: omission preserves a
field, explicit `null` clears a nullable field, and nested values are complete
replacements rather than recursive patches. Pyre validates the resulting complete
value atomically and rejects unknown, derived, or invalid fields without changing
state.

State fields support Pyre scalar, collection, `Json<T>`, and tagged-union types.
Ephemeral `DateTime` values use whole Unix epoch seconds in generated Rust and
TypeScript state types and on the wire. Trusted application sessions may use the
ordinary accepted DateTime inputs; the server canonicalizes them before deriving
Connection fields.
State is not relational: links, indexes, table directives, queries, and record
permissions do not apply. Run `pyre check` and `pyre generate` after changing a
declaration. Generated artifacts include:

```text
pyre/generated/typescript/core/state.ts
pyre/generated/rust/state.rs
```

## TypeScript Client

Bind public APIs to the generated `StateTypes` bundle:

```ts
import { PyreClient, EphemeralUpdateError } from "@pyre/client";
import type { StateTypes } from "./pyre/generated/typescript/core/state";

const client = await PyreClient.create({
  schema: schemaMetadata,
  server: { baseUrl: "https://api.example.com" },
  cacheNamespace: signedInUserId,
});

await client.syncDatabase(databaseId);

const unsubscribe = await client.subscribeEphemeralState<StateTypes>(
  databaseId,
  (snapshot) => {
    const mine = snapshot.authoritative.connectionId;
    console.log(snapshot.authoritative.connections, snapshot.authoritative.shared);
    console.log(snapshot.authoritative.freshness, mine);
    console.log(snapshot.desired, snapshot.latestOutcome);
  },
);

await client.updateEphemeralConnection<StateTypes>(databaseId, {
  cursor: { x: 12, y: 8 },
});

try {
  await client.updateEphemeralShared<StateTypes>(databaseId, { slide: 2 });
} catch (error) {
  if (error instanceof EphemeralUpdateError) {
    console.error(error.outcome.status, error.outcome.error);
  }
}
```

The `PyreClient` methods are asynchronous because they obtain the
database-specific internal client. `subscribeEphemeralState` immediately reports
the current snapshot and returns an unsubscribe function.

`authoritative` is the latest complete server view. `desired.connection` and
`desired.shared` are the caller's latest local top-level intent; they are not proof
that the server accepted a write. Await each update and handle
`EphemeralUpdateError`. `latestOutcome` is observable status for UI or diagnostics.
Repeated writes to one field collapse to the newest desired value, and stale HTTP
responses cannot discard newer intent.

To subscribe without write intent, configure the client with:

```ts
server: {
  baseUrl: "https://api.example.com",
  ephemeralWrite: false,
}
```

A read-only client receives snapshots, changes, and removals but its public update
methods reject before HTTP. Write intent alone does not grant Shared authority;
the server policy, authenticated owner, current epoch, private participation
capability, connection identity, and lease must all match.

## Cadence And Limits

Client patches are coalesced independently for Connection and Shared. The default
minimum interval is 50 ms per channel and can be set with
`ephemeralMaxUpdateCadenceMs`. Lease renewal is independent of application writes;
`ephemeralLeaseCadenceMs` defaults to 10 seconds. Do not use application activity
as a heartbeat. Ephemeral HTTP work times out after 8 seconds by default; configure
`ephemeralRequestTimeoutMs` when deployment latency requires a different bound.

The server sends complete changed entries and explicit Connection removals.
Downstream cadence coalesces intermediate values while guaranteeing a trailing
latest value for a healthy connection. Participant count, entry size, payload
size, and pending delivery are bounded. Overflow produces an explicit resnapshot
instead of silent divergence; `@pyre/client` performs that authenticated recovery
and marks authority stale while it is in progress.

## Disconnect And Reconnect

On disconnect, `authoritative.freshness.stale` is `true`; the last remote view may
remain available but must not be presented as live. Reconnect creates a fresh
server Connection ID and starts from a complete snapshot. The client then
republishes its current desired Connection fields so current activity follows the
new identity.

Historical patches are never replayed. In particular, stale desired Shared state
is not automatically republished after reconnect, because another participant may
have changed Shared while this client was offline. Durable cache recovery and
ephemeral reconnection are independent.

## Server Setup

After migrate and generate, the built-in server reads the generated state contract
automatically:

```bash
pyre migrate ./db/app.db --push
pyre generate
pyre serve ./db/app.db \
  --dev-session '{"userId":1}' \
  --participant-shared-writes
```

Omit `--participant-shared-writes` for server-owned Shared state. `pyre serve`
exposes the normal SSE stream and authenticated `/ephemeral/*` routes; applications
should use the public client methods rather than construct route bodies or trust a
Connection ID themselves. See [`pyre serve`](./pyre-serve.md) for endpoint and
deployment options.

Signed production sessions have payload shape
`{ session, exp, sessionKey }`. When a schema declares ephemeral state,
`sessionKey` is required and must remain stable across token refresh. It is the
server-authenticated owner identity and is stored only as a hash. A Connection ID
is routing state, not a credential. `pyre serve` sends a separate high-entropy
participation capability only in that connection's `connected` message and requires
it on every ephemeral HTTP request; snapshots and peer changes never contain it.
Static development sessions use one stable
process-local owner; unsigned trusted session JSON cannot preserve ownership when
the complete credential changes.

Custom TypeScript servers use `createDatabaseRuntime` from
`@pyre/server/ephemeral`; custom Rust servers use
`pyre::server::runtime::DatabaseRuntime` with the generated manifest contract and
generated `state.rs` value/patch types at application boundaries. Trusted server
code can call the runtime's server-owned Shared patch API even when participant
writes are disabled. Transport adapters must authenticate joins and every patch,
retain an independent participation capability rather than resolving private
handles from connection IDs, poll bounded deliveries, renew/expire leases, process
transport closure, and close the runtime. See
[Custom Ephemeral Runtime](./ephemeral-runtime.md) for the focused ownership API.

The native runtime deliberately accepts `serde_json::Value` at its contract
boundary; generated Rust types provide the typed application boundary:

```rust
use generated::state::{PatchField, Shared, SharedPatch};

let patch = SharedPatch {
    slide: PatchField::Value(2),
    ..Default::default()
};
runtime.patch_shared(&serde_json::to_value(patch)?)?;

let shared: Shared = serde_json::from_value(
    runtime.snapshot()?.shared.expect("Shared is declared"),
)?;
```

Participant adapters use `join(JoinEvidence)`, retain the returned opaque
`Participant` and `Subscription`, call `patch_connection` or
`patch_shared_from_participant`, deliver `poll` results, and call `leave` when the
transport closes. Server-owned code uses `patch_shared`. Handles, owner IDs,
epochs, and generations remain server-private.

## Lifetime And Deployment

There must be exactly one authoritative `DatabaseRuntime` for each resident
concrete database. `pyre serve` provides that owner for its one configured
database. A custom application must retain the runtime beside its database handle,
not in `ContextManager` and not in a second competing registry.

Shared survives when the last participant leaves while that runtime remains
resident. Closing, evicting, or replacing the runtime discards every Connection,
resets Shared to schema defaults, and creates a fresh opaque ephemeral epoch.
Reopening the same persisted SQLite or libSQL database does not restore any
ephemeral value.

V1 supports one authoritative serving process per database. Load-balancer
stickiness alone is not sufficient; multi-process replication and distributed
ownership are not provided.
