# Server Database Contexts

## Scope

The context manager is a thin server-only adapter between an application's
existing authentication system and Pyre's existing database executor. It caches
the effective session for one authenticated application session and one database.
It is not a new login system, database driver, or client lifecycle.

The application remains the authority for identity, membership, roles, database
access, and session expiration. Pyre does not persist login credentials or derive
authorization from browser-supplied session values.

## Application Boundary

For each request, the application authenticates through its existing session
system and supplies a stable session key plus the requested database ID. The key
must distinguish independently revocable sessions, not merely users. It is a
server-side cache key, not a new bearer credential or client context token.

The application supplies a resolver that authorizes the requested database and
returns the effective Pyre session for that database. A user can have different
session values in different databases. Resolution must not mutate a shared
current-database field in the application login session.

Database lookup is supplied separately through the server's `getDatabase`
integration. The application owns routing and database handles; authorization
does not let a caller substitute another database after resolution.

Unknown or inaccessible databases must fail without exposing private membership
or routing details. Resolver failures do not install a usable cache entry.

## Cache and Lifetime

Cache entries are keyed by the pair `(session key, database ID)`. Keep both parts
distinct rather than joining them with an ambiguous delimiter. A cache hit for
one pair must never reuse another pair's effective session.

Entries have bounded lifetimes. Expired entries require fresh application
resolution; a cache hit must not indefinitely extend an old authorization grant.
The application must not reuse a session key for a different authentication
context while an earlier entry can remain valid.

Application authority changes require invalidating the affected cached contexts.
Logout and revocation must invalidate the applicable session; database-specific
membership changes must invalidate the applicable database authorization.
Invalidation must prevent an older pending resolution from reinstalling stale
authority. A retained context must not bypass expiration or invalidation.

This cache is local to its manager. Applications with multiple server instances
must propagate invalidation or accept their configured cache lifetime as the
freshness bound. There is no durable or distributed session registry here.

Both implementations use monotonic cache lifetimes and cap entries at 1,024.
TypeScript coalesces pending lookups for a pair and permits 64 unsettled
resolutions. Rust permits 128 pending resolutions; concurrent misses can resolve
more than once but reuse the installed context. Invalidated pending work retains
its reservation until settlement (or future cancellation in Rust). Expiry is
checked lazily, with no cleanup threads or timers. Applications still authenticate
each request and invalidate contexts when authority changes; cache TTL is not a
replacement for login expiration checks.

## Scoped Execution

The server resolves a context for the authenticated session and requested
database, then executes application work through `context.run(operation, input)`.
The callback receives the resolved database and effective server session and
passes them to the existing executor.

TypeScript exposes `createContextManager` from `@pyre/server/context`:

```ts
const contexts = createContextManager({
  getSessionKey: (session: LoginSession) => session.id,
  resolveSession: (session, databaseId) =>
    membership.resolveSession(session.userId, databaseId), // null denies access
  getDatabase: databaseId => databases.get(databaseId),
  maxAgeMs: 60_000,
});

const context = await contexts.get(authenticatedSession, databaseId);
const result = await context.run(
  (db, session, input) => run(db, queries, operationId, input, session),
  input,
);

// After committing membership changes or revoking a login:
contexts.invalidate(authenticatedSession.id, databaseId);
contexts.invalidateSession(authenticatedSession.id);
```

`run` above is the existing `@pyre/server/query` executor. Database lookup and
membership are application functions, not new services supplied by the manager.
Global and resolved sessions must remain immutable; operations must not mutate
the cached session. No JSON conversion or browser session installation is needed.

Rust exposes `ContextManager` and `Config` under `pyre::server::context`, with
`get_session_key`, `resolve_session`, `get_database`, and `max_age` callbacks/config.
`get(&session, database_id)` returns a shared context. Its `run(operation, input)`
passes an application database handle and `Arc<DatabaseSession>` to an async
operation. Database lookup returns `Result<Handle, E>`; lookup errors do not invoke
the operation. `RunError::Operation(E)` preserves executor errors, while
`RunError::StaleExecution` reports invalidation/expiry after invocation without
claiming the operation did not commit. `tests/context_manager.rs` demonstrates
the ordinary Rust query executor with application-owned connection locking.

Conceptually, the integration is:

1. Authenticate the request using the application's session system.
2. Resolve or reuse the context keyed by session key and database ID.
3. Obtain the requested database through the server's `getDatabase` integration.
4. Run the existing executor inside `context.run`, using the supplied session.
5. Return the ordinary executor result through the application's existing handler.

The callback boundary avoids building a second query or catchup executor into
the manager. Existing generated query metadata, input/session validation, SQL,
and permission enforcement remain unchanged. The manager does not add a separate
operation allowlist or reinterpret the schema.

Scoped execution checks context validity around asynchronous work. Invalidation
does not cancel SQL already dispatched, undo a committed mutation, or recall a
result already returned to application code. A stale completion must not be
treated as proof that a write did not happen or retried automatically.

The application owns callback behavior and must use the supplied database and
session together. The manager is not a sandbox for untrusted server code.

## Unchanged Boundaries

No effective session or browser projection is sent to clients by this mechanism.
There are no context negotiation messages, schema artifacts, schema fingerprints,
new routes, headers, control frames, or wire protocol changes.

The manager does not create, pool, serialize access to, or close database
connections. It does not manage live connections, sync subscriptions, publication
queues, transport delivery, client caches, or browser readiness.

The application may supply an accessible database list to its client separately
from the active sync set selected with `setSyncedDatabases`. Accessibility does
not start sync and is not an authorization grant. This introduces no enumeration
API and does not change existing multi-database routing.

## Verification

Manager tests should establish isolation by session key and database ID, cache
reuse, fresh resolution after expiry, denial and resolver failure handling,
invalidation during pending resolution and execution, and use of the existing
executor with the resolved session. These are server mechanism checks, not
claims about browser lifecycle or network delivery.
