# TypeScript Identity Boundary

Entity streams and IndexedDB require `SchemaMetadata.tables[table].primaryKey`
with the declared column name and `int` or `uuid` kind. There is no `id` fallback.
Integer identities are JavaScript safe integers; UUIDs are hyphenated hexadecimal
UUID strings. Values are not coerced or case-folded. Invalid identities reject
instead of being filtered out. Entity `change.id` reports the declared identity;
the row payload is preserved without adding or replacing any field.

IndexedDB version 3 stores `{ tableName, identity, updatedAt, row }`, keyed by
`[tableName, identity]`. The IndexedDB key preserves number/string types, and the
existing database-specific cache name preserves database scope. The nested row
keeps domain columns named `tableName`, `identity`, and `id` intact.

Upgrading a v1/v2 cache deletes and recreates `tables`, `syncCursor`, and `meta`
in the version-change transaction. Old rows, cursors, revision, and epoch are
discarded together: the worker must resync rather than reuse progress for an
incompatible base. Connections opened by this version close on version change.
An older tab that does not close its connection can block the upgrade until it
is closed. Epoch reset continues to atomically clear rows/cursor/revision and
install the new epoch.

Current-version caches are validated before either runtime receives initial rows
or progress. A corrupt row or schema-incompatible identity fails client creation;
there is no empty-cache fallback with retained progress. Regenerate client metadata
when upgrading. Other declared key types are marked `unsupported` and fail explicitly.

Query results are projections, not entities. Neither query delta consumer adds
identity metadata or looks up a table from an alias. Positional paths, including
nested relationship paths, work for non-`id` keys and projections without keys.
Existing explicit `#(...)` selectors address a selected property literally named
`id`, not an inferred primary key. Both consumers use the same parser: `#(1)`
matches numeric 1 only; `#("1")` matches string "1" only. Quoted selectors use JSON
string escaping; legacy bare nonnumeric strings remain supported. Worker
producers should use positional paths when there is no selected identity.

This change adds no edit runtime, temporary integer IDs, UUID create optimism,
or UUID allocation API. Applications can allocate UUIDs with `crypto.randomUUID()`
before constructing future generated edits. Existing optimistic updates no
longer invent a row for a missing cached target.

## Verification

Run `bun test ./packages/client/src-ts/` from the repository root. Native browser
persistence tests are opt-in: set `PYRE_PLAYWRIGHT_MODULE` to an installed
Playwright module's absolute `index.mjs` path and run the same command. They use
Chromium and an ephemeral loopback server, covering UUID/integer writes, invalid
identity rejection, reload, paging, table/database isolation, v1/v2 reset, and
epoch reset. Without that environment variable the native-browser test is skipped.
