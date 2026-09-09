# `pyre serve`

`pyre serve` starts Pyre's built-in single-database HTTP server.

It is useful for local development, demos, and simple deployments where you want Pyre to provide the standard client/server endpoints without writing custom server glue.

## Quick Start

Generate Pyre artifacts first:

```bash
pyre generate
```

Run migrations or push your schema to a local database:

```bash
pyre migrate ./db/app.db --push
```

Start the server:

```bash
pyre serve ./db/app.db
```

The server listens on:

```text
http://127.0.0.1:3000
```

## Session Data In Development

If your schema has no `session { ... }` block, no session setup is required.

If your schema requires session fields, pass a static development session:

```bash
pyre serve ./db/app.db --dev-session '{"userId":1,"role":"admin"}'
```

Every request and live sync connection uses that same session.

`--dev-session` is intended for local development. It is only allowed on loopback bind addresses unless you explicitly pass `--allow-unsafe-dev-session`.

## Remote libSQL/Turso

For a remote database, pass the database URL and database auth token:

```bash
pyre serve libsql://example.turso.io \
  --auth $TURSO_AUTH_TOKEN \
  --dev-session '{"userId":1}'
```

`--auth` authenticates to the database. It is not end-user authentication.

## Production Auth Model

`pyre serve` does not implement login, users, OAuth, cookies, or role management.

For production-like use, put it behind an authenticated upstream server or reverse proxy. The upstream authenticates the caller, builds the Pyre session object from server-owned state, and forwards that full session object to `pyre serve` in a trusted header.

Signed session header mode:

```bash
pyre serve ./db/app.db \
  --session-header x-pyre-session \
  --session-secret $PYRE_SESSION_SECRET
```

The signed header format is documented in the [`pyre serve` spec](../spec/pyre-serve.md).

The upstream must remove any client-supplied `x-pyre-session` header before setting its own.

## Client Setup

With default endpoint paths:

```ts
const client = await PyreClient.create({
  schema,
  server: {
    baseUrl: "http://127.0.0.1:3000",
  },
});
```

No browser session configuration is needed, including when the server uses `--dev-session`. Server sessions, schema permissions, and application authentication cookies are unchanged. Explicit `Session`-dependent local filters must be rejected clearly: use ordinary inputs to filter already-authorized data (not grant permissions), or explicitly execute on the authenticated server. There is no automatic server fallback. Queries whose only `Session` usage is in server schema permissions remain normal local queries.

If your browser app runs on a different origin, allow it with CORS:

```bash
pyre serve ./db/app.db \
  --dev-session '{"userId":1}' \
  --cors-origin http://localhost:5173
```

## Endpoints

`pyre serve` exposes:

```text
GET  /health
POST /sync
GET  /sync/events
POST /db/:queryId
```

These are the default endpoints expected by `@pyre/client`.

## Options

```text
pyre serve <database>
  --auth <TOKEN>
  --host <HOST>                       default: 127.0.0.1
  --port <PORT>                       default: 3000
  --generated <DIR>                   default: pyre/generated
  --database-id <ID>                  default: default
  --session-header <HEADER>
  --session-secret <SECRET>
  --dev-session <JSON>
  --cors-origin <ORIGIN>
  --page-size <N>                     default: 1000
  --allow-unsafe-dev-session
  --allow-unsafe-unsigned-session
```

## Limits

- One database per server process.
- SSE only for live sync.
- No built-in login or user/session store.
- Generated artifacts must already exist. Run `pyre generate` before `pyre serve`.
