# Pyre - 0.1.13

`0.1.13` adds singleton records and atomic transaction blocks across parsing, typechecking, SQL generation, generated clients, native and WASM runtimes, sync, and MCP workflows. It also formalizes immutable fields so migrations and generated operations consistently reject unsupported writes while preserving valid create-time values.

Sync pagination now uses composite cursors to prevent records with identical timestamps from being skipped during catch-up. This release expands migration, query, sync, generated-client, and cross-runtime coverage for these behaviors.
