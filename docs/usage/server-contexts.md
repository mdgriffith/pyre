# Server Contexts Guide

This optional server-only adapter caches effective Pyre session resolution per
authenticated login and database, not connections. Use it when a custom TypeScript
or Rust server benefits from that cache. It is not required for server authentication
or sync, and is not a login system or a client lifecycle. Start with
[Sync Setup](./sync.md) for ordinary server authentication and browser sync.

## TypeScript Custom Server

This integration factory plugs into an existing authenticated handler. Supply your
application's membership resolver and database registry and the generated `queries`
map from `./pyre/generated/typescript/server`. Adapt `DatabaseSession` to the shared
`pyre/session.pyre` definition. No particular HTTP endpoints are required.

```typescript
import type { Client } from "@libsql/client";
import { createContextManager } from "@pyre/server/context";
import { run, type QueryMap } from "@pyre/server/query";

type LoginSession = Readonly<{ id: string; userId: number }>;
type DatabaseSession = Readonly<{ userId: number }>;

export function integratePyre(app: {
  authenticate(request: Request): Promise<LoginSession>;
  resolveSession(login: LoginSession, dbId: string): Promise<DatabaseSession | null>;
  getDatabase(dbId: string): Promise<Client>;
}, queries: QueryMap) {
  const contexts = createContextManager({
    getSessionKey: (session: LoginSession) => session.id,
    resolveSession: (session, dbId) => app.resolveSession(session, dbId),
    getDatabase: dbId => app.getDatabase(dbId),
    maxAgeMs: 60_000,
  });

  return {
    contexts,
    async execute(request: Request, dbId: string, operationId: string,
      input: Record<string, unknown>) {
      const session = await app.authenticate(request);
      const context = await contexts.get(session, dbId);
      return context.run(
        (db, dbSession, args: Record<string, unknown>) =>
          run(db, queries, operationId, args, dbSession),
        input,
      );
    },
  };
}
```

Instantiate once for the server's intended cache scope. Call `execute` from your
existing handler with parsed input and the generated operation ID, not the Pyre
source operation name. Authentication must reject expired/revoked logins on every
request. `resolveSession` checks database membership and returns `null` to deny;
unknown/inaccessible IDs should not reveal private routing or membership details.
`getDatabase` is a server-owned lookup called on each run after authorization.
The ordinary executor still validates inputs/session and enforces permissions.
Return its ordinary response using your existing handler's error handling; the
manager does not publish sync messages or manage live connections.

Use a trusted, independently revocable login/session ID for `getSessionKey`, not
a user ID shared across logins or a client-supplied key. Keep both global and
resolved sessions immutable, including inside operations. Do not mutate a shared
current-database field. Always use the callback's database and session together.
Neither session nor context is serialized to the browser.

## Rust Counterparts

Use `pyre::server::context::{ContextManager, Config}`. Construct with
`ContextManager::new(Config { get_session_key, resolve_session, get_database, max_age })`:

- `get_session_key` returns a trusted `String` login key.
- `resolve_session` returns `SessionFuture<'a, S>` (a boxed async `Option<S>`); `None` denies access.
- `get_database` takes the database ID and asynchronously returns `Result<Handle, E>`.
- `max_age` is a positive `std::time::Duration`.

`contexts.get(&session, db_id).await?` returns a shared context.
`context.run(operation, input).await` passes the application handle,
`Arc<DatabaseSession>`, and input to an async operation returning `Result<T, E>`.
Use the existing `pyre::server::query::run` executor there. The application owns
connection locking; waits for locks inside the callback count as execution.
See `tests/context_manager.rs` for an executor integration example.

## Lifetime And Invalidation

Entries are keyed by `(session key, database ID)`. The required finite positive
`maxAgeMs` (Rust `max_age`) uses monotonic time starting before resolution;
cache hits do not extend it. Expiry is lazy, with no cleanup timers. Both managers
cap allocations at 1,024; TypeScript permits 64 unsettled resolutions and Rust 128.
Denials and failed resolutions do not install usable contexts.

After authority changes, call `contexts.invalidate(sessionKey, dbId)` for a pair,
`contexts.invalidateSession(sessionKey)` for logout/revocation, or
`contexts.invalidateDatabase(dbId)` for a database-wide change. Rust uses
`invalidate_pair`, `invalidate_session`, and `invalidate_database`. Invalidation fences
pending resolution and retained contexts. Do not reuse a login key for different
authority while old entries can remain valid. Propagate invalidation across server
instances or explicitly accept the configured TTL as the cache freshness bound.
TTL does not replace authentication or membership-change handling.

`stale_context` (Rust `RunError::Stale`) means the operation was not invoked.
`stale_execution` (Rust `RunError::StaleExecution`) means authority expired or was
invalidated after invocation: the outcome is unknown and a mutation may have
committed, even if the callback failed. Never automatically retry or replay stale
execution. The application must reconcile the outcome. Rust preserves ordinary
lookup/executor failures as `RunError::Database(E)` / `RunError::Operation(E)`.

The application owns authentication, membership, database handles, connection
locking, routing, subscriptions, and delivery. Invalidation cannot cancel dispatched
SQL, undo writes, recall returned data, or erase browser caches. There are no cache
revocation guarantees. This adapter adds no routes, transport headers, or protocol.
