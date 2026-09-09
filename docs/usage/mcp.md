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
5. `pyre_preview_query` or `pyre_query` for ad hoc exploration

## Bundled Docs And Resources

The MCP server exposes bundled documentation as both tools and resources.

Examples:

- `pyre_docs` with topic `getting-started`
- `pyre_docs` with topic `sync` for the primary client/server integration workflow
- `pyre_docs` with topic `elm-sync` for optional Elm UI and port bridge integration
- `pyre_docs` with topic `multi-database-upgrade` to extend sync to multiple source databases
- `pyre_docs` with topic `server-contexts` for the optional server-side session caching API and invalidation
- `pyre_docs` with topic `schema`
- `pyre_docs` with topic `query` for selects, mutations, and transaction blocks
- `pyre_docs` with topic `migrations`
- `pyre_docs` with topic `serve`
- `pyre_docs` with topic `project-structure`
- `pyre_docs` with topic `troubleshooting`
- `pyre://project/schema`
- `pyre://guides/query`
- `pyre://guides/sync`
- `pyre://guides/elm-sync`
- `pyre://guides/multi-database-upgrade`
- `pyre://guides/server-contexts`

Discover topics through `tools/list` (the `pyre_docs` topic enum) or guides through
`resources/list`. Retrieve a guide with `tools/call` using
`{"name":"pyre_docs","arguments":{"topic":"sync"}}`, or
`resources/read` using `{"uri":"pyre://guides/sync"}`. The same topics
are available through `pyre docs` and `pyre docs <topic>`.

Read `sync` first for authentication, app-owned database selection, and local query boundaries. Continue with `elm-sync` if the UI uses Elm or `multi-database-upgrade` when adding databases. `server-contexts` is optional and is not a prerequisite for these workflows.

## When Not To Use MCP

MCP is not required for normal local Pyre usage.

If you are a human working in a shell, the CLI is usually simpler:

```bash
pyre docs
pyre check
pyre generate
pyre serve db/app.db
```
