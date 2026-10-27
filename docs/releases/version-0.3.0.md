# Pyre 0.3.0

## Schema loading and migrations

- Store namespace and external identity metadata so each database can load its schema independently, including shared session types and references to other namespaces.
- Refresh required identity metadata even when no SQL migrations are pending, and retain historical identity fields while resolving snapshots during migration.
- Reject unsupported automatic column changes and require confirmation, or `--force`, for destructive table removals with `migrate --push`.
- Improve database and migration diagnostics, including credential redaction and rollback reporting.

## Identity and query improvements

- Add `Sequence.Int` for server-assigned insertion ordering while preserving UUID identities.
- Fix UUID reference code generation and preserve typed query results across generated runtimes.

## Upgrading

- Upgrade the compiler and all Pyre runtime packages together, then regenerate application code.
- Run `pyre migrate <database> --namespace <namespace>` with the complete project schemas available to refresh metadata for existing databases.
- Changes to existing column types and adding `Sequence.Int` to existing records require explicit SQL migrations.

See the [migration guide](../usage/migrations.md) for details.
