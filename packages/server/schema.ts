import type { Client, InStatement, Transaction } from "@libsql/client";
import * as wasm from "./wasm/pyre_wasm.js";
import { requireDatabaseId, type DatabaseId } from "./database-id";

const DEFAULT_SCHEMA_KEY = "__default__";
const INTERNAL_TABLES = new Set(["_pyre_migrations", "_pyre_sync", "_pyre_external_identities"]);
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
    const tables = await db.execute("select name from sqlite_master where type = 'table' and name = '_pyre_external_identities'");
    introspection.external_identities = [];
    if (tables.rows.length > 0) {
        const identities = await db.execute("select namespace, record_name, field_name, identity_kind from _pyre_external_identities order by namespace, record_name, field_name");
        introspection.external_identities = identities.rows.map(row => ({
            namespace: row.namespace, record: row.record_name, field: row.field_name, kind: row.identity_kind,
        }));
    }
    const columns = await db.execute("pragma table_info('_pyre_migrations')");
    if (columns.rows.some(row => row.name === "schema_namespace")) {
        const namespaces = await db.execute("select schema_namespace from _pyre_migrations where finished_at is not null and error is null and schema is not null order by id desc limit 1");
        introspection.schema_namespace = namespaces.rows[0]?.schema_namespace ?? null;
    }
    return introspection;
}

async function introspectDatabase(tx: Client | Transaction): Promise<{
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
