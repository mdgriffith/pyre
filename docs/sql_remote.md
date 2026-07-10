# Remote SQL Execution (libSQL/Turso)

## Compatibility

Turso remote databases do not support `CREATE TEMPORARY TABLE` or `CREATE
TABLE ... AS SELECT`. Generated mutation SQL must not rely on temporary tables.

Turso supports parameterized statements, `INSERT`, `UPDATE`, `DELETE`,
`RETURNING`, atomic batches, and interactive transactions. `last_insert_rowid()`
is connection-scoped, so it must not be used to pass a generated ID between
separate remote statements.

## Mutation Strategy

Single-table mutations should use `RETURNING` as the source of typed mutation
responses and sync affected rows:

- `INSERT ... RETURNING` returns the inserted row.
- `UPDATE ... RETURNING` returns the updated row.
- `DELETE ... RETURNING` returns the deleted row.

The runtime must consume each returned result set before issuing another write,
and it must execute multi-statement mutations in one transaction.

## Nested Inserts

Nested inserts with generated integer IDs require dependent statements:

1. Insert the parent with `RETURNING id`.
2. Bind the returned ID into each child insert.
3. Repeat for descendants.

This is more remote round trips than a single batch, but it removes the
unsupported temp-table statements. The manifest/runtime needs explicit
prior-result bindings to express this safely.

### Pre-assigned IDs

Caller- or runtime-assigned IDs remove the dependency between parent and child
inserts. UUID and ULID IDs are the preferred form: Pyre can assign every ID
before execution, render child foreign keys directly, and send all nested
inserts in one atomic remote batch. This is the future latency optimization for
nested writes.

Auto-increment integer IDs cannot use that optimization without changing the ID
model or allocating IDs ahead of time.

## Migration Plan

1. Move single-table inserts, updates, and deletes to `RETURNING`.
2. Make native and TypeScript runtimes consume returned rows for responses and
   affected-row sync data.
3. Wrap native multi-statement mutations in an interactive transaction.
4. Add manifest support for binding prior `RETURNING` values into nested steps.
5. Remove the legacy temp-table generator once nested inserts use result
   bindings or pre-assigned UUID/ULID IDs.
