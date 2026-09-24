# Custom Ephemeral Runtime

This guide covers custom server ownership and transport adaptation. For schema
syntax, generated application types, public browser APIs, reconnect semantics, and
deployment constraints, start with [Ephemeral State](./ephemeral-state.md).

Use `createDatabaseRuntime` when the application owns its database routing and
transport. The helper keeps the application database handle and a per-instance
Rust/WASM `DatabaseRuntime` together, but does not create a registry, timers, or
HTTP endpoints.

```ts
import { createDatabaseRuntime } from "@pyre/server/ephemeral";
import { init } from "@pyre/server/wasm";
import manifest from "../pyre/generated/manifest.json";
import type { StateTypes } from "../pyre/generated/typescript/core/state";

await init();

const runtime = createDatabaseRuntime<typeof database, StateTypes>({
  databaseId,
  database,
  contract: manifest.ephemeral!,
  config: {
    sharedWritePolicy: "serverOnly",
    leaseDurationMs: 30_000,
    downstreamDeliveryCadenceMs: 50,
  },
});

const { connectionId, handle, snapshot } = runtime.join({
  ownerId: authenticatedSessionId,
  trustedSession: resolvedSession,
  writable: true,
});
```

`connectionId` is public routing and snapshot identity. `handle` is an independent
opaque participation capability used by `patchConnection`, participant Shared
writes, lease renewal, polling, resubscription, and leave/unsubscribe. Keep the
handle server-private whenever the adapter can do so. If an HTTP or stream adapter
sends it to a client, treat it as a high-entropy bearer capability: disclose it only
to that participation, require it on every related request, and never publish it in
snapshots or peer changes. The WASM bridge resolves the handle before Rust performs
the applicable owner, epoch, generation, and lease checks. A visible connection ID
alone cannot resolve a private `Participant` or `Subscription`.

Retain one runtime owner for each resident database. The owner decides when to
poll deliveries, sweep leases with `expire`, process transport closure with
`leave` or `unsubscribe`, and retire the runtime with `close`. Always close before
discarding the database. Recreating a runtime creates a fresh epoch, resets
ephemeral state, and fences old connections.

Use `DatabaseRuntimeError.code` for stable error handling and inspect
`validationErrors` for contract validation details. `ContextManager` can resolve
trusted sessions, but it does not own or retain database runtimes.

`PyreSession` is the canonical boundary for application session values: accepted
`DateTime` inputs, including JavaScript `Date` values converted to RFC 3339 by the
JSON/WASM boundary, become integer Unix seconds before `pyre serve` derives
`Connection` state. A custom runtime that calls `join` directly must provide this
canonical trusted-session JSON; in particular, its `DateTime` values are integer
Unix seconds.
