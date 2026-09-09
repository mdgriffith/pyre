# `pyre serve` Built-In Server

## Goal

`pyre serve` starts a small built-in Rust HTTP server for local development, demos, and simple single-database deployments.

It exposes the standard Pyre query, mutation, catchup, and live-sync endpoints without requiring an application to write its own server glue.

The command is intentionally narrow. It is not an authentication framework, application router, multi-tenant gateway, or multi-database orchestration layer.

## Non-Goals

- No multi-database routing in v1.
- No schema-family selection in v1.
- No built-in login, user database, OAuth, cookie sessions, or role management.
- No arbitrary app authorization logic beyond Pyre session validation and Pyre permissions.
- No session-id lookup store such as Redis, Postgres, SQLite auth tables, or external introspection.
- No JWT/JWKS validation in v1 unless a later spec explicitly adds it.

## Command

```bash
pyre serve <database>
```

Examples:

```bash
pyre serve ./db/app.db
pyre serve ./db/app.db --dev-session '{"userId":1,"role":"admin"}'
pyre serve libsql://example.turso.io --auth $TURSO_AUTH_TOKEN --dev-session '{"userId":1}'
```

Production-like usage behind an authenticated upstream:

```bash
pyre serve ./db/app.db \
  --session-header x-pyre-session \
  --session-secret $PYRE_SESSION_SECRET
```

## Options

```text
pyre serve <database>
  --auth <TOKEN>
  --host <HOST>
  --port <PORT>
  --generated <DIR>
  --database-id <ID>
  --session-header <HEADER>
  --session-secret <SECRET>
  --dev-session <JSON>
  --cors-origin <ORIGIN>
  --page-size <N>
  --allow-unsafe-dev-session
  --allow-unsafe-unsigned-session
```

Defaults:

```text
--host 127.0.0.1
--port 3000
--generated pyre/generated
--database-id default
--page-size 1000
```

`--page-size` is capped by the server runtime's maximum page size.

`--auth` is database authentication, such as a Turso/libSQL auth token. It is not end-user authentication.

`--session-header` defaults to `x-pyre-session` only when trusted-header mode is enabled by `--session-secret` without an explicit header name. Otherwise, omitting both `--session-header` and `--session-secret` means no request session header is accepted.

## Database Scope

V1 supports exactly one source database per server process.

The database may be:

- A local SQLite/libSQL file path.
- A libSQL/Turso URL.
- An environment variable reference if the existing CLI database argument conventions support it.

The server still includes a `databaseId` in sync responses and live messages so it remains compatible with the existing client protocol. For v1, this is a fixed server-wide value from `--database-id`.

Requests that provide a conflicting `databaseId` fail.

## Generated Artifacts

`pyre serve` expects generated server artifacts to exist before startup:

```text
pyre/generated/manifest.json
pyre/generated/rust/server.rs, if needed by the Rust implementation
```

The server should not run `pyre generate` implicitly in v1. Failing fast keeps startup behavior predictable and avoids writing files from a command whose primary job is serving traffic.

If generated artifacts are missing or stale, the error should tell the user to run:

```bash
pyre generate
```

## Endpoints

The built-in server exposes the same logical routes expected by `@pyre/client`.

```text
GET  /health
POST /sync
GET  /sync/events
POST /db/:queryId
```

### `GET /health`

Returns basic server readiness.

```json
{
  "ok": true,
  "databaseId": "default"
}
```

### `POST /sync`

Runs catchup sync.

Request body:

```json
{
  "databaseId": "default",
  "syncCursor": {
    "tables": {}
  }
}
```

Response is the runtime catchup result and includes `databaseId` and, when available, `serverRevision`.

### `GET /sync/events`

Opens a live sync stream.

The initial event confirms the connection and provides a server-assigned `sessionId`:

```json
{
  "type": "connected",
  "sessionId": "...",
  "connectionId": "...",
  "databaseId": "default"
}
```

`sessionId` and `connectionId` are the same value in v1. `connectionId` is included for compatibility with the current client mutation-origin protocol.

Live mutation messages use the existing sync protocol:

```json
{
  "type": "delta",
  "serverRevision": 12,
  "databaseId": "default",
  "data": []
}
```

When the live delta is too large or fanout is too broad, the server may send:

```json
{
  "type": "syncRequired",
  "serverRevision": 12,
  "databaseId": "default"
}
```

### `POST /db/:queryId`

Runs a generated Pyre query or mutation.

Request body is the generated query input JSON.

For mutations, the server calculates live deltas after successful execution, sends them to connected sessions, and returns the mutation response envelope with `serverRevision` when available.

## Session Model

`pyre serve` is auth-neutral but session-aware.

It requires a full Pyre session object, not a session id.

The session object is validated against the generated Pyre `session { ... }` schema and then used for:

- Query and mutation session arguments.
- Sync permission evaluation.
- Live delta filtering.

The built-in server does not resolve session ids. That would require configuring a session store and would turn `pyre serve` into an application auth server.

## Dev Session Mode

For local development, use `--dev-session`:

```bash
pyre serve ./db/app.db --dev-session '{"userId":1,"role":"admin"}'
```

Every request and live connection uses that same session object.

If the Pyre schema has no session fields, this works without any session flags:

```bash
pyre serve ./db/app.db
```

If the schema requires session fields and no session source is configured, startup fails with a helpful message that includes the session schema and an example:

```text
This Pyre schema requires session data.

For local development:
  pyre serve ./db/app.db --dev-session '{"userId":1}'

For production, run behind authenticated upstream infrastructure and pass:
  --session-header x-pyre-session --session-secret <secret>
```

`--dev-session` is only allowed on loopback bind addresses unless `--allow-unsafe-dev-session` is passed.

## Trusted Header Mode

For production-like usage, an upstream application or reverse proxy authenticates the user, constructs the Pyre session object, and forwards it to `pyre serve` in a trusted header.

Example decoded session:

```json
{
  "userId": 123,
  "role": "member"
}
```

The header carries the full session object, encoded as base64url JSON.

```http
x-pyre-session: eyJ1c2VySWQiOjEyMywicm9sZSI6Im1lbWJlciJ9
```

Unsigned trusted headers are only allowed on loopback bind addresses unless `--allow-unsafe-unsigned-session` is passed.

### Signed Header Mode

When `--session-secret` is configured, the header is signed.

Payload before encoding:

```json
{
  "session": {
    "userId": 123,
    "role": "member"
  },
  "exp": 1730000000
}
```

Wire format:

```text
base64url(payload).base64url(hmac_sha256(payload, secret))
```

Rules:

- The signature is calculated over the exact first segment bytes.
- `exp` is required for signed headers.
- Expired headers are rejected.
- The session object inside `session` is validated against the Pyre session schema.

Key rotation is out of scope for v1. A later version may support multiple secrets or `kid` headers.

## Upstream Requirements

When using trusted header mode, the upstream must:

- Authenticate the caller using the app's normal mechanism.
- Authorize the caller at the app boundary as needed.
- Remove any client-supplied session header before setting its own.
- Construct the Pyre session from server-owned authenticated state.
- Keep `pyre serve` inaccessible except through trusted infrastructure, unless signed headers are required.

`pyre serve` treats the configured session header as authority.

## Safety Defaults

The default bind address is `127.0.0.1`.

If `--host` is non-loopback, startup fails unless one of these is true:

- `--session-secret` is configured.
- The schema has no session fields and no session header is accepted.
- `--allow-unsafe-dev-session` is explicitly passed with `--dev-session`.
- `--allow-unsafe-unsigned-session` is explicitly passed with unsigned `--session-header`.

Unsafe modes should print a prominent warning at startup.

## CORS

By default, CORS should be friendly for local development on loopback and conservative elsewhere.

`--cors-origin` may be provided one or more times to allow browser clients from specific origins:

```bash
pyre serve ./db/app.db --cors-origin http://localhost:5173
```

Wildcard CORS with credentials should not be enabled by default.

## Client Configuration

For local dev with default routes:

```ts
const client = await PyreClient.create({
  schema,
  server: {
    baseUrl: "http://localhost:3000",
  },
});
```

For apps where upstream auth adds headers/cookies before requests reach `pyre serve`, the browser should usually not send `x-pyre-session` directly. The upstream should derive and set that header server-side.

The browser has no `Session` configuration, `connect` session result, `setSession` API, or local `$session` substitution. Explicit `Session`-dependent local query filters must fail with a clear error directing the app to use ordinary query inputs or explicitly execute on the authenticated server. There is no automatic server fallback. Inputs filter already-authorized data, not grant permissions; queries whose only `Session` usage is in server schema permissions remain normal local queries. This does not change the server session model, permissions, auth cookies, or routes, and does not solve permission-contraction cache cleanup.

## Error Behavior

Startup errors should be explicit for:

- Missing generated artifacts.
- Database connection failure.
- Missing Pyre schema/migrations in the target database.
- Session schema requires values but no session source is configured.
- Unsafe non-loopback auth configuration.

Runtime errors should avoid leaking secrets, auth tokens, or full session payloads.

## Implementation Plan

1. Add `Serve` to the Rust CLI.
2. Add an internal HTTP server dependency and route layer.
3. Load `pyre/generated/manifest.json` at startup.
4. Connect to the single configured database.
5. Load the Pyre schema context from the database.
6. Implement session extraction for `--dev-session`, unsigned trusted header, and signed trusted header.
7. Implement `POST /sync` using `SyncServer::catchup`.
8. Implement `GET /sync/events` using SSE and an in-memory connected-session registry.
9. Implement `POST /db/:queryId` using `pyre::server::query::run` and `SyncServer::calculate_deltas` for mutations.
10. Add CLI/help text and safety validation.
11. Add tests for CLI parsing, session extraction, unsafe mode rejection, catchup, mutation fanout, and error messages.

## Open Questions

- Should endpoint paths be configurable, or should v1 keep fixed paths?
- Should signed session headers allow a small clock-skew leeway?
- Should `--dev` exist as shorthand for loopback, permissive local CORS, and helpful session errors?
- Should startup verify that generated artifacts match the database schema hash?
