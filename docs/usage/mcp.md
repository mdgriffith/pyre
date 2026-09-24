# MCP Guide

Pyre's MCP server exposes structured project inspection, documentation, database workflows, and dynamic query execution over JSON-RPC.

Use MCP when you want:

- agent-oriented access to Pyre docs and project state
- structured query previews and database inspection
- a tool surface instead of shelling out to CLI commands directly

Use the CLI when you want:

- a direct human workflow in a shell
- simple local iteration
- stdout/stderr behavior that fits normal scripting

## Start The MCP Server

```bash
pyre mcp
```

The transport is newline-delimited JSON-RPC over stdin/stdout.

## Main Tool Groups

### Project and docs

- `pyre_project_info`
- `pyre_schema`
- `pyre_docs`
- resource reads like `pyre://project/schema`

### Validation and generation

- `pyre_check`
- `pyre_format`
- `pyre_generate`
- `pyre_init`
- `pyre_introspect`

### Database workflows

- `pyre_generate_migration`
- `pyre_migrate`
- `pyre_db_status`

### Dynamic query workflows

- `pyre_preview_query`
- `pyre_explain_query`
- `pyre_query`

## CLI To MCP Mapping

```text
pyre check                    -> pyre_check
pyre format                   -> pyre_format
pyre generate                 -> pyre_generate
pyre init                     -> pyre_init
pyre introspect               -> pyre_introspect
pyre migration                -> pyre_generate_migration
pyre migrate                  -> pyre_migrate
project schema read           -> pyre_schema or pyre://project/schema
bundled docs                  -> pyre_docs or pyre://guides/*
```

## High-Level Output Shapes

Many MCP tools are structured wrappers around CLI commands and return a result envelope like:

```json
{
  "ok": true,
  "command": ["pyre", "..."],
  "status": 0,
  "stdout": "...",
  "stderr": "..."
}
```

The query-focused tools return more structured payloads:

- `pyre_preview_query`: generated SQL, input schema, session args
- `pyre_explain_query`: bound values plus query-plan output
- `pyre_query`: actual query or mutation results

## Recommended Agent Workflow

For a new project, a good default read-first flow is:

1. `pyre_project_info`
2. `pyre_schema`
3. `pyre_check`
4. `pyre_db_status` if a database is in play
5. Read the relevant bundled guides before writing integration code (see below).
6. Use `pyre_preview_query` for ad hoc query validation; use `pyre_query` when execution is intended.

For application writes, first choose the operation boundary: generated CRUD/composition, a named command, or a trusted import. MCP dynamic query tools are for inspection and explicit ad hoc execution; they are not the browser submission protocol. Inspect the actual generated modules for field names, required inputs, namespace markers, and result shapes rather than inventing helpers from examples. After schema/query changes, run `pyre_check` and `pyre_generate` and compile the application's TypeScript/Elm integration.

## Bundled Docs And Resources

The MCP server exposes bundled documentation as both tools and resources.

Examples:

- `pyre_docs` with topic `getting-started`
- `pyre_docs` with topic `sync` for the primary client/server integration workflow
- `pyre_docs` with topic `ephemeral-state` for transient Connection/Shared state and lifecycle
- `pyre_docs` with topic `elm-sync` for optional Elm UI and port bridge integration
- `pyre_docs` with topic `multi-database-upgrade` to extend sync to multiple source databases
- `pyre_docs` with topic `server-contexts` for the optional server-side session caching API and invalidation
- `pyre_docs` with topic `schema`
- `pyre_docs` with topic `query` for selects, named transactions, CRUD/composition contracts, and operation results
- `pyre_docs` with topic `seeding` for explicit-session composed writes and the separate import helper
- `pyre_docs` with topic `rust-server` for native manifest execution and sync integration
- `pyre_docs` with topic `migrations`
- `pyre_docs` with topic `serve`
- `pyre_docs` with topic `project-structure`
- `pyre_docs` with topic `troubleshooting`
- `pyre://project/schema`
- `pyre://guides/query`
- `pyre://guides/sync`
- `pyre://guides/ephemeral-state`
- `pyre://guides/elm-sync`
- `pyre://guides/multi-database-upgrade`
- `pyre://guides/server-contexts`
- `pyre://guides/seeding`
- `pyre://guides/rust-server`

Discover topics through `tools/list` (the `pyre_docs` topic enum) or guides through
`resources/list`. Retrieve a guide with `tools/call` using
`{"name":"pyre_docs","arguments":{"topic":"sync"}}`, or
`resources/read` using `{"uri":"pyre://guides/sync"}`. The same topics
are available through `pyre docs` and `pyre docs <topic>`.

Use this reading order for application integration:

1. `schema`: syncability and UUID identity, fields, and server permissions.
2. `query`: named commands versus pure generated builders, explicit atomic submission, nullable inputs, cardinality, and typed indexed results.
3. `sync`: TypeScript client setup, local reads, `client.submit`, custom `$batch` routing, one-worker reconciliation, and recovery limits.
4. `elm-sync` for Elm apps, or `seeding` / `rust-server` for server-owned execution without a browser. Use `multi-database-upgrade` when adding instances and `migrations` when upgrading an existing deployment.

These guides are the canonical application-facing explanations; repository specs contain design/conformance detail. The same existing Markdown files are embedded at CLI build time for both `pyre docs` and MCP, so rebuild/upgrade the executable and restart the MCP process to pick up documentation changes. A checked-out Markdown change alone does not change a running MCP binary.

When generating code or explaining guarantees, preserve these boundaries: synced records use non-null UUID primary keys; builders do not execute until submission; the server owns permissions; one batch targets one database; pending edits are memory-only; unknown outcomes are not automatically replayed; ordinary reconnect does not recover removals missed during delivery gaps. `server-contexts` remains optional server-side session caching, not a required handshake.

## When Not To Use MCP

MCP is not required for normal local Pyre usage.

If you are a human working in a shell, the CLI is usually simpler:

```bash
pyre docs
pyre check
pyre generate
pyre serve db/app.db
```
