# Pyre 0.2.0

## Local edit API

- Generate typed create, update, and delete builders for TypeScript and Elm. Submit edits explicitly with `client.submit` or `Db.Edit.submit`, with optimistic local updates reconciled through the existing sync engine.
- Compose generated edits and named mutations into ordered, atomic batches. The server validates and executes compiled operations under the authenticated session, rolling back the batch if a generated write does not affect exactly one row.
- Return typed, indexed operation results, with generated UUID create builders allocating UUIDv7 identities.
- Support explicit-session composed writes for seeding and server-owned operations.

## Sync improvements

- Preserve readers and pending writes across ordinary reconnects, and recover missed removals during catchup.
- Reconcile pending field edits, creates, and deletes with authoritative sync results, including removing rejected optimistic rows.
- Enforce original row visibility when authorizing incremental removals and authorize HTTP sync origins.

## Upgrading

- **Synced records now require non-null UUID primary keys.** Query-only namespaces can use `@syncable(false)` to retain integer or plain-string primary keys.
- Upgrade the compiler and all Pyre runtime packages together, then regenerate application code.
- Local TypeScript composed writes and catchup require file-backed libSQL; private in-memory databases are rejected.

See the [generated CRUD and composed operations guide](../usage/query.md#generated-crud-and-composed-operations), [sync setup](../usage/sync.md#7-submit-composed-writes), and [Elm integration guide](../usage/elm-sync.md) for usage.
