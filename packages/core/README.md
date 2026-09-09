# @pyre/core

Shared TypeScript contracts for Pyre packages.

This package provides common types used by generated code and runtime packages (for example schema metadata and query-shape types).

## Database Context Protocol

`parseContextRequest` and `parseContextMessage` validate version 1 database-context
negotiation/control JSON. They reject unknown envelope fields and malformed
identifiers, versions, variants, and timestamps without coercing values.

These are wire-shape checks, not authentication, session-schema validation,
current-time expiry checks, or a context lifecycle implementation. Parsers return
the validated input without cloning it; callers must not mutate accepted context
data. Runtime integration is specified in
[`docs/spec/database-context-lifecycle.md`](../../docs/spec/database-context-lifecycle.md).

## Install

```bash
bun add @pyre/core
```
