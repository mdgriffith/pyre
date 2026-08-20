# Pyre - 0.1.12

`0.1.12` adds a generated, schema-typed Rust seed helper for fixture and import workflows. It supports nested linked records, SQLite defaults and generated IDs, nullable fields, JSON, custom types, DateTime values, and multiple schema namespaces, while applying each seed operation atomically with path-specific errors.

Nested query and mutation inserts are now atomic across the CLI, MCP, and native Rust server paths, preventing partial writes when a later statement fails. This release also preserves nested tagged-union subtypes when reconstructing JSON query results and improves client diagnostics with concise, structured logs for application, IndexedDB, and server activity.
