# Pyre

A schema and query language for building typesafe persistence using SQLite.

## Repository Map

- `src/` - Core Rust engine (parser, typechecker, SQL/code generation, sync)
- `pyre-cli/` - CLI entrypoint and subcommands (`pyre check`, `pyre generate`, `pyre migrate`, ...)
- `tests/` - Integration-style Rust tests, grouped by feature area (`parsing`, `queries`, `formatting`, ...)
- `packages/` - TypeScript runtime packages (`@pyre/core`, `@pyre/server`, `@pyre/client`)
- `wasm/` - WASM build and bindings
- `playground/` - Example projects and local experimentation setups
- `docs/usage/` - End-user setup and usage guides
- `docs/dev/` - Build and contributor-focused docs
- `docs/spec/` - Language and SQL generation specs

## Pre-requisites

You'll need Rust, Cargo and iconv installed.

Or you can use [devbox](https://www.jetify.com/devbox) to get all the right deps without polluting your system:

```
devbox shell
```

## Getting Started

```
cargo run
```

Useful CLI docs commands:

```bash
pyre docs
pyre docs getting-started
pyre docs sync
pyre docs elm-sync
pyre docs schema
pyre docs query
pyre docs serve
pyre docs mcp
```

Built-in docs are also available under `docs/usage/`.

Recommended reading order:

1. [Getting started](docs/usage/getting-started.md): set up a project and generate your first schema and queries.
2. [Sync](docs/usage/sync.md): connect your app and server, select databases, and query local data.
3. [Elm integration](docs/usage/elm-sync.md), if your UI uses Elm.

Keep the [query](docs/usage/query.md) and [schema](docs/usage/schema.md) references handy as you build. For additional workflows, see [migrations](docs/usage/migrations.md), the [built-in server](docs/usage/pyre-serve.md), and [multi-database integration](docs/usage/multi-database-upgrade.md). [Server contexts](docs/usage/server-contexts.md) is an optional server-side session caching API, not a prerequisite for sync.

## Examples

- `playground/simple/`
- `playground/sync/`
