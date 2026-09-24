# Migration Guide

Pyre supports three related but different schema-to-database workflows:

- `pyre migrate <database> --push` updates the database directly from your current schema.
- `pyre migration <name> --db <database>` generates SQL migration files.
- `pyre migrate <database>` applies migration files that already exist on disk.

## Rule Of Thumb

```text
Local iteration, prototypes, throwaway databases:
  pyre migrate <database> --push

Checked-in SQL migrations for teams and deployments:
  pyre migration <name> --db <database>
  pyre migrate <database>
```

## Direct Push Workflow

`--push` is the fastest way to get a local database in sync with the current schema:

```bash
pyre migrate db/app.db --push
```

What it does:

- typechecks your schema
- introspects the target database
- computes the schema diff
- applies the resulting SQL directly
- stores the latest Pyre schema metadata in the database

Use this when you want the shortest local development loop and do not need checked-in SQL migration files.

### Schema-Only Directives

Adding or removing `@immutable` changes Pyre's write typechecking and generated update inputs, but not the physical SQLite schema. It produces no migration SQL and installs no trigger or constraint. A direct push still stores the latest Pyre schema metadata so later dynamic queries use the current immutability rule.

## Checked-In Migration Workflow

Use this when you want explicit SQL migration files under `pyre/migrations/`.

### 1. Generate A Migration

```bash
pyre migration add_users --db db/app.db
```

This creates a timestamped folder containing:

- `migration.sql`
- `schema.diff`

Pyre refuses to generate a new migration if older migration folders have not been applied to the target database yet.

### 2. Apply Existing Migrations

```bash
pyre migrate db/app.db
```

This applies migration folders that already exist on disk.

## New Project Examples

For a brand new local project, the simplest path is:

```bash
pyre migrate db/app.db --push
```

If you want a migration-file-first project from day one, use:

```bash
pyre migration initial --db db/app.db
pyre migrate db/app.db
```

## MCP Equivalents

### Direct push

```json
{
  "name": "pyre_migrate",
  "arguments": {
    "database": "db/app.db",
    "push": true
  }
}
```

### Generate migration files

```json
{
  "name": "pyre_generate_migration",
  "arguments": {
    "name": "add_users",
    "database": "db/app.db"
  }
}
```

### Apply migration files

```json
{
  "name": "pyre_migrate",
  "arguments": {
    "database": "db/app.db"
  }
}
```

### Inspect database status

```json
{
  "name": "pyre_db_status",
  "arguments": {
    "database": "db/app.db"
  }
}
```

## Namespaces

For namespaced schemas, pass `--namespace` in the CLI or `namespace` in MCP arguments so Pyre operates on the intended schema partition.

```bash
pyre migration add_billing_tables --db db/app.db --namespace Billing
pyre migrate db/app.db --namespace Billing
pyre migrate db/app.db --namespace Billing --push
```

## Upgrading Synced Identities And Clients

Synced namespaces require exactly one non-null `Id.Uuid @id` field per record. To keep integer or plain-string keys in a server-only database, declare `@syncable(false)` at namespace scope. That is a query-only execution choice, not compatibility with the UUID-based sync worker.

For an existing synced database:

1. Plan an explicit old-to-new UUID mapping for primary keys and migrate foreign-key values together. Update external references and stored session IDs that refer to those records.
2. Review and apply the data migration; changing the schema type or running `--push` alone does not construct this mapping or preserve relationships automatically.
3. Regenerate TypeScript, Elm, and server manifests with the matching compiler. Deploy schema, server/runtime packages, and generated clients together; old protocols and generated artifacts are not a supported mixed-version upgrade.
4. Reload server schema caches. IndexedDB v4 atomically discards legacy browser caches and their cursors/revisions; the next initialization loads server authority. Pending optimistic edits are memory-only and do not survive reload.

Generated create builders allocate UUIDv7 once. Existing/imported UUIDs may use other versions; do not regenerate valid identities merely to make them v7. Custom primary-key names are supported throughout the client.

For application integration changes, update Elm calls to `Pyre.getResult databaseId queryId queries`, handle `QueryUpdated databaseId queryId`, and retain `databaseId` in bridge messages. Entity consumers must handle `op: 'remove'` as well as `op: 'row'`. See [Elm + Sync](./elm-sync.md) and [Sync Setup](./sync.md).

## Common Mistakes

- Generating a migration and then running `pyre migrate --push`.
  `--push` skips migration files entirely.
- Expecting `pyre migrate <database>` to create migration folders.
  It only applies folders that already exist.
- Forgetting `--namespace` for multi-namespace projects.
