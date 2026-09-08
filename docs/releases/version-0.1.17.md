# Pyre 0.1.17

- Render MCP parse and typecheck errors with CLI-style source diagnostics.
- Return invalid namespace errors without terminating the MCP server.
- Report CLI, filesystem, migration, and introspection failures instead of silently ignoring them or reporting success.
- Preserve completed results on partial MCP query failure and include query, execution stage, and migration file context in errors.
