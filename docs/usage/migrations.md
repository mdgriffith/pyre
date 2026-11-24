# Migrations

Preserved databases run finalized migrations. A bundle contains the SQL, the complete target schema, and any assertions. A database applies the bundle using its stored catalog binding, so the working schema is not needed at deployment.

## Create and apply a migration

```sh
pyre migration create initial
pyre db init app.db

# After editing the working schema:
pyre migration create add_notes
pyre migration check
pyre migrate app.db
```

Use `--namespace Billing` when working with a named namespace and `--migration-dir` to set a non-default filesystem location. The chain is ordered by predecessor hashes; folder names are for people. Commit finalized folders with the exact file bytes intact.

## Drafts and fixture checks

`migration create NAME --draft` prepares an editable draft even when the schema is unchanged. Automatic generation compares against the latest finalized schema. Changes requiring explicit SQL, and candidates that fail replay, remain drafts. After editing `migration.sql` and optionally adding `assertions/*.sql`, run:

```sh
pyre migration check rewrite_notes --fixtures predecessor.json
pyre migration finalize rewrite_notes --grace 2d
```

Each assertion is one read-only `SELECT`; it passes only if it returns no rows. Fixture files use the same table-keyed JSON format as `db seed`. Without fixtures, replay checks the structure and executability on a fresh database; it does not prove the migration is correct for production data.

If the catalog head changes before a draft is finalized, regenerate against the new head, carry forward handwritten SQL and assertions, and check again. `migration unfinalize NAME` returns an unshared filesystem head to a draft when there is no known application or rollout evidence. Use a follow-up migration after sharing or applying it.

## Apply and recover

Stop serving the target and drain in-flight work before running `pyre migrate`. Restart with compatible handlers after verifying the installed state. Each migration commits its SQL, assertions, installed schema, and receipt together. If one fails, earlier migrations stay applied. Data-changing SQL on a synced target changes its sync epoch so reconnecting clients rebuild their caches.

If the commit outcome is unknown, keep the database out of service. On a fresh connection, `pyre migrate app.db --inspect NAME` confirms application by receipt or identifies that the target is still at the expected predecessor. Resolve divergence or failed inspection before retrying.

Migration SQL supports transactional table/index DDL and `INSERT`, `UPDATE`, and `DELETE`. It rejects transaction control, `PRAGMA` (including foreign-key toggles in rebuild scripts), writes outside the main database, and references to reserved storage. V1 write migrations do not support targets with triggers or virtual tables.

## Adopt, audit, or use a disposable database

```sh
pyre db audit app.db
pyre db baseline app.db --at initial
pyre db policy app.db --set preserved
```

Audit compares live structure with the installed schema. Baseline adopts an existing database only when its structure matches the named bundle; it does not execute the bundle or fabricate receipts. Promotion requires matching finalized provenance or an explicit baseline and leaves application rows untouched.

For an explicitly disposable development database, use `pyre db init dev.db --disposable` followed by `pyre migrate dev.db --push`. A database already bound to a finalized chain uses finalized migrations even if its policy is disposable. Use `db reset` or `db reseed` only for deliberate replacement of a disposable database.

## Target-local catalog

```sh
pyre db init app.db --migration-catalog database
pyre migration create add_notes --database app.db --schema target.pyre --draft
pyre migration show-draft add_notes --database app.db
pyre migration edit-draft add_notes updated-draft.json --database app.db
pyre migration check add_notes --database app.db
pyre migration finalize add_notes --database app.db
pyre migrate app.db
```

Target-local init generates and checks a root from the working schema, then installs and stores it on an empty target in the same transaction. It does not read the filesystem migration catalog. Database-catalog authoring can consume a standalone schema plus optional `--external-identities identities.json`. Finalizing publishes an artifact to the catalog; it does not apply it.

See the [bundle format and compiler contracts](../spec/migration-bundles.md) for the wire format.

## TypeScript hosts

Initialize server WASM before calling the migration APIs. For a filesystem catalog, pass the exact contents of the sealed bundles, including the raw manifest text. Pyre verifies the hashes and orders the chain by predecessor. For a target-local catalog, ordinary apply reads from the database.

```ts
import { applyFinalizedMigrations, initPreservedDatabase } from "@pyre/server";

// bundles: [{ manifest: manifestText, files: { "migration.sql": sql, "schema.pyre": schema } }]
await initPreservedDatabase(db, "Billing", bundles);
await applyFinalizedMigrations(db, "Billing", { bundles });

// On a database already bound to its target-local catalog:
await applyFinalizedMigrations(otherDb, "Billing", { catalog: "database" });
```

Use `baselineDatabase(db, namespace, hash, source)` to adopt an exact finalized schema, `auditDatabaseSchema(db)` to compare live structure with the installed schema, and `setDatabasePreserved(db, namespace, source)` for explicit promotion. `ensureDatabase` checks the finalized runtime fingerprint or updates an explicitly disposable, unbound schema. Use `initDisposableDatabase` for a new working-schema development database.

If a `FinalizedMigrationError` reports `outcome-unknown`, keep the target out of service and use `inspectMigrationOutcome(freshClient, namespace, attemptedBundle, catalog)` before retrying. Pass the exact attempted artifact and its catalog binding. Receipts expose application and grace deadlines as Unix seconds (`bigint`).
