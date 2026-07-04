import { Client } from "@libsql/client";
import * as wasm from "./wasm/pyre_wasm.js";
import { normalizeForWasmJson } from "./wasm-json";
import { requireDatabaseId, type DatabaseId } from "./database-id";
import { activateSchemaForDatabase } from "./schema";

export type SessionValue = null | number | string | Uint8Array;
export const DEFAULT_SYNC_PAGE_SIZE = 1000;
export const MAX_SYNC_PAGE_SIZE = 5000;
export const MAX_SYNC_CURSOR_TABLES = 512;
export const MAX_SYNC_CURSOR_PERMISSION_HASH_BYTES = 256;
const SYNC_ROWS_JSON_COLUMN = "_pyre_rows";

function normalizePageSize(pageSize: number): number {
    if (!Number.isFinite(pageSize) || pageSize <= 0) {
        throw new Error("pageSize must be greater than zero");
    }

    return Math.min(Math.floor(pageSize), MAX_SYNC_PAGE_SIZE);
}

function normalizeParams(params: unknown[] | undefined): unknown[] {
    return (params ?? []).map((value) => {
        if (Array.isArray(value)) {
            return Uint8Array.from(value as number[]);
        }
        return value;
    });
}

function validateSyncCursor(syncCursor: SyncCursor): void {
    if (!syncCursor || typeof syncCursor !== "object" || !syncCursor.tables || typeof syncCursor.tables !== "object") {
        throw new Error("syncCursor must be an object with tables");
    }

    const entries = Object.entries(syncCursor.tables);
    if (entries.length > MAX_SYNC_CURSOR_TABLES) {
        throw new Error(`syncCursor references ${entries.length} tables; max is ${MAX_SYNC_CURSOR_TABLES}`);
    }

    for (const [tableName, entry] of entries) {
        if (!entry || typeof entry !== "object") {
            throw new Error(`syncCursor entry for ${tableName} must be an object`);
        }

        if (typeof entry.permission_hash !== "string") {
            throw new Error(`syncCursor permission_hash for ${tableName} must be a string`);
        }

        if (new TextEncoder().encode(entry.permission_hash).byteLength > MAX_SYNC_CURSOR_PERMISSION_HASH_BYTES) {
            throw new Error(`syncCursor permission_hash for ${tableName} is too large`);
        }
    }
}

/**
 * Sync cursor tracks the last seen state for each table.
 */
export interface SyncCursor {
    tables: Record<string, {
        last_seen_updated_at: number | null;
        permission_hash: string;
    }>;
}

/**
 * Result of a sync page request.
 */
export interface SyncPageResult {
    databaseId?: DatabaseId;
    serverRevision?: number;
    tables: Record<
        string,
        {
            rows: any[];
            permission_hash: string;
            last_seen_updated_at: number | null;
        }
    >;
    has_more: boolean;
}

function tryParseNestedJsonContainer(value: unknown): unknown {
    if (typeof value !== "string") {
        return value;
    }

    const trimmed = value.trim();
    if (!(trimmed.startsWith("{") || trimmed.startsWith("["))) {
        return value;
    }

    try {
        return JSON.parse(trimmed);
    } catch {
        return value;
    }
}

function parseJsonColumnValue(value: unknown): unknown {
    const rawValue = value instanceof Uint8Array
        ? new TextDecoder().decode(value)
        : value;

    if (typeof rawValue !== "string") {
        return rawValue;
    }

    const parsed = JSON.parse(rawValue);
    return tryParseNestedJsonContainer(parsed);
}

function reshapeSyncTableGroups(tableGroups: Array<{ table_name: string; headers: string[]; rows: unknown[][] }>) {
    const reshaped = wasm.reshape_sync_table_groups(normalizeForWasmJson(tableGroups));

    if (typeof reshaped === "string" && reshaped.startsWith("Error:")) {
        throw new Error(reshaped);
    }

    return (typeof reshaped === "string" ? JSON.parse(reshaped) : reshaped) as Array<{
        table_name: string;
        headers: string[];
        rows: unknown[][];
    }>;
}

function coerceUnixSeconds(value: unknown): number {
    if (typeof value === "number") {
        return value;
    }

    if (typeof value === "bigint") {
        return Number(value);
    }

    return new Date(value as string | Date).getTime() / 1000;
}

function rowsFromSyncQueryResult(queryResult: any, headers: string[]): Array<Record<string, any>> {
    if (queryResult.columns?.[0] !== SYNC_ROWS_JSON_COLUMN) {
        return queryResult.rows || [];
    }

    const rawRows = queryResult.rows?.[0]?.[SYNC_ROWS_JSON_COLUMN];
    const rowArrays = typeof rawRows === "string"
        ? JSON.parse(rawRows)
        : Array.isArray(rawRows) ? rawRows : [];

    return rowArrays.map((row: unknown[]) => {
        const rowObject: Record<string, any> = {};
        for (let index = 0; index < headers.length; index += 1) {
            rowObject[headers[index]] = row[index] ?? null;
        }
        return rowObject;
    });
}

/**
 * Session data for sync operations.
 */
export interface SyncSession {
    [key: string]: SessionValue;
}

/**
 * Handle a sync request, returning data that needs to be synced.
 * 
 * @param db - The database client
 * @param syncCursor - Current sync cursor state
 * @param session - Session data for permission evaluation
 * @param pageSize - Number of rows to fetch per table (default: 1000)
 * @returns Sync page result with data to sync
 * @example
 * ```typescript
 * import { catchup } from "pyre-wasm/server";
 * const result = await catchup(db, syncCursor, session, 1000);
 * ```
 */
export async function catchup(
    db: Client,
    syncCursor: SyncCursor,
    session: SyncSession,
    pageSize: number = DEFAULT_SYNC_PAGE_SIZE,
    databaseId?: DatabaseId,
): Promise<SyncPageResult> {
    activateSchemaForDatabase(databaseId);
    const effectivePageSize = normalizePageSize(pageSize);
    validateSyncCursor(syncCursor);

    // Step 1: Get sync status SQL
    const statusStatement = wasm.get_sync_status_sql(syncCursor, session);
    if (typeof statusStatement === "string" && statusStatement.startsWith("Error:")) {
        throw new Error(statusStatement);
    }
    const statusSql = typeof statusStatement === "string" ? statusStatement : statusStatement.sql;
    const statusParams = typeof statusStatement === "string" ? [] : normalizeParams(statusStatement.params);

    // Step 2: Execute sync status SQL
    const statusResult = await db.execute(statusParams.length > 0 ? { sql: statusSql, args: statusParams } : statusSql);
    const rawServerRevision = statusResult.rows[0]?.server_revision;
    const serverRevision = typeof rawServerRevision === "number" || typeof rawServerRevision === "bigint"
        ? Number(rawServerRevision)
        : null;

    // Step 3: Get sync SQL for tables that need syncing
    const syncSqlResult = wasm.get_sync_sql(statusResult.rows, syncCursor, session, effectivePageSize);
    if (typeof syncSqlResult === "string" && syncSqlResult.startsWith("Error:")) {
        throw new Error(syncSqlResult);
    }

    const sqlResult =
        typeof syncSqlResult === "string"
            ? JSON.parse(syncSqlResult)
            : syncSqlResult;

    const result: SyncPageResult = {
        ...(databaseId ? { databaseId: requireDatabaseId(databaseId) } : {}),
        tables: {},
        has_more: false,
    };

    if (!Array.isArray(sqlResult.tables) || sqlResult.tables.length === 0) {
        if (serverRevision !== null) {
            result.serverRevision = serverRevision;
        }
        return result;
    }

    // Collect all SQL statements for batch execution
    const allSqlStatements: any[] = [];
    for (const tableSql of sqlResult.tables) {
        for (let index = 0; index < tableSql.sql.length; index += 1) {
            const params = normalizeParams(tableSql.params?.[index]);
            allSqlStatements.push(params.length > 0
                ? { sql: tableSql.sql[index], args: params }
                : tableSql.sql[index]
            );
        }
    }

    // Execute all SQL statements in a single batch
    const allQueryResults = await db.batch(allSqlStatements);

    // Process results for each table
    let resultIndex = 0;
    for (const tableSql of sqlResult.tables) {
        const updatedAtIndex = tableSql.headers.indexOf("updatedAt");
        const jsonColumns = new Set<string>(tableSql.json_columns ?? []);
        const tableRows: any[] = [];
        let maxUpdatedAt: number | null = null;

        for (let sqlIndex = 0; sqlIndex < tableSql.sql.length; sqlIndex += 1) {
            const queryResult = allQueryResults[resultIndex++];
            const columns = queryResult.columns?.[0] === SYNC_ROWS_JSON_COLUMN
                ? tableSql.headers
                : queryResult.columns;
            const rows = rowsFromSyncQueryResult(queryResult, tableSql.headers);

            for (const row of rows) {
                const rowObject: Record<string, any> = {};
                for (const column of columns) {
                    const value = row[column];
                    rowObject[column] = jsonColumns.has(column)
                        ? parseJsonColumnValue(value)
                        : value;
                }
                tableRows.push(rowObject);

                if (updatedAtIndex >= 0 && rowObject.updatedAt !== null && rowObject.updatedAt !== undefined) {
                    const updatedAt = coerceUnixSeconds(rowObject.updatedAt);
                    if (maxUpdatedAt === null || updatedAt > maxUpdatedAt) {
                        maxUpdatedAt = updatedAt;
                    }
                }
            }
        }

        const hasMoreForTable = tableRows.length > effectivePageSize;
        const finalRows = hasMoreForTable ? tableRows.slice(0, effectivePageSize) : tableRows;

        const reshapedGroup = reshapeSyncTableGroups([
            {
                table_name: tableSql.table_name,
                headers: tableSql.headers,
                rows: finalRows.map((row) => tableSql.headers.map((header: string) => row[header] ?? null)),
            },
        ])[0];

        const reshapedRows = (reshapedGroup?.rows || []).map((row) => {
            const rowObject: Record<string, unknown> = {};

            for (let index = 0; index < reshapedGroup.headers.length; index += 1) {
                rowObject[reshapedGroup.headers[index]] = row[index] ?? null;
            }

            return rowObject;
        });

        if (hasMoreForTable && updatedAtIndex >= 0 && finalRows.length > 0) {
            const lastRow = finalRows[finalRows.length - 1];
            const lastUpdatedAt = lastRow.updatedAt;
            if (lastUpdatedAt !== null && lastUpdatedAt !== undefined) {
                const updatedAt = coerceUnixSeconds(lastUpdatedAt);
                maxUpdatedAt = updatedAt;
            }
        }

        result.tables[tableSql.table_name] = {
            rows: reshapedRows,
            permission_hash: tableSql.permission_hash,
            last_seen_updated_at: maxUpdatedAt,
        };

        if (hasMoreForTable) {
            result.has_more = true;
        }
    }

    if (serverRevision !== null) {
        result.serverRevision = serverRevision;
    }

    return result;
}
