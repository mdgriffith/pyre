import type { Client, InStatement, Row, Transaction } from "@libsql/client";
import * as wasm from "./wasm/pyre_wasm.js";
import { requireDatabaseId, type DatabaseId } from "./database-id";
import { seed, type SeedInput } from "./query";

const DEFAULT_SCHEMA_KEY = "__default__";
const INTERNAL_TABLES = new Set(["_pyre_migrations", "_pyre_sync", "_pyre_external_identities", "_pyre_database_policy", "_pyre_finalized_schema", "_pyre_migration_receipts", "_pyre_migration_bundles", "_pyre_migration_drafts"]);
const CREATE_EXTERNAL_IDENTITIES = `create table if not exists _pyre_external_identities (
    namespace text not null,
    record_name text not null,
    field_name text not null,
    identity_kind text not null check (identity_kind in ('Id.Int', 'Id.Uuid')),
    primary key (namespace, record_name, field_name)
)`;

export type ExternalIdentity = {
    namespace: string;
    record: string;
    field: string;
    kind: "Id.Int" | "Id.Uuid";
};
const introspectionsByDatabaseId = new Map<string, unknown>();
const INTERNAL_SETUP_SQL: InStatement[] = [
    `create table if not exists _pyre_migrations (
        id integer not null primary key autoincrement,
        created_at integer not null default (unixepoch()),
        name text not null,
        finished_at integer,
        error text,
        sql text not null,
        schema text,
        schema_namespace text
    )`,
    `create table if not exists _pyre_sync (
        id integer not null primary key check (id = 1),
        database_epoch text not null,
        server_revision integer not null default 0 check (server_revision >= 0)
    )`,
    "insert into _pyre_sync (id, database_epoch, server_revision) values (1, lower(hex(randomblob(16))), 0) on conflict(id) do nothing",
];

export type EnsureDatabaseOutcome = "created" | "migrated" | "up-to-date";

export type DatabasePolicy = "preserved" | "disposable";

type PolicySql = {
    create: string; find: string; read: string; write: string;
    hasNamespace: string; readNamespace: string;
};

async function readPolicy(tx: Transaction, sql: PolicySql, namespace?: string): Promise<{
    policy: DatabasePolicy;
    marked: boolean;
}> {
    if (namespace !== undefined && (await tx.execute(sql.hasNamespace)).rows.length !== 0) {
        const actual = (await tx.execute(sql.readNamespace)).rows[0]?.schema_namespace;
        if (actual != null && actual !== namespace) {
            throw new Error(`Expected database namespace '${namespace}', found '${actual}'`);
        }
    }
    const { rows } = await tx.execute(sql.find);
    if (rows.length === 0) return { policy: "preserved", marked: false };
    if (rows.length === 1 && rows[0].sql === sql.create) {
        const policy = (await tx.execute(sql.read)).rows[0]?.policy;
        if (policy === "preserved" || policy === "disposable") return { policy, marked: true };
    }
    throw new Error("Malformed database policy metadata");
}

/** Read the policy without creating metadata. Initialize server WASM before use. */
export async function inspectDatabasePolicy(db: Client, namespace?: string): Promise<DatabasePolicy> {
    await requireFileBackedLocalDatabase(db, "Database policy operations");
    const sql = wasm.database_policy_sql() as PolicySql;
    const tx = await db.transaction("read");
    try {
        return (await readPolicy(tx, sql, namespace)).policy;
    } finally {
        try { await tx.rollback(); } finally { tx.close(); }
    }
}

/** Persist explicit opt-in on the existing database. Initialize server WASM before use. */
export async function setDatabaseDisposable(db: Client, namespace?: string): Promise<DatabasePolicy> {
    await requireFileBackedLocalDatabase(db, "Database policy operations");
    const sql = wasm.database_policy_sql() as PolicySql;
    return databaseWrite<DatabasePolicy>(db, "Database policy", async tx => {
        const { marked } = await readPolicy(tx, sql, namespace);
        if (!marked) await tx.execute(sql.create);
        await tx.execute({ sql: sql.write, args: ["disposable"] });
        return "disposable";
    });
}

export class DatabaseOutcomeUnknown extends Error {}

export type MigrationCatalog = "filesystem" | "database";

/** Exact UTF-8 contents of a finalized bundle. The manifest is not part of `files`. */
export type FinalizedMigrationBundle = {
    manifest: string;
    files: Readonly<Record<string, string>>;
};

export type MigrationSource =
    | { catalog?: "filesystem"; bundles: readonly FinalizedMigrationBundle[] }
    | { catalog: "database" };

export type MigrationReceipt = { appliedAt: bigint; updateBy: bigint | null };
export type MigrationApplication =
    | { hash: string; outcome: "applied" | "already-applied"; receipt: MigrationReceipt }
    | { hash: string; outcome: "covered-by-baseline" };
export type MigrationInspection = { outcome: "applied"; receipt: MigrationReceipt } | { outcome: "not-applied" };

export class FinalizedMigrationError extends Error {
    constructor(
        readonly outcome: "not-applied" | "rolled-back" | "outcome-unknown",
        cause: unknown,
        readonly hash?: string,
    ) {
        const detail = cause instanceof Error ? cause.message : String(cause);
        const recovery = outcome === "outcome-unknown" ? "; reconnect and inspect before retrying" : "";
        super(`${outcome.replaceAll("-", " ")}${hash ? ` (${hash})` : ""}: ${detail}${recovery}`, { cause });
    }
}

type StoredBundle = { manifest: string; files: string };
type BundleKey = { hash: string; predecessor: string | null; apiVersion: string };
type FinalizedStep = BundleKey & {
    schema: string; identities: ExternalIdentity[]; fingerprint: string;
    graceSeconds: string | null; statements: string[]; assertions: [string, string][]; install: InStatement[];
};
type FinalizedPlan = { steps: FinalizedStep[]; store: InStatement[]; baseline: InStatement[] | null };
type FinalizedSql = Record<
    "createState" | "createReceipts" | "createBundles" | "createPolicy" | "readState" | "readBundles" |
    "readReceipt" | "insertReceipt" | "tableExists" | "nonempty" | "unsafeConnection" | "unsafeWrites" |
    "createMetadata" | "hasMetadataNamespace" | "addMetadataNamespace" | "readStructure", string
> & { preserve: InStatement };

const finalizedSql = () => wasm.finalized_migration_sql() as FinalizedSql;
const storedBundle = (bundle: FinalizedMigrationBundle): StoredBundle => ({ manifest: bundle.manifest, files: JSON.stringify(bundle.files) });

function historyPlan(namespace: string, catalog: MigrationCatalog, bundles: readonly StoredBundle[], init = false, at?: string): FinalizedPlan {
    return wasm.plan_finalized_history(namespace, catalog, bundles, init, at) as FinalizedPlan;
}

async function hasRows(tx: Transaction, sql: string, args: string[] = []): Promise<boolean> {
    return (await tx.execute(args.length === 0 ? sql : { sql, args })).rows.length > 0;
}

async function localArtifacts(tx: Transaction, sql: FinalizedSql, namespace: string): Promise<StoredBundle[]> {
    if (!await hasRows(tx, sql.tableExists, ["_pyre_migration_bundles"])) return [];
    return (await tx.execute({ sql: sql.readBundles, args: [namespace] })).rows.map(row => ({
        manifest: requireText(row.manifest), files: requireText(row.files),
    }));
}

function requireText(value: unknown): string {
    if (typeof value !== "string") throw new Error("Malformed finalized migration metadata");
    return value;
}

async function readReceipt(tx: Transaction, sql: FinalizedSql, bundle: BundleKey): Promise<MigrationReceipt | undefined> {
    const row = (await tx.execute({ sql: sql.readReceipt, args: [bundle.hash] })).rows[0];
    if (!row) return undefined;
    if (row.predecessor !== bundle.predecessor || row.api_version !== bundle.apiVersion) {
        throw new Error("Recorded receipt conflicts with the migration artifact");
    }
    return { appliedAt: BigInt(requireText(row.applied_at)), updateBy: row.update_by === null ? null : BigInt(requireText(row.update_by)) };
}

function position(plan: FinalizedPlan, hash: unknown): number {
    const index = plan.steps.findIndex(step => step.hash === hash);
    if (index < 0) throw new Error(`Target history diverges: ${hash} is absent from the bound catalog`);
    return index;
}

function installedPosition(plan: FinalizedPlan, state: Row, namespace: string, catalog: MigrationCatalog): number {
    if (state.namespace !== namespace || state.catalog !== catalog) throw new Error("Target namespace or catalog does not match its stored binding");
    const index = position(plan, state.head);
    const { apiVersion, fingerprint } = plan.steps[index];
    if (state.api_version !== apiVersion || state.fingerprint !== fingerprint) {
        throw new Error("Installed schema metadata conflicts with its catalog artifact");
    }
    return index;
}

async function validateMigrationConnection(tx: Transaction, sql: FinalizedSql): Promise<void> {
    if (await hasRows(tx, sql.unsafeConnection)) throw new Error("Migration requires a dedicated connection without temp objects or attached databases");
}

async function migrationWrite<T>(
    db: Client,
    work: (tx: Transaction, started: () => void) => Promise<{ value: T; commit: boolean }>,
    hash?: string,
    inspecting = false,
): Promise<T> {
    let tx: Transaction;
    try { tx = await db.transaction("write"); }
    catch (cause) { throw new FinalizedMigrationError(inspecting ? "outcome-unknown" : "not-applied", cause, hash); }
    let started = false;
    let finishing = false;
    try {
        const { value, commit } = await work(tx, () => { started = true; });
        finishing = true;
        if (commit) await tx.commit(); else await tx.rollback();
        return value;
    } catch (cause) {
        if (finishing) throw new FinalizedMigrationError("outcome-unknown", cause, hash);
        try {
            if (tx.closed) throw new Error("Transaction closed without confirming rollback");
            await tx.rollback();
        } catch (rollback) {
            throw new FinalizedMigrationError("outcome-unknown", new AggregateError([cause, rollback], "Rollback was not confirmed"), hash);
        }
        throw new FinalizedMigrationError(inspecting ? "outcome-unknown" : started ? "rolled-back" : "not-applied", cause, hash);
    } finally {
        try { tx.close(); } catch { /* Commit or rollback determines the outcome. */ }
    }
}

async function installFinalizedRuntime(tx: Transaction, sql: FinalizedSql, statements: InStatement[]): Promise<void> {
    await tx.execute(sql.createMetadata);
    if (!await hasRows(tx, sql.hasMetadataNamespace)) await tx.execute(sql.addMetadataNamespace);
    for (const statement of statements) await tx.execute(statement);
}

async function runFinalizedHistory(db: Client, namespace: string, source: MigrationSource | {
    catalog: MigrationCatalog; bundles: readonly FinalizedMigrationBundle[];
}, initialize: boolean): Promise<MigrationApplication[]> {
    const catalog = source.catalog ?? "filesystem";
    let sql: FinalizedSql;
    let plan: FinalizedPlan;
    try {
        await requireFileBackedLocalDatabase(db, "Finalized migrations");
        sql = finalizedSql();
        if ("bundles" in source && (initialize || catalog === "filesystem")) {
            plan = historyPlan(namespace, catalog, source.bundles.map(storedBundle), initialize);
        } else {
            // This snapshot chooses the pending chain. Each write revalidates
            // membership against the target-local catalog under its lock.
            const tx = await db.transaction("read");
            try {
                const state = await readFinalizedState(tx);
                if (state?.catalog !== catalog || state.namespace !== namespace) throw new Error("Target has no matching database catalog binding");
                plan = historyPlan(namespace, catalog, await localArtifacts(tx, sql, namespace));
            } finally {
                try { await tx.rollback(); } finally { tx.close(); }
            }
        }
        if (plan.steps.length === 0) throw new Error("No finalized migrations in this namespace");
    } catch (cause) {
        throw new FinalizedMigrationError("not-applied", cause);
    }

    const outcomes: MigrationApplication[] = [];
    for (const [index, step] of plan.steps.entries()) {
        const first = initialize && index === 0;
        const { hash } = step;
        outcomes.push(await migrationWrite<MigrationApplication>(db, async (tx, started) => {
            await validateMigrationConnection(tx, sql);
            const state = await readFinalizedState(tx);
            if (first) {
                if (state || await hasRows(tx, sql.nonempty)) throw new Error("Initialization requires an empty database");
            } else {
                if (!state) throw new Error("Target has no finalized provenance; initialize or baseline it first");
                const head = installedPosition(plan, state, namespace, catalog);
                if (catalog === "database") {
                    const local = historyPlan(namespace, catalog, await localArtifacts(tx, sql, namespace));
                    position(local, state.head);
                    position(local, hash);
                }
                const baseline = state.baseline === null ? -1 : position(plan, state.baseline);
                if (index <= baseline && baseline <= head) return { value: { hash, outcome: "covered-by-baseline" }, commit: false };
                const receipt = await readReceipt(tx, sql, step);
                if (receipt) {
                    if (index > head) throw new Error("Migration receipt is ahead of installed history");
                    return { value: { hash, outcome: "already-applied", receipt }, commit: false };
                }
                if (step.predecessor !== state.head) throw new Error(`Unexpected predecessor; target head is ${state.head}`);
            }
            if (step.statements.length && await hasRows(tx, sql.unsafeWrites)) {
                throw new Error("V1 migration writes do not support targets with triggers or virtual tables");
            }

            started();
            if (first) {
                for (const statement of [sql.createState, sql.createReceipts, sql.createPolicy, sql.preserve]) await tx.execute(statement);
                if (catalog === "database") {
                    await tx.execute(sql.createBundles);
                    for (const statement of plan.store) await tx.execute(statement);
                }
            }
            for (const [i, statement] of step.statements.entries()) {
                try { await tx.execute(statement); }
                catch (cause) { throw new Error(`migration.sql statement ${i + 1}: ${cause}`, { cause }); }
            }
            for (const [name, statement] of step.assertions) {
                let found: boolean;
                try { found = await hasRows(tx, statement); }
                catch (cause) { throw new Error(`assertion ${name}: ${cause}`, { cause }); }
                if (found) throw new Error(`assertion ${name} returned a row`);
            }
            await installFinalizedRuntime(tx, sql, step.install);
            const appliedAt = BigInt(requireText((await tx.execute("select cast(unixepoch() as text) as ts")).rows[0]?.ts));
            const updateBy = step.graceSeconds === null ? null : appliedAt + BigInt(step.graceSeconds);
            if (updateBy !== null && updateBy > 9223372036854775807n) throw new Error("Grace deadline overflows database timestamp");
            await tx.execute({ sql: sql.insertReceipt, args: [hash, step.predecessor, appliedAt, step.apiVersion, updateBy] });
            return { value: { hash, outcome: "applied", receipt: { appliedAt, updateBy } }, commit: true };
        }, hash));
    }
    return outcomes;
}

/** Create a preserved database from finalized history. The root requires an empty target. */
export function initPreservedDatabase(db: Client, namespace: string, bundles: readonly FinalizedMigrationBundle[], catalog: MigrationCatalog = "filesystem"): Promise<MigrationApplication[]> {
    return runFinalizedHistory(db, namespace, { catalog, bundles }, true);
}

/** Apply finalized history from the stored binding. The host owns the maintenance window. */
export function applyFinalizedMigrations(db: Client, namespace: string, source: MigrationSource): Promise<MigrationApplication[]> {
    return runFinalizedHistory(db, namespace, source, false);
}

/** Inspect an ambiguous commit on a fresh, dedicated client. Does not retry. */
export async function inspectMigrationOutcome(db: Client, namespace: string, bundle: FinalizedMigrationBundle, catalog: MigrationCatalog = "filesystem"): Promise<MigrationInspection> {
    let key: BundleKey;
    let sql: FinalizedSql;
    try {
        await requireFileBackedLocalDatabase(db, "Finalized migrations");
        sql = finalizedSql();
        key = wasm.verify_finalized_bundle(namespace, storedBundle(bundle)) as BundleKey;
    } catch (cause) { throw new FinalizedMigrationError("outcome-unknown", cause); }
    return migrationWrite<MigrationInspection>(db, async tx => {
        await validateMigrationConnection(tx, sql);
        const state = await readFinalizedState(tx);
        if (!state) {
            if (key.predecessor === null && !await hasRows(tx, sql.nonempty)) return { value: { outcome: "not-applied" }, commit: false };
            throw new Error("No matching target history");
        }
        if (state.namespace !== namespace || state.catalog !== catalog) throw new Error("Target has a different namespace or catalog binding");
        const receipt = await readReceipt(tx, sql, key);
        if (receipt) return { value: { outcome: "applied", receipt }, commit: false };
        if (key.predecessor === state.head) return { value: { outcome: "not-applied" }, commit: false };
        throw new Error("Target history diverged; do not retry");
    }, key.hash, true);
}

async function auditStructure(tx: Transaction, sql: FinalizedSql, namespace: string, source: string, identities: ExternalIdentity[]): Promise<void> {
    const { rows } = await tx.execute(sql.readStructure);
    wasm.audit_finalized_structure(namespace, source, identities, rows.map(row => [row.type, row.name, row.sql]));
}

/** Adopt exactly the chosen finalized schema without running migration SQL or creating receipts. */
export async function baselineDatabase(db: Client, namespace: string, at: string, source: MigrationSource): Promise<void> {
    await requireFileBackedLocalDatabase(db, "Database baseline");
    const sql = finalizedSql();
    const catalog = source.catalog ?? "filesystem";
    return migrationWrite(db, async tx => {
        await validateMigrationConnection(tx, sql);
        if (await readFinalizedState(tx) || await hasRows(tx, sql.tableExists, ["_pyre_migration_receipts"])) throw new Error("Target already has finalized migration history");
        await readPolicy(tx, wasm.database_policy_sql() as PolicySql, namespace);
        const local = await localArtifacts(tx, sql, namespace);
        if (catalog === "filesystem" && (local.length || await hasRows(tx, sql.tableExists, ["_pyre_migration_drafts"]))) {
            throw new Error("Target already contains database-catalog authoring data");
        }
        const plan = historyPlan(namespace, catalog, "bundles" in source && catalog === "filesystem" ? source.bundles.map(storedBundle) : local, false, at);
        const step = plan.steps[position(plan, at)];
        await auditStructure(tx, sql, namespace, step.schema, step.identities);
        await tx.execute(sql.createState);
        await tx.execute(sql.createReceipts);
        await installFinalizedRuntime(tx, sql, plan.baseline!);
        return { value: undefined, commit: true };
    }, at);
}

/** Promote an existing target only after verifying finalized provenance and structure. */
export async function setDatabasePreserved(db: Client, namespace: string, source: MigrationSource): Promise<DatabasePolicy> {
    await requireFileBackedLocalDatabase(db, "Database promotion");
    const sql = finalizedSql();
    const catalog = source.catalog ?? "filesystem";
    return migrationWrite<DatabasePolicy>(db, async tx => {
        await validateMigrationConnection(tx, sql);
        const { policy, marked } = await readPolicy(tx, wasm.database_policy_sql() as PolicySql, namespace);
        const state = await readFinalizedState(tx);
        if (!state) throw new Error("Promotion requires verified finalized history or an explicit baseline");
        const plan = historyPlan(namespace, catalog, "bundles" in source && catalog === "filesystem" ? source.bundles.map(storedBundle) : await localArtifacts(tx, sql, namespace));
        const head = plan.steps[installedPosition(plan, state, namespace, catalog)];
        if (state.baseline !== head.hash && !await readReceipt(tx, sql, head)) throw new Error("Installed head has neither a matching receipt nor a baseline");
        await auditStructure(tx, sql, namespace, head.schema, head.identities);
        verifyInstalledFingerprint((await introspectDatabase(tx)).introspection, state);
        if (!marked) await tx.execute(sql.createPolicy);
        if (policy !== "preserved" || !marked) await tx.execute(sql.preserve);
        return { value: "preserved", commit: true };
    });
}

/** Compare live structure to the installed schema without modifying either. */
export async function auditDatabaseSchema(db: Client): Promise<void> {
    const sql = finalizedSql();
    const tx = await db.transaction("read");
    try {
        const state = await readFinalizedState(tx);
        if (!state) throw new Error("Target has no finalized installation");
        const { introspection } = await introspectDatabase(tx);
        verifyInstalledFingerprint(introspection, state);
        await auditStructure(tx, sql, introspection.schema_namespace, introspection.schema_source, introspection.external_identities);
    } finally {
        try { await tx.rollback(); } finally { tx.close(); }
    }
}

async function databaseWrite<T>(db: Client, operation: string, work: (tx: Transaction) => Promise<T>): Promise<T> {
    const tx = await db.transaction("write");
    let committing = false;
    try {
        const result = await work(tx);
        committing = true;
        await tx.commit();
        return result;
    } catch (error) {
        if (committing) throw new DatabaseOutcomeUnknown(`${operation} commit outcome unknown`, { cause: error });
        try {
            if (tx.closed) throw new Error("Transaction closed without confirming rollback");
            await tx.rollback();
        } catch (rollback) {
            throw new DatabaseOutcomeUnknown(`${operation} rollback outcome unknown`, { cause: new AggregateError([error, rollback]) });
        }
        throw error;
    } finally {
        try { tx.close(); } catch { /* Commit or rollback determines the outcome. */ }
    }
}

/** Install a working schema on an empty target and mark it disposable. */
export function initDisposableDatabase(db: Client, namespace: string, source: string, identities: ExternalIdentity[] = []): Promise<void> {
    return runDisposableDatabase(db, "init", namespace, source, identities);
}

/** Replace the schema and lineage in place. Use a dedicated maintenance client. */
export function resetDatabase(db: Client, namespace: string, source: string, identities: ExternalIdentity[] = []): Promise<void> {
    return runDisposableDatabase(db, "reset", namespace, source, identities);
}

export class ReseedError extends Error {
    constructor(
        readonly kind: "reset-failed" | "reset-outcome-unknown" | "fresh-without-fixtures" | "seed-outcome-unknown",
        detail: string,
        cause: unknown,
    ) {
        const summary = {
            "reset-failed": "Reseed stopped during reset",
            "reset-outcome-unknown": "Reseed reset outcome unknown. Inspect the target before retrying",
            "fresh-without-fixtures": "Reset committed: the database has a fresh initialized schema and no fixture rows. Fix the failure and rerun reseed from the beginning",
            "seed-outcome-unknown": "Reset committed, but reseed fixture outcome is unknown. Inspect the target before retrying",
        }[kind];
        super(`${summary}: ${detail}`, { cause });
    }
}

/**
 * Commit a reset, then seed separately. Use a dedicated maintenance client.
 * Pass a loader to defer reading and parsing input until after reset.
 */
export async function reseedDatabase(
    db: Client,
    namespace: string,
    source: string,
    fixtures: SeedInput | (() => SeedInput | Promise<SeedInput>),
    identities: ExternalIdentity[] = [],
): Promise<Record<string, unknown[]>> {
    const describe = (error: unknown) => error instanceof Error ? error.message : String(error);
    try {
        await runDisposableDatabase(db, "reseed", namespace, source, identities);
    } catch (error) {
        throw new ReseedError(error instanceof DatabaseOutcomeUnknown ? "reset-outcome-unknown" : "reset-failed", describe(error), error);
    }

    let input: SeedInput;
    try {
        input = typeof fixtures === "function" ? await fixtures() : fixtures;
    } catch (error) {
        throw new ReseedError("fresh-without-fixtures", describe(error), error);
    }
    const result = await seed(db, input, { namespace });
    if (result.kind === "success") return result.response;
    const error = result.error;
    throw new ReseedError(error.errorType === "OutcomeUnknown" ? "seed-outcome-unknown" : "fresh-without-fixtures", error.message, error);
}

async function requireFileBackedLocalDatabase(db: Client, operation: string): Promise<void> {
    // The local adapter detaches the connection when starting an interactive transaction.
    if (db.protocol === "file") {
        const databases = await db.execute("pragma database_list");
        if (!databases.rows.some(row => row.name === "main" && typeof row.file === "string" && row.file.length > 0)) {
            throw new Error(`${operation} require a file-backed local database`);
        }
    }
}

/** @internal libSQL does not expose the configured integer mode on Client. */
export async function requireSeedIntegerMode(tx: Transaction): Promise<void> {
    const { rows } = await tx.execute("select 1 as integer_mode_probe");
    if (typeof rows[0]?.integer_mode_probe === "string") {
        throw new Error('Seed does not support libSQL intMode "string"; use "number" or "bigint"');
    }
}

function planDisposable(catalog: string, reset: boolean, namespace: string, source: string, identities: ExternalIdentity[]): InStatement[] {
    try {
        return wasm.plan_disposable_database(catalog, reset, namespace, source, identities) as InStatement[];
    } catch (error) {
        throw new Error(String(error));
    }
}

async function readCatalog(tx: Transaction): Promise<string> {
    const catalog = (await tx.execute(wasm.database_catalog_sql())).rows[0]?.catalog;
    if (typeof catalog !== "string") throw new Error("Could not read database object catalog");
    return catalog;
}

async function resetDatabaseInBatch(db: Client, namespace: string, source: string, identities: ExternalIdentity[], reseed: boolean): Promise<void> {
    // migrate() switches FK enforcement off before BEGIN and restores it after
    // the batch. This is synchronous on SQLite and stream-scoped on Hrana.
    // Plan from a read snapshot and verify its preconditions in the batch.
    if (db.protocol === "file" && Number((await db.execute("pragma foreign_keys")).rows[0]?.foreign_keys) !== 1) {
        throw new Error("Local reset requires a maintenance client with foreign-key enforcement enabled");
    }
    const sql = wasm.database_policy_sql() as PolicySql;
    const tx = await db.transaction("read");
    let catalog: string;
    let hasNamespace: boolean;
    try {
        if ((await readPolicy(tx, sql, namespace)).policy !== "disposable") {
            throw new Error("Reset requires an existing disposable database");
        }
        if (reseed) await requireSeedIntegerMode(tx);
        hasNamespace = (await tx.execute(sql.hasNamespace)).rows.length !== 0;
        catalog = await readCatalog(tx);
    } finally {
        try { await tx.rollback(); } finally { tx.close(); }
    }

    const statements = planDisposable(JSON.stringify({ ...JSON.parse(catalog), foreign_keys: 0 }), true, namespace, source, identities);
    // SQLite RAISE() is only available in triggers. Invalid JSON aborts the
    // batch's first statement if any precondition is false or NULL.
    const guard = {
        sql: `with current_catalog as (${wasm.database_catalog_sql()}), policy_objects as (${sql.find})
            select case when json_extract(catalog, '$.foreign_keys') = 0
                and json_remove(catalog, '$.foreign_keys') = json_remove(?, '$.foreign_keys')
                and (select count(*) = 1 and min(sql) = ? from policy_objects)
                and (select policy from (${sql.read})) = 'disposable'
                ${hasNamespace ? `and coalesce((${sql.readNamespace}) collate binary = ?, 1)` : ""}
                then 1 else json('Pyre reset preconditions changed') end
            from current_catalog`,
        args: hasNamespace ? [catalog, sql.create, namespace] : [catalog, sql.create],
    };
    try {
        await db.migrate([guard, ...statements]);
    } catch (cause) {
        // migrate() does not expose which step failed or confirm rollback.
        throw new DatabaseOutcomeUnknown("Database reset batch outcome unknown", { cause });
    }
}

async function runDisposableDatabase(db: Client, operation: "init" | "reset" | "reseed", namespace: string, source: string, identities: ExternalIdentity[]): Promise<void> {
    await requireFileBackedLocalDatabase(db, "Database lifecycle operations");
    if (operation !== "init") {
        return resetDatabaseInBatch(db, namespace, source, identities, operation === "reseed");
    }
    return databaseWrite(db, "Database initialization", async tx => {
        const statements = planDisposable(await readCatalog(tx), false, namespace, source, identities);
        for (const statement of statements) await tx.execute(statement);
    });
}

type MigrationPlan = {
    sql: InStatement[];
    mark_success: InStatement;
    external_identities: ExternalIdentity[];
};

function sameIdentities(a: ExternalIdentity[], b: ExternalIdentity[]): boolean {
    return a.length === b.length && a.every(identity => b.some(candidate =>
        candidate.namespace === identity.namespace && candidate.record === identity.record
        && candidate.field === identity.field && candidate.kind === identity.kind));
}

/** Check an installed finalized schema, or update an explicitly disposable database. */
export async function ensureDatabase(
    db: Client,
    schemaName: string,
    schemaSource: string,
    externalIdentities: ExternalIdentity[] = [],
): Promise<EnsureDatabaseOutcome> {
    const tx = await db.transaction("write");
    try {
        const { initialized, introspection } = await introspectDatabase(tx);
        const { policy } = await readPolicy(tx, wasm.database_policy_sql() as PolicySql, schemaName);
        const installed = await readFinalizedState(tx);
        if (policy !== "disposable" || installed) {
            if (!installed) throw new Error("Finalized migration provenance required; use db init or db baseline, or initialize explicitly as disposable");
            const fingerprint = wasm.runtime_schema_fingerprint(schemaName, schemaSource, externalIdentities);
            const actual = wasm.runtime_schema_fingerprint(introspection.schema_namespace, introspection.schema_source, introspection.external_identities);
            if (installed.namespace !== schemaName || actual !== installed.fingerprint || fingerprint !== installed.fingerprint) {
                throw new Error("Apply finalized migrations matching the generated schema before serving");
            }
            await tx.rollback();
            return "up-to-date";
        }
        if (!initialized && introspection.tables.length > 0) {
            throw new Error(
                "Refusing to initialize a non-empty database that is not managed by Pyre",
            );
        }

        const result = wasm.migrate_with_introspection(
            `ensure:${schemaName}`,
            schemaSource,
            { ...introspection, target_schema_namespace: schemaName, target_external_identities: externalIdentities },
        ) as { Ok: MigrationPlan } | { Err: unknown[] };
        if ("Err" in result) {
            throw new Error(`Schema migration failed: ${JSON.stringify(result.Err)}`);
        }

        if (
            initialized
            && result.Ok.sql.length === 0
            && introspection.schema_source === schemaSource
            && introspection.schema_namespace === schemaName
            && sameIdentities(introspection.external_identities, result.Ok.external_identities)
        ) {
            await tx.rollback();
            return "up-to-date";
        }

        const setup = !initialized && result.Ok.sql.length === 0
            ? INTERNAL_SETUP_SQL
            : [];
        await tx.batch([...setup, ...result.Ok.sql]);
        await ensureMetadataStorage(tx);
        await tx.execute(result.Ok.mark_success);
        await upsertExternalIdentities(tx, result.Ok.external_identities);
        await tx.execute({
            sql: "update _pyre_migrations set schema_namespace = ? where id = (select id from _pyre_migrations where finished_at is not null and error is null and schema is not null order by id desc limit 1)",
            args: [schemaName],
        });
        await tx.commit();
        return initialized ? "migrated" : "created";
    } catch (error) {
        if (!tx.closed) {
            await tx.rollback();
        }
        throw error;
    } finally {
        tx.close();
    }
}

async function readFinalizedState(tx: Transaction): Promise<Row | undefined> {
    const sql = finalizedSql();
    if (!await hasRows(tx, sql.tableExists, ["_pyre_finalized_schema"])) return undefined;
    const installed = (await tx.execute(sql.readState)).rows[0];
    if (!installed) throw new Error("Missing finalized installation metadata");
    return installed;
}

function verifyInstalledFingerprint(introspection: any, installed: Row): void {
    if (installed.namespace !== introspection.schema_namespace || installed.fingerprint !== wasm.runtime_schema_fingerprint(introspection.schema_namespace, introspection.schema_source, introspection.external_identities)) {
        throw new Error("Installed runtime schema does not match its finalized fingerprint");
    }
}

async function ensureMetadataStorage(tx: Transaction): Promise<void> {
    const columns = await tx.execute("pragma table_info('_pyre_migrations')");
    if (!columns.rows.some(row => row.name === "schema_namespace")) {
        await tx.execute("alter table _pyre_migrations add column schema_namespace text");
    }
    await tx.execute(CREATE_EXTERNAL_IDENTITIES);
}

async function upsertExternalIdentities(tx: Transaction, identities: ExternalIdentity[]): Promise<void> {
    for (const identity of identities) {
        await tx.execute({
            sql: "insert into _pyre_external_identities (namespace, record_name, field_name, identity_kind) values (?, ?, ?, ?) on conflict (namespace, record_name, field_name) do update set identity_kind = excluded.identity_kind where identity_kind != excluded.identity_kind",
            args: [identity.namespace, identity.record, identity.field, identity.kind],
        });
    }
    await tx.execute({
        sql: "delete from _pyre_external_identities where (namespace, record_name, field_name) not in (select json_extract(value, '$.namespace'), json_extract(value, '$.record'), json_extract(value, '$.field') from json_each(?))",
        args: [JSON.stringify(identities)],
    });
}

async function loadMetadata(db: Client | Transaction, introspection: any): Promise<any> {
    const tables = await db.execute("select name from main.sqlite_master where type = 'table' and name = '_pyre_external_identities'");
    introspection.external_identities = [];
    if (tables.rows.length > 0) {
        const identities = await db.execute("select namespace, record_name, field_name, identity_kind from main._pyre_external_identities order by namespace, record_name, field_name");
        introspection.external_identities = identities.rows.map(row => ({
            namespace: row.namespace, record: row.record_name, field: row.field_name, kind: row.identity_kind,
        }));
    }
    const columns = await db.execute("pragma main.table_info('_pyre_migrations')");
    if (columns.rows.some(row => row.name === "schema_namespace")) {
        const namespaces = await db.execute("select schema_namespace from main._pyre_migrations where finished_at is not null and error is null and schema is not null order by id desc limit 1");
        introspection.schema_namespace = namespaces.rows[0]?.schema_namespace ?? null;
    }
    return introspection;
}

export async function introspectDatabase(tx: Client | Transaction): Promise<{
    initialized: boolean;
    introspection: any;
}> {
    const initializedResult = await tx.execute(wasm.sql_is_initialized());
    if (initializedResult.rows.length === 0) {
        throw new Error("Failed to check if database is initialized");
    }
    const initialized = Number(initializedResult.rows[0].is_initialized) === 1;
    const result = await tx.execute(
        initialized ? wasm.sql_introspect() : wasm.sql_introspect_uninitialized(),
    );
    if (result.rows.length === 0) {
        throw new Error("Failed to get database introspection");
    }

    return {
        initialized,
        introspection: await loadMetadata(tx, filterInternalTables(JSON.parse(result.rows[0].result as string))),
    };
}

async function readIntrospectionSnapshot(db: Client): Promise<any> {
    const tx = await db.transaction("read");
    try {
        const { introspection } = await introspectDatabase(tx);
        const installed = await readFinalizedState(tx);
        if (installed) verifyInstalledFingerprint(introspection, installed);
        await tx.commit();
        return introspection;
    } catch (error) {
        if (!tx.closed) await tx.rollback();
        throw error;
    } finally {
        tx.close();
    }
}

function schemaKey(databaseId?: DatabaseId): string {
    return databaseId ? requireDatabaseId(databaseId) : DEFAULT_SCHEMA_KEY;
}

export function activateSchemaForDatabase(databaseId?: DatabaseId): void {
    const introspection = introspectionsByDatabaseId.get(schemaKey(databaseId));
    if (introspection === undefined) {
        if (!databaseId) {
            return;
        }

        throw new Error(
            `No schema loaded for databaseId: ${requireDatabaseId(databaseId)}`,
        );
    }

    wasm.set_schema(introspection);
}

/**
 * Load schema from database into WASM cache.
 * This introspects the database and caches the schema for use by sync operations.
 * 
 * @param db - The database client to introspect
 * @throws Error if schema loading fails
 * @example
 * ```typescript
 * import { loadSchemaFromDatabase } from "pyre-wasm/server";
 * const db = createClient({ url: "file:./test.db" });
 * await loadSchemaFromDatabase(db);
 * ```
 */
export async function loadSchemaFromDatabase(db: Client): Promise<void>;
export async function loadSchemaFromDatabase(databaseId: DatabaseId, db: Client): Promise<void>;
export async function loadSchemaFromDatabase(
    databaseOrDb: DatabaseId | Client,
    maybeDb?: Client,
): Promise<void> {
    const databaseId = maybeDb ? requireDatabaseId(databaseOrDb as DatabaseId) : undefined;
    const db = maybeDb ?? (databaseOrDb as Client);
    const introspection = await readIntrospectionSnapshot(db);
    wasm.set_schema(introspection);
    introspectionsByDatabaseId.set(schemaKey(databaseId), introspection);
}

function filterInternalTables(introspection: any): any {
    if (!introspection || !Array.isArray(introspection.tables)) {
        return introspection;
    }

    return {
        ...introspection,
        tables: introspection.tables.filter((table: any) => !INTERNAL_TABLES.has(table?.name)),
    };
}

/**
 * Get the Pyre schema source from the database.
 * Returns the raw Pyre schema text stored with the latest successful migration.
 * 
 * @param db - The database client
 * @returns The Pyre schema source text, or empty string if not found
 * @example
 * ```typescript
 * import { getPyreSchemaSource } from "pyre-wasm/server";
 * const db = createClient({ url: "file:./test.db" });
 * const schemaSource = await getPyreSchemaSource(db);
 * ```
 */
export async function getPyreSchemaSource(db: Client): Promise<string> {
    try {
        const result = await db.execute(
            "SELECT schema FROM _pyre_migrations WHERE finished_at IS NOT NULL AND error IS NULL AND schema IS NOT NULL ORDER BY id DESC LIMIT 1"
        );

        if (result.rows.length === 0) {
            return "";
        }

        const schema = result.rows[0].schema;
        return typeof schema === "string" ? schema : "";
    } catch (error) {
        // Table might not exist or database might not be initialized
        return "";
    }
}

/**
 * Get the introspection JSON from the database.
 * Returns the full introspection result including tables, foreign keys, and schema source.
 * 
 * @param db - The database client
 * @returns The introspection JSON object
 * @example
 * ```typescript
 * import { getIntrospectionJson } from "pyre-wasm/server";
 * const db = createClient({ url: "file:./test.db" });
 * const introspection = await getIntrospectionJson(db);
 * ```
 */
export async function getIntrospectionJson(db: Client): Promise<any> {
    let introspection = await readIntrospectionSnapshot(db);
    // Process introspection through WASM to populate links
    introspection = wasm.process_introspection(introspection);


    return introspection;
}
