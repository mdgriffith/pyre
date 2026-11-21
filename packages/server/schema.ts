import type { Client, InStatement, Transaction } from "@libsql/client";
import * as wasm from "./wasm/pyre_wasm.js";
import { requireDatabaseId, type DatabaseId } from "./database-id";
import { seed, type SeedInput } from "./query";

const DEFAULT_SCHEMA_KEY = "__default__";
const INTERNAL_TABLES = new Set(["_pyre_migrations", "_pyre_sync", "_pyre_external_identities", "_pyre_database_policy"]);
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
    return runDisposableDatabase(db, false, namespace, source, identities);
}

/** Replace the schema and lineage of a disposable database in place. */
export function resetDatabase(db: Client, namespace: string, source: string, identities: ExternalIdentity[] = []): Promise<void> {
    return runDisposableDatabase(db, true, namespace, source, identities);
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
 * Commit a reset, then seed separately. Keep application traffic stopped until success.
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
        await resetDatabase(db, namespace, source, identities);
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

async function runDisposableDatabase(db: Client, reset: boolean, namespace: string, source: string, identities: ExternalIdentity[]): Promise<void> {
    await requireFileBackedLocalDatabase(db, "Database lifecycle operations");
    return databaseWrite(db, reset ? "Database reset" : "Database initialization", async tx => {
        if (reset && (await readPolicy(tx, wasm.database_policy_sql() as PolicySql, namespace)).policy !== "disposable") {
            throw new Error("Reset requires an existing disposable database");
        }
        const { rows } = await tx.execute(wasm.database_catalog_sql());
        const catalog = rows[0]?.catalog;
        if (typeof catalog !== "string") throw new Error("Could not read database object catalog");
        let statements: InStatement[];
        try {
            statements = wasm.plan_disposable_database(catalog, reset, namespace, source, identities) as InStatement[];
        } catch (error) {
            throw new Error(String(error));
        }
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

/** Create or migrate a database to a generated Pyre schema. */
export async function ensureDatabase(
    db: Client,
    schemaName: string,
    schemaSource: string,
    externalIdentities: ExternalIdentity[] = [],
): Promise<EnsureDatabaseOutcome> {
    const tx = await db.transaction("write");
    try {
        const { initialized, introspection } = await introspectDatabase(tx);
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
