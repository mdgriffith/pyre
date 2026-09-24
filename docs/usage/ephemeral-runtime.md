# Custom Ephemeral Runtime

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

const { connectionId, snapshot } = runtime.join({
  ownerId: authenticatedSessionId,
  trustedSession: resolvedSession,
  writable: true,
});
```

Only `connectionId` crosses the application boundary. Participant and
subscription capabilities stay private in WASM. Handle-based operations resolve
those capabilities before Rust performs the applicable owner, epoch, generation,
and lease checks.

Retain one runtime owner for each resident database. The owner decides when to
poll deliveries, sweep leases with `expire`, process transport closure with
`leave` or `unsubscribe`, and retire the runtime with `close`. Always close before
discarding the database. Recreating a runtime creates a fresh epoch, resets
ephemeral state, and fences old connections.

Use `DatabaseRuntimeError.code` for stable error handling and inspect
`validationErrors` for contract validation details. `ContextManager` can resolve
trusted sessions, but it does not own or retain database runtimes.
