# Pyre 0.1.18

- Add optional Rust and TypeScript server context managers for cached per-login, per-database session resolution, with expiry and explicit invalidation.
- Remove client-side sessions and keep session resolution and authorization on the server.
- Guard local query execution for queries that require server-side session data.
- Update sync, query, and MCP documentation and add a server contexts integration guide.
