import type { Client, InStatement, Transaction } from "@libsql/client";
import * as wasm from "./wasm/pyre_wasm.js";
import { requireDatabaseId, type DatabaseId } from "./database-id";

const DEFAULT_SCHEMA_KEY = "__default__";
const INTERNAL_TABLES = new Set(["_pyre_migrations", "_pyre_sync"]);
const introspectionsByDatabaseId = new Map<string, unknown>();
const INTERNAL_SETUP_SQL: InStatement[] = [
    `create table if not exists _pyre_migrations (
        id integer not null primary key autoincrement,
        created_at integer not null default (unixepoch()),
        name text not null,
        finished_at integer,
        error text,
        sql text not null,
        schema text
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
};

/** Create or migrate a database to a generated Pyre schema. */
export async function ensureDatabase(
    db: Client,
    schemaName: string,
    schemaSource: string,
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
            introspection,
        ) as { Ok: MigrationPlan } | { Err: unknown[] };
        if ("Err" in result) {
            throw new Error(`Schema migration failed: ${JSON.stringify(result.Err)}`);
        }

        if (
            initialized
            && result.Ok.sql.length === 0
            && introspection.schema_source === schemaSource
        ) {
            await tx.rollback();
            return "up-to-date";
        }

        const setup = !initialized && result.Ok.sql.length === 0
            ? INTERNAL_SETUP_SQL
            : [];
        await tx.batch([...setup, ...result.Ok.sql, result.Ok.mark_success]);
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

async function introspectDatabase(tx: Transaction): Promise<{
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
        introspection: filterInternalTables(JSON.parse(result.rows[0].result as string)),
    };
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
    const isInitializedQuery = wasm.sql_is_initialized();
    const isInitializedResult = await db.execute(isInitializedQuery);

    if (isInitializedResult.rows.length === 0) {
        throw new Error("Failed to check if database is initialized");
    }

    const isInitialized = isInitializedResult.rows[0].is_initialized === 1;

    let introspection;
    if (isInitialized) {
        const introspectionQuery = wasm.sql_introspect();
        const introspectionResult = await db.execute(introspectionQuery);
        if (introspectionResult.rows.length === 0) {
            throw new Error("Failed to get introspection result");
        }
        introspection = filterInternalTables(JSON.parse(introspectionResult.rows[0].result as string));
    } else {
        const uninitializedIntrospect = wasm.sql_introspect_uninitialized();
        const introspectionResult = await db.execute(uninitializedIntrospect);
        if (introspectionResult.rows.length === 0) {
            throw new Error("Failed to get introspection result");
        }
        introspection = filterInternalTables(JSON.parse(introspectionResult.rows[0].result as string));
    }

    introspectionsByDatabaseId.set(schemaKey(databaseId), introspection);
    wasm.set_schema(introspection);
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
    const isInitializedQuery = wasm.sql_is_initialized();
    const isInitializedResult = await db.execute(isInitializedQuery);

    if (isInitializedResult.rows.length === 0) {
        throw new Error("Failed to check if database is initialized");
    }

    const isInitialized = isInitializedResult.rows[0].is_initialized === 1;

    let introspection;
    if (isInitialized) {
        const introspectionQuery = wasm.sql_introspect();
        const introspectionResult = await db.execute(introspectionQuery);
        if (introspectionResult.rows.length === 0) {
            throw new Error("Failed to get introspection result");
        }
        introspection = filterInternalTables(JSON.parse(introspectionResult.rows[0].result as string));
    } else {
        const uninitializedIntrospect = wasm.sql_introspect_uninitialized();
        const introspectionResult = await db.execute(uninitializedIntrospect);
        if (introspectionResult.rows.length === 0) {
            throw new Error("Failed to get introspection result");
        }
        introspection = filterInternalTables(JSON.parse(introspectionResult.rows[0].result as string));
    }

    // Process introspection through WASM to populate links
    introspection = wasm.process_introspection(introspection);


    return introspection;
}
