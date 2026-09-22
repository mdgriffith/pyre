import { Client, InStatement, type Transaction } from "@libsql/client";
import type { LinkInfo, SchemaMetadata, TableMetadata } from "@pyre/core";
import type { ZodType } from "zod";
import { buildArgs, formatResultData, toSqlStatements, type SqlInfo } from "./runtime/sql";

export type SessionValue =
    | null
    | boolean
    | number
    | string
    | Uint8Array
    | Date
    | SessionValue[]
    | { [key: string]: SessionValue };

type Validator<T> = ZodType<T>;

/**
 * Query metadata containing all information needed to execute a query.
 */
export interface QueryMetadata {
    id: string;
    operation?: "query" | "insert" | "update" | "delete" | string;
    primary_db?: string;
    attached_dbs?: string[];
    sql: SqlInfo[];
    syncSql?: SqlInfo[];
    /** Compiler-owned sync effects for each SQL variant; never inferred from SQL text. */
    syncEffects?: { sql: boolean; syncSql: boolean };
    session_args: string[];
    optional_input_args: string[];
    json_input_args: string[];
    InputValidator: Validator<any>;
    SessionValidator: Validator<any>;
    /** Compiler-owned index of the direct write whose cardinality must be one. */
    generatedEdit?: { writeStatement: number; syncWriteStatement?: number; createId?: string };
}

/**
 * Map of query IDs to their metadata.
 */
export interface QueryMap {
    [queryId: string]: QueryMetadata;
}

/** Transport contains identifiers and values only; SQL and authority stay on the server. */
export interface OperationDescriptor {
    queryId: string;
    input: unknown;
}

/**
 * Session data structure - can be any object with string keys.
 */
export interface Session {
    [key: string]: any;
}

/**
 * Connected session for sync delta calculation.
 */
export interface ConnectedSession {
    session_id: string;
    fields: Record<string, SessionValue>;
}

/**
 * Result of executing a query.
 */
export interface QueryResult {
    kind: "success" | "error";
    /** The JSON response to return to the client (only present on success) */
    response?: unknown;
    /** Error details (only present on error) */
    error?: {
        errorType: string;
        message: string;
        operationIndex?: number;
    };
    /**
     * Broadcast sync deltas to connected clients.
     * Always present, but will be a no-op if there are no affected rows or no connected sessions.
     * 
     * @param sendToSession - Callback to send a message to a specific session
     * @example
     * ```typescript
     * await result.sync((sessionId, message) => {
     *   const client = connectedClients.get(sessionId);
     *   if (client?.ws.readyState === 1) {
     *     client.ws.send(JSON.stringify(message));
     *   }
     * });
     * ```
     */
    sync(sendToSession: (sessionId: string, message: any) => void): Promise<SyncResult>;
}

export interface SyncResult {
    databaseEpoch?: string;
    serverRevision?: number;
    originMessage?: unknown;
}

export type SyncDeltasFn = (
    affectedRowGroups: any[],
    connectedSessions: Map<string, { session: Record<string, SessionValue>; [key: string]: any }>,
    sendToSession: (sessionId: string, message: any) => void,
    originSessionId?: string,
    committedRevision?: { databaseEpoch: string; serverRevision: number }
) => Promise<SyncResult | void>;

export interface RunOptions {
    mode?: "normal" | "sync";
    allocateSyncRevision?: boolean;
}

export type SeedPrimitive = null | boolean | number | string | Uint8Array;
export type SeedJsonObject = { [key: string]: SeedJsonValue };
export type SeedJsonValue = SeedPrimitive | Date | SeedJsonObject | SeedJsonValue[];
export type SeedValue = SeedJsonValue;
export type SeedRow = {
    [field: string]: SeedValue | SeedRow | SeedRow[];
};
export type SeedInput = Record<string, SeedRow[]>;
export type SeedValidators = Record<string, Record<string, Validator<unknown>>>;
export interface SeedOptions {
    batchSize?: number;
    transactionMode?: "manual" | "batch" | "none";
}

export interface SeedResult {
    kind: "success" | "error";
    response?: Record<string, unknown[]>;
    error?: {
        errorType: "InvalidInput" | "DatabaseError";
        message: string;
    };
}

type SeedContext = {
    db: Client;
    schema: SchemaMetadata;
    validators?: SeedValidators;
    batchSize: number;
    transactionMode: "manual" | "batch" | "none";
    statementIndex: number;
    physicalColumns: Map<string, Set<string>>;
};

type SeedPreparedRow = {
    path: string;
    scalarValues: Record<string, SeedValue>;
    nestedValues: Array<{ key: string; link: LinkInfo; value: SeedRow | SeedRow[] }>;
};

function extractAffectedRowGroups(sql: SqlInfo[], resultSets: any[]): any[] {
    const groups: unknown[] = [];
    const includedResultSets = resultSets.filter((_, index) => sql[index]?.include);

    for (const resultSet of includedResultSets) {
        if (!resultSet?.columns?.length) {
            continue;
        }

        if (!resultSet.columns.includes("_affectedRows")) {
            continue;
        }

        for (const row of resultSet.rows || []) {
            if (!("_affectedRows" in row)) {
                continue;
            }

            const raw = row._affectedRows;
            let parsed: unknown;

            if (typeof raw === "string") {
                parsed = JSON.parse(raw);
            } else {
                parsed = raw;
            }

            if (Array.isArray(parsed)) {
                groups.push(...parsed);
            } else if (parsed != null) {
                groups.push(parsed);
            }
        }
    }

    return groups;
}


function decodeOrError<T>(validator: Validator<T>, data: unknown, context: string): { valid: boolean; error?: string; value?: T } {
    const parsed = validator.safeParse(data);
    if (!parsed.success) {
        const errorStr = String(parsed.error);
        return { valid: false, error: `${context}: ${errorStr}` };
    }
    return { valid: true, value: parsed.data };
}

/**
 * Execute a query using the provided query map and database client.
 * 
 * @param db - The database client (already connected)
 * @param queryMap - Map of query IDs to query metadata
 * @param queryId - The query ID to execute
 * @param args - Query arguments
 * @param executingSession - The session executing the query
 * @param connectedSessions - Map of all connected sessions (for sync delta calculation)
 * @returns Query result with response and sync function (always present)
 * @example
 * ```typescript
 * import { run } from "pyre-wasm/server";
 * import { queries } from "./generated/typescript/server";
 * const result = await run(db, queries, "createPost", args, session, connectedClients);
 * await result.sync((sessionId, message) => { ... });
 * ```
 */
export async function run(
    db: Client,
    queryMap: QueryMap,
    queryId: string | readonly OperationDescriptor[],
    args: any,
    executingSession: Session,
    connectedSessions?: Map<string, { session: Record<string, SessionValue>;[key: string]: any }>,
    syncDeltas?: SyncDeltasFn,
    originSessionId?: string,
    options: RunOptions = {},
): Promise<QueryResult> {
    if (Array.isArray(queryId)) {
        return runOperations(db, queryMap, queryId, executingSession, connectedSessions, syncDeltas, originSessionId, options);
    }
    // Look up query metadata
    const id = queryId as string;
    const query = Object.hasOwn(queryMap, id) ? queryMap[id] : undefined;
    if (!query) {
        return {
            kind: "error",
            error: {
                errorType: "UnknownQuery",
                message: `Unknown query ID: ${queryId}`,
            },
            async sync() { return {}; },
        };
    }
    if (query.generatedEdit) {
        return runOperations(db, queryMap, [{ queryId: id, input: args }], executingSession, connectedSessions, syncDeltas, originSessionId, options, true);
    }

    // Validate input
    const inputValidation = decodeOrError(query.InputValidator, args, "Input");
    if (!inputValidation.valid) {
        return {
            kind: "error",
            error: {
                errorType: "InvalidInput",
                message: inputValidation.error || "Invalid input",
            },
            async sync() { return {}; },
        };
    }

    // Validate session
    const sessionValidation = decodeOrError(query.SessionValidator, executingSession, "Session");
    if (!sessionValidation.valid) {
        return {
            kind: "error",
            error: {
                errorType: "InvalidSession",
                message: sessionValidation.error || "Invalid session",
            },
            async sync() { return {}; },
        };
    }

    // Prepare arguments
    const validatedInput = inputValidation.value ?? {};
    const validatedSession = sessionValidation.value ?? {};
    const validArgs = buildArgs(
        validatedInput as Record<string, any>,
        validatedSession as Record<string, any>,
        query.session_args,
        query.optional_input_args,
        query.json_input_args,
    );

    // Prepare SQL statements
    const useSyncMode = options.mode === "sync";
    const activeSql = useSyncMode ? query.syncSql ?? query.sql : query.sql;
    const sqlStatements: InStatement[] = toSqlStatements(activeSql, validArgs);
    const allocateRevision = options.allocateSyncRevision && hasSyncEffect(query, options.mode);

    // Execute query
    // Allocate in the mutation's transaction, never in the later fanout callback.
    // Otherwise delayed publication could give older row values a newer revision.
    if (allocateRevision) {
        sqlStatements.push("update _pyre_sync set server_revision = server_revision + 1 where id = 1 returning database_epoch, server_revision");
    }
    const resultSets = allocateRevision ? await db.batch(sqlStatements, "write") : await db.batch(sqlStatements);
    let committedRevision: { databaseEpoch: string; serverRevision: number } | undefined;
    if (allocateRevision) {
        const stamp = resultSets.pop()?.rows[0];
        if (typeof stamp?.database_epoch !== "string" || (typeof stamp?.server_revision !== "number" && typeof stamp?.server_revision !== "bigint")) {
            throw new Error("Failed to allocate Pyre sync server revision");
        }
        committedRevision = { databaseEpoch: stamp.database_epoch, serverRevision: Number(stamp.server_revision) };
    }
    const affectedRowGroups: unknown[] = extractAffectedRowGroups(activeSql, resultSets);
    const response = formatResultData(activeSql, resultSets);

    return executionResult(response, combineAffectedRows(affectedRowGroups), connectedSessions, syncDeltas, originSessionId, committedRevision);
}

function executionResult(
    response: unknown,
    affectedRowGroups: any[],
    connectedSessions?: Map<string, { session: Record<string, SessionValue>; [key: string]: any }>,
    syncDeltas?: SyncDeltasFn,
    originSessionId?: string,
    committedRevision?: { databaseEpoch: string; serverRevision: number },
): QueryResult {

    // Always create sync function - it will be a no-op if there's nothing to send
    /**
     * Broadcast sync deltas to connected clients.
     * 
     * For each session group, sends filtered table groups.
     * Clients receive only the rows they have permission to see.
     * 
     * Message format sent to each client (grouped by table for efficiency):
     * ```json
     * [
     *   {
     *     "table_name": "users",
     *     "headers": ["id", "name"],
     *     "rows": [[1, "Alice"], [2, "Bob"]]
     *   },
     *   {
     *     "table_name": "posts",
     *     "headers": ["id", "title"],
     *     "rows": [[10, "Hello"], [11, "World"]]
     *   }
     * ]
     * ```
     */
    let syncPromise: Promise<SyncResult> | undefined;
    function sync(sendToSession: (sessionId: string, message: any) => void): Promise<SyncResult> {
        syncPromise ??= publish(sendToSession);
        return syncPromise;
    }

    async function publish(sendToSession: (sessionId: string, message: any) => void): Promise<SyncResult> {
        // Early return if nothing to sync
        if (affectedRowGroups.length === 0) {
            return {};
        }

        if (!syncDeltas) {
            return {};
        }

        const syncResult = await syncDeltas(affectedRowGroups, connectedSessions ?? new Map(), sendToSession, originSessionId, committedRevision) ?? {};
        if (typeof syncResult.serverRevision === "number") {
            queryResult.response = {
                ...(syncResult.databaseEpoch === undefined ? {} : { databaseEpoch: syncResult.databaseEpoch }),
                serverRevision: syncResult.serverRevision,
                ...(syncResult.originMessage === undefined ? {} : { sync: syncResult.originMessage }),
                result: queryResult.response,
            };
        }

        return syncResult;
    }

    const queryResult: QueryResult = {
        kind: "success",
        response,
        sync,
    };

    return queryResult;
}

async function executeCheckedStatements(db: Pick<Client, "execute">, statements: InStatement[], writeStatement: number) {
    if (!Number.isInteger(writeStatement) || writeStatement < 0 || writeStatement >= statements.length) {
        throw new Error("Invalid generated edit metadata");
    }
    const results = [];
    for (const [index, statement] of statements.entries()) {
        results.push(await db.execute(statement));
        if (index === writeStatement) {
            // libsql RETURNING may report rowsAffected=0. changes() is the direct
            // write count, excluding triggers, on this transaction connection.
            const count = await db.execute("select changes() as count");
            if (Number(count.rows[0]?.count) !== 1) throw new Error("Generated edit must affect exactly one row");
        }
    }
    return results;
}

async function writeTransaction<T>(db: Client, execute: (tx: Transaction) => Promise<T>): Promise<T> {
    // The local adapter detaches the client's connection for an interactive
    // transaction. Private in-memory databases would silently lose their state.
    if (db.protocol === "file") {
        const databases = await db.execute("pragma database_list");
        if (!databases.rows.some(row => row.name === "main" && typeof row.file === "string" && row.file.length > 0)) {
            throw new Error("Composed/generated operations require a file-backed local database");
        }
    }
    const tx = await db.transaction("write");
    let committing = false;
    try {
        const result = await execute(tx);
        committing = true;
        await tx.commit();
        return result;
    } catch (error) {
        try { if (!tx.closed) await tx.rollback(); } catch (_) { /* Preserve the execution outcome. */ }
        if (committing) throw new CommitOutcomeUnknown();
        throw error;
    } finally {
        tx.close();
    }
}

class CommitOutcomeUnknown extends Error {}

async function runOperations(
    db: Client,
    queryMap: QueryMap,
    operations: readonly OperationDescriptor[],
    executingSession: Session,
    connectedSessions: Map<string, { session: Record<string, SessionValue>; [key: string]: any }> | undefined,
    syncDeltas: SyncDeltasFn | undefined,
    originSessionId: string | undefined,
    options: RunOptions,
    single = false,
): Promise<QueryResult> {
    const fail = (errorType: string, message: string, operationIndex?: number): QueryResult => ({
        kind: "error", error: { errorType, message, ...(operationIndex === undefined ? {} : { operationIndex }) },
        async sync() { return {}; },
    });
    let namespace: string | undefined;
    for (const [index, operation] of operations.entries()) {
        if (!operation || typeof operation.queryId !== "string" || !Object.hasOwn(operation, "input") || Object.keys(operation).some(key => key !== "queryId" && key !== "input")) {
            return fail("InvalidInput", "Expected an operation descriptor", index);
        }
        const query = Object.hasOwn(queryMap, operation.queryId) ? queryMap[operation.queryId] : undefined;
        if (!query) return fail("UnknownQuery", "Unknown operation", index);
        if (query.generatedEdit?.createId) {
            const input = operation.input as Record<string, unknown> | null;
            const id = input?.[query.generatedEdit.createId];
            if (typeof id !== 'string' || !/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(id)) {
                return fail("InvalidInput", "Generated creates require a canonical UUIDv7", index);
            }
        }
        const currentNamespace = query.primary_db ?? "";
        namespace ??= currentNamespace;
        if (namespace !== currentNamespace || query.attached_dbs?.length) return fail("InvalidInput", "Operations must target one database namespace", index);
        for (const [validator, value, type] of [
            [query.InputValidator, operation.input, "InvalidInput"],
            [query.SessionValidator, executingSession, "InvalidSession"],
        ] as const) {
            if (!validator.safeParse(value).success) return fail(type, "Operation validation failed", index);
        }
    }
    if (operations.length === 0) return executionResult([], [], connectedSessions, syncDeltas, originSessionId);
    const responses: Array<{ index: number; queryId: string; result: unknown }> = [];
    const affected: any[] = [];
    let finalGroups: any[] = [];
    let operationIndex = 0;
    let revision: { databaseEpoch: string; serverRevision: number } | undefined;
    try {
        await writeTransaction(db, async tx => {
            for (const operation of operations) {
                const metadata = queryMap[operation.queryId];
                // Reuse validation, binding, SQL selection and result formatting.
                // This facade executes within the outer transaction; it never commits.
                const executor = {
                    batch: (statements: InStatement[]) => metadata.generatedEdit
                        ? executeCheckedStatements(tx, statements, options.mode === "sync" ? metadata.generatedEdit.syncWriteStatement ?? metadata.generatedEdit.writeStatement : metadata.generatedEdit.writeStatement)
                        : tx.batch(statements),
                } as Client;
                const result = await run(executor, { [operation.queryId]: { ...metadata, generatedEdit: undefined } }, operation.queryId, operation.input, executingSession,
                    undefined, async groups => { affected.push(...groups); }, undefined, { mode: options.mode });
                if (result.kind === "error") throw new Error("Operation validation failed");
                responses.push({ index: operationIndex, queryId: operation.queryId, result: result.response });
                await result.sync(() => {});
                operationIndex += 1;
            }
            finalGroups = combineAffectedRows(affected);
            if (options.allocateSyncRevision && operations.some(op => {
                const query = queryMap[op.queryId];
                return hasSyncEffect(query, options.mode);
            })) {
                const stamp = (await tx.execute("update _pyre_sync set server_revision = server_revision + 1 where id = 1 returning database_epoch, server_revision")).rows[0];
                const serverRevision = Number(stamp?.server_revision);
                if (typeof stamp?.database_epoch !== "string" || !Number.isSafeInteger(serverRevision)) throw new Error("Invalid sync revision");
                revision = { databaseEpoch: stamp.database_epoch, serverRevision };
            }
        });
    } catch (error) {
        if (error instanceof CommitOutcomeUnknown) return fail("OutcomeUnknown", "Commit outcome unknown; do not automatically replay");
        return fail("TransactionFailed", "Operation batch failed", operationIndex);
    }
    return executionResult(single ? responses[0].result : responses, finalGroups, connectedSessions, syncDeltas, originSessionId, revision);
}

function hasSyncEffect(query: QueryMetadata, mode?: "sync" | "normal"): boolean {
    return (mode === "sync" && query.syncSql !== undefined
        ? query.syncEffects?.syncSql
        : query.syncEffects?.sql) ?? false;
}

function combineAffectedRows(affected: any[]): any[] {
    // Only final row versions may be authorized/published: intermediate versions
    // could leak data after a later operation revokes access to the same row.
    const finalGroups = new Map<string, any>();
    const preimages = new Map<string, any>();
    const seen = new Set<string>();
    for (const group of affected) {
        const idIndex = group.headers.indexOf(group.primary_key ?? "id");
        if (idIndex < 0) throw new Error("Affected rows require identity");
        for (const row of group.rows) {
            const key = JSON.stringify([group.table_name, row[idIndex]]);
            const preimage = group.headers.includes("_pyre_preimage");
            const removed = group.headers.includes("_pyre_removed");
            if (!seen.has(key) && (preimage || removed)) {
                const headers = group.headers.filter((header: string) => !header.startsWith("_pyre_"));
                preimages.set(key, { ...group, headers: [...headers, "_pyre_preimage"], rows: [[...headers.map((header: string) => row[group.headers.indexOf(header)]), true]] });
            }
            seen.add(key);
            if (!preimage) finalGroups.set(key, { ...group, rows: [row] });
        }
    }
    const tables = new Map<string, any>();
    for (const group of [...preimages.values(), ...finalGroups.values()]) {
        const key = JSON.stringify([group.table_name, group.headers]);
        const table = tables.get(key);
        if (table) table.rows.push(...group.rows);
        else tables.set(key, group);
    }
    return [...tables.values()];
}

/**
 * Insert seed data using Pyre schema links to connect nested records.
 *
 * This is intended for server-side fixture/import setup. It bypasses Pyre query
 * permissions and does not currently integrate with Pyre sync metadata.
 */
export async function seed(
    db: Client,
    schema: SchemaMetadata,
    input: SeedInput,
    validators?: SeedValidators,
    options: SeedOptions = {},
): Promise<SeedResult> {
    const context: SeedContext = {
        db,
        schema,
        validators,
        batchSize: options.batchSize ?? 100,
        transactionMode: resolveSeedTransactionMode(db, options.transactionMode),
        statementIndex: 0,
        physicalColumns: new Map(),
    };
    const response: Record<string, unknown[]> = {};

    try {
        validateSeedInput(schema, input);
        if (context.transactionMode === "manual") {
            await db.execute("begin");
        }

        for (const [tableName, rows] of Object.entries(input)) {
            const table = schema.tables[tableName];
            response[tableName] = await insertSeedRows(
                context,
                table,
                rows.map((row, index) => ({ row, path: `${tableName}[${index}]` })),
            );
        }

        if (context.transactionMode === "manual") {
            await db.execute("commit");
        }
        return { kind: "success", response };
    } catch (error) {
        if (context.transactionMode === "manual") {
            try {
                await db.execute("rollback");
            } catch (_) {
                // Ignore rollback failures; the original error is more useful.
            }
        }

        return {
            kind: "error",
            error: {
                errorType: error instanceof SeedInputError ? "InvalidInput" : "DatabaseError",
                message: error instanceof Error ? error.message : "Seed failed",
            },
        };
    }
}

function resolveSeedTransactionMode(
    db: Client,
    requestedMode: SeedOptions["transactionMode"],
): "manual" | "batch" | "none" {
    if (requestedMode === "manual" || requestedMode === "none") {
        return requestedMode;
    }

    if (requestedMode === "batch") {
        if (typeof db.batch !== "function") {
            throw new Error("seed batch mode requires a database client with batch support");
        }

        return "batch";
    }

    return typeof db.batch === "function" ? "batch" : "manual";
}

class SeedInputError extends Error { }

function validateSeedInput(schema: SchemaMetadata, input: SeedInput): void {
    if (input == null || typeof input !== "object" || Array.isArray(input)) {
        throw new SeedInputError("seed input must be an object keyed by table name");
    }

    for (const [tableName, rows] of Object.entries(input)) {
        if (!(tableName in schema.tables)) {
            throw new SeedInputError(`unknown seed table '${tableName}'`);
        }
        if (!Array.isArray(rows)) {
            throw new SeedInputError(`seed table '${tableName}' must be an array`);
        }
        rows.forEach((row, index) => {
            if (row == null || typeof row !== "object" || Array.isArray(row)) {
                throw new SeedInputError(`seed row '${tableName}[${index}]' must be an object`);
            }
        });
    }
}

async function insertSeedRow(
    context: SeedContext,
    table: TableMetadata,
    row: SeedRow,
    path: string,
    inheritedValues: Record<string, SeedValue> = {},
): Promise<Record<string, unknown>> {
    const [inserted] = await insertSeedRows(context, table, [{ row, path, inheritedValues }]);
    return inserted;
}

async function insertSeedRows(
    context: SeedContext,
    table: TableMetadata,
    rows: Array<{ row: SeedRow; path: string; inheritedValues?: Record<string, SeedValue> }>,
): Promise<Record<string, unknown>[]> {
    const preparedRows: SeedPreparedRow[] = [];

    for (const item of rows) {
        preparedRows.push(await prepareSeedRow(context, table, item.row, item.path, item.inheritedValues ?? {}));
    }

    const insertedRows = await insertScalarRows(context, table, preparedRows);

    for (let rowIndex = 0; rowIndex < preparedRows.length; rowIndex += 1) {
        const prepared = preparedRows[rowIndex];
        const inserted = insertedRows[rowIndex];

        for (const nested of prepared.nestedValues.filter(({ link }) => isParentToChildLink(table, link))) {
            const linkedTable = context.schema.tables[nested.link.to.table];
            if (!linkedTable) {
                throw new SeedInputError(`seed link '${prepared.path}.${nested.key}' points to unknown table '${nested.link.to.table}'`);
            }
            const parentValue = inserted[nested.link.from];
            if (!isSeedValue(parentValue)) {
                throw new SeedInputError(`seed link '${prepared.path}.${nested.key}' cannot derive '${nested.link.from}' from inserted parent row`);
            }

            const childRows = Array.isArray(nested.value) ? nested.value : [nested.value];
            const nestedResult = await insertSeedRows(
                context,
                linkedTable,
                childRows.map((childRow, index) => ({
                    row: childRow,
                    path: `${prepared.path}.${nested.key}[${index}]`,
                    inheritedValues: { [nested.link.to.column]: parentValue },
                })),
            );
            inserted[nested.key] = Array.isArray(nested.value) ? nestedResult : nestedResult[0];
        }
    }

    return insertedRows;
}

async function prepareSeedRow(
    context: SeedContext,
    table: TableMetadata,
    row: SeedRow,
    path: string,
    inheritedValues: Record<string, SeedValue> = {},
): Promise<SeedPreparedRow> {
    const scalarValues: Record<string, SeedValue> = { ...inheritedValues };
    const nestedValues: Array<{ key: string; link: LinkInfo; value: SeedRow | SeedRow[] }> = [];
    const columns = new Set((table.columns ?? []).map((column) => column.name));

    for (const [key, value] of Object.entries(row)) {
        if (columns.has(key)) {
            if (!isSeedValue(value)) {
                throw new SeedInputError(`seed field '${path}.${key}' must be a scalar column value`);
            }
            assertCanonicalDiscriminators(value, `${path}.${key}`);
            const validatedValue = validateSeedColumnValue(context, table, key, value, `${path}.${key}`);
            if (key in scalarValues && !sameSeedValue(scalarValues[key], validatedValue)) {
                throw new SeedInputError(`seed field '${path}.${key}' conflicts with a value derived from its parent link`);
            }
            scalarValues[key] = validatedValue;
            continue;
        }

        const link = table.links[key];
        if (!link) {
            throw new SeedInputError(`unknown seed field '${path}.${key}'; expected a column or link on table '${table.name}'`);
        }
        if (!isSeedLinkValue(value)) {
            throw new SeedInputError(`seed link '${path}.${key}' must be an object or array of objects`);
        }
        assertCanonicalDiscriminators(value, `${path}.${key}`);
        nestedValues.push({ key, link, value });
    }

    for (const nested of nestedValues.filter(({ link }) => !isParentToChildLink(table, link))) {
        const linkedTable = context.schema.tables[nested.link.to.table];
        if (!linkedTable) {
            throw new SeedInputError(`seed link '${path}.${nested.key}' points to unknown table '${nested.link.to.table}'`);
        }
        if (Array.isArray(nested.value)) {
            throw new SeedInputError(`seed link '${path}.${nested.key}' must be a single object because '${nested.link.from}' is set on '${table.name}'`);
        }
        const linkedRow = await insertSeedRow(context, linkedTable, nested.value, `${path}.${nested.key}`);
        const linkedValue = linkedRow[nested.link.to.column];
        if (!isSeedValue(linkedValue)) {
            throw new SeedInputError(`seed link '${path}.${nested.key}' did not return '${nested.link.to.column}'`);
        }
        if (nested.link.from in scalarValues && !sameSeedValue(scalarValues[nested.link.from], linkedValue)) {
            throw new SeedInputError(`seed field '${path}.${nested.link.from}' conflicts with nested link '${nested.key}'`);
        }
        scalarValues[nested.link.from] = linkedValue;
    }

    return { path, scalarValues, nestedValues };
}

function validateSeedColumnValue(
    context: SeedContext,
    table: TableMetadata,
    columnName: string,
    value: SeedValue,
    path: string,
): SeedValue {
    const validator = context.validators?.[table.name]?.[columnName];
    if (!validator || value == null) {
        return value;
    }

    const result = decodeOrError(validator, value, path);
    if (!result.valid) {
        throw new SeedInputError(`invalid seed field '${path}': ${result.error}`);
    }
    return result.value as SeedValue;
}

async function insertScalarRow(
    context: SeedContext,
    table: TableMetadata,
    values: Record<string, SeedValue>,
    path: string,
): Promise<Record<string, unknown>> {
    const [inserted] = await insertScalarRows(context, table, [{ path, scalarValues: values, nestedValues: [] }]);
    return inserted;
}

async function insertScalarRows(
    context: SeedContext,
    table: TableMetadata,
    rows: SeedPreparedRow[],
): Promise<Record<string, unknown>[]> {
    const insertedRows: Record<string, unknown>[] = [];

    for (let offset = 0; offset < rows.length; offset += context.batchSize) {
        const chunk = rows.slice(offset, offset + context.batchSize);
        const statements = await Promise.all(chunk.map(async ({ scalarValues, path }) => {
            const inputColumnNames = Object.keys(scalarValues);
            const knownColumns = new Set((table.columns ?? []).map((column) => column.name));

            for (const columnName of inputColumnNames) {
                if (!knownColumns.has(columnName)) {
                    throw new SeedInputError(`unknown seed column '${path}.${columnName}' on table '${table.name}'`);
                }
            }

            const normalizedValues = await normalizeSeedValues(context, table, scalarValues);
            const columnNames = Object.keys(normalizedValues);
            const args: Record<string, SeedPrimitive> = {};
            const placeholders = columnNames.map((columnName) => {
                const argName = `seed_${context.statementIndex++}`;
                args[argName] = normalizedValues[columnName];
                return `$${argName}`;
            });
            const sql = columnNames.length === 0
                ? `insert into ${quoteIdentifier(table.name)} default values returning *`
                : `insert into ${quoteIdentifier(table.name)} (${columnNames.map(quoteIdentifier).join(", ")}) values (${placeholders.join(", ")}) returning *`;

            return { sql, args };
        }));

        try {
            const results = await executeSeedInsertStatements(context, statements);
            for (const result of results) {
                const row = result.rows?.[0];
                if (!row) {
                    throw new Error("insert returned no rows");
                }
                insertedRows.push(formatReturnedSeedRow(table, row as Record<string, unknown>));
            }
        } catch (error) {
            const message = error instanceof Error ? error.message : "database insert failed";
            throw new Error(`failed to insert seed rows starting at '${chunk[0]?.path ?? table.name}' into '${table.name}': ${message}`);
        }
    }

    return insertedRows;
}

async function executeSeedInsertStatements(
    context: SeedContext,
    statements: InStatement[],
): Promise<Array<{ rows?: unknown[] }>> {
    if (statements.length === 0) {
        return [];
    }

    if (context.transactionMode === "batch") {
        if (typeof context.db.batch !== "function") {
            throw new Error("seed batch mode requires a database client with batch support");
        }

        return await context.db.batch(statements) as Array<{ rows?: unknown[] }>;
    }

    const results: Array<{ rows?: unknown[] }> = [];
    for (const statement of statements) {
        results.push(await context.db.execute(statement) as { rows?: unknown[] });
    }

    return results;
}

async function normalizeSeedValues(
    context: SeedContext,
    table: TableMetadata,
    values: Record<string, SeedValue>,
): Promise<Record<string, SeedPrimitive>> {
    const physicalColumns = await getPhysicalColumns(context, table);
    const logicalColumns = new Map((table.columns ?? []).map((column) => [column.name, column]));
    const normalized: Record<string, SeedPrimitive> = {};

    for (const [columnName, value] of Object.entries(values)) {
        const column = logicalColumns.get(columnName);
        if (!column) {
            normalized[columnName] = toSqlSeedValue(value);
            continue;
        }

        if (column.type.startsWith("Json")) {
            normalized[columnName] = toJsonSqlValue(value);
            continue;
        }

        if (column.type === "DateTime") {
            normalized[columnName] = toDateTimeSqlValue(value, `${table.name}.${columnName}`);
            continue;
        }

        if (isConstructedValue(value) && hasNestedPhysicalColumns(physicalColumns, columnName)) {
            flattenConstructedValue(normalized, physicalColumns, columnName, value);
            continue;
        }

        normalized[columnName] = toSqlSeedValue(value);
    }

    return normalized;
}

async function getPhysicalColumns(context: SeedContext, table: TableMetadata): Promise<Set<string>> {
    const cached = context.physicalColumns.get(table.name);
    if (cached) {
        return cached;
    }

    const result = await context.db.execute(`pragma table_info(${quoteIdentifier(table.name)})`);
    const columns = new Set<string>();
    for (const row of result.rows ?? []) {
        const name = (row as Record<string, unknown>).name;
        if (typeof name === "string") {
            columns.add(name);
        }
    }
    context.physicalColumns.set(table.name, columns);
    return columns;
}

function flattenConstructedValue(
    output: Record<string, SeedPrimitive>,
    physicalColumns: Set<string>,
    prefix: string,
    value: SeedValue,
): void {
    if (!isConstructedValue(value)) {
        output[prefix] = toSqlSeedValue(value);
        return;
    }

    const discriminator = constructedDiscriminator(value);
    if (discriminator !== undefined && physicalColumns.has(prefix)) {
        output[prefix] = discriminator;
    }

    if (value == null || typeof value !== "object" || Array.isArray(value) || value instanceof Uint8Array) {
        return;
    }

    for (const [fieldName, fieldValue] of Object.entries(value)) {
        if (fieldName === "_type") {
            continue;
        }

        const fieldPrefix = `${prefix}__${fieldName}`;
        if (!hasPhysicalColumnAtOrBelow(physicalColumns, fieldPrefix)) {
            continue;
        }

        if (isConstructedValue(fieldValue) && hasNestedPhysicalColumns(physicalColumns, fieldPrefix)) {
            flattenConstructedValue(output, physicalColumns, fieldPrefix, fieldValue);
        } else if (physicalColumns.has(fieldPrefix)) {
            output[fieldPrefix] = toSqlSeedValue(fieldValue);
        }
    }
}

function formatReturnedSeedRow(table: TableMetadata, row: Record<string, unknown>): Record<string, unknown> {
    const formatted: Record<string, unknown> = {};

    for (const column of table.columns ?? []) {
        if (column.type.startsWith("Json")) {
            formatted[column.name] = parseJsonSqlValue(row[column.name]);
        } else if (hasReturnedNestedColumns(row, column.name)) {
            formatted[column.name] = reconstructConstructedValue(row, column.name);
        } else {
            formatted[column.name] = normalizeReturnedScalar(column.type, row[column.name]);
        }
    }

    return formatted;
}

function reconstructConstructedValue(row: Record<string, unknown>, prefix: string): unknown {
    const discriminator = row[prefix];
    if (discriminator == null) {
        return null;
    }

    const result: Record<string, unknown> = { _type: discriminator };
    const childFields = directChildFields(row, prefix);

    for (const field of childFields) {
        const fieldPrefix = `${prefix}__${field}`;
        if (hasReturnedNestedColumns(row, fieldPrefix)) {
            result[field] = reconstructConstructedValue(row, fieldPrefix);
        } else {
            result[field] = parseJsonSqlValue(row[fieldPrefix]);
        }
    }

    return result;
}

function directChildFields(row: Record<string, unknown>, prefix: string): string[] {
    const marker = `${prefix}__`;
    const fields = new Set<string>();
    for (const key of Object.keys(row)) {
        if (!key.startsWith(marker)) {
            continue;
        }
        const rest = key.slice(marker.length);
        fields.add(rest.split("__")[0]);
    }
    return [...fields].sort();
}

function normalizeReturnedScalar(type: string, value: unknown): unknown {
    if (type === "Bool") {
        return value === true || value === 1;
    }
    return value;
}

function hasReturnedNestedColumns(row: Record<string, unknown>, prefix: string): boolean {
    return Object.keys(row).some((key) => key.startsWith(`${prefix}__`));
}

function hasPhysicalColumnAtOrBelow(physicalColumns: Set<string>, prefix: string): boolean {
    if (physicalColumns.has(prefix)) {
        return true;
    }
    for (const column of physicalColumns) {
        if (column.startsWith(`${prefix}__`)) {
            return true;
        }
    }
    return false;
}

function hasNestedPhysicalColumns(physicalColumns: Set<string>, prefix: string): boolean {
    for (const column of physicalColumns) {
        if (column.startsWith(`${prefix}__`)) {
            return true;
        }
    }
    return false;
}

function isSeedValue(value: unknown): value is SeedValue {
    return value == null
        || typeof value === "boolean"
        || typeof value === "number"
        || typeof value === "string"
        || value instanceof Uint8Array
        || Array.isArray(value)
        || typeof value === "object";
}

function isSeedLinkValue(value: unknown): value is SeedRow | SeedRow[] {
    if (Array.isArray(value)) {
        return value.every((item) => item != null && typeof item === "object" && !Array.isArray(item) && !(item instanceof Uint8Array));
    }
    return value != null && typeof value === "object" && !(value instanceof Uint8Array);
}

function assertCanonicalDiscriminators(value: unknown, path: string): void {
    if (value == null || typeof value !== "object" || value instanceof Uint8Array) {
        return;
    }
    if (Array.isArray(value)) {
        value.forEach((item, index) => assertCanonicalDiscriminators(item, `${path}[${index}]`));
        return;
    }

    const record = value as Record<string, unknown>;
    for (const legacyKey of ["type", "type_", "$" ] as const) {
        if (legacyKey in record) {
            throw new SeedInputError(`seed value '${path}' uses '${legacyKey}' as a discriminator; use '_type'`);
        }
    }

    for (const [key, nested] of Object.entries(record)) {
        assertCanonicalDiscriminators(nested, `${path}.${key}`);
    }
}

function sameSeedValue(a: SeedValue, b: SeedValue): boolean {
    if (a instanceof Date || b instanceof Date) {
        return a instanceof Date && b instanceof Date && a.getTime() === b.getTime();
    }
    if (a instanceof Uint8Array || b instanceof Uint8Array) {
        return a instanceof Uint8Array && b instanceof Uint8Array && a.length === b.length && a.every((value, index) => value === b[index]);
    }
    return a === b;
}

function isConstructedValue(value: unknown): value is SeedValue {
    return typeof value === "string" || constructedDiscriminator(value) !== undefined;
}

function constructedDiscriminator(value: unknown): string | undefined {
    if (typeof value === "string") {
        return value;
    }
    if (value == null || typeof value !== "object" || Array.isArray(value) || value instanceof Uint8Array) {
        return undefined;
    }
    const record = value as Record<string, unknown>;
    if (typeof record._type === "string") {
        return record._type;
    }
    return undefined;
}

function toSqlSeedValue(value: SeedValue): SeedPrimitive {
    if (value instanceof Date) {
        if (Number.isNaN(value.getTime())) {
            throw new SeedInputError("invalid Date seed value");
        }
        return Math.floor(value.getTime() / 1000);
    }
    if (value == null || typeof value === "boolean" || typeof value === "number" || typeof value === "string" || value instanceof Uint8Array) {
        return value;
    }
    return JSON.stringify(normalizeSeedDates(value));
}

function toDateTimeSqlValue(value: SeedValue, path: string): SeedPrimitive {
    if (value == null) {
        return value;
    }

    if (value instanceof Date) {
        if (!Number.isNaN(value.getTime())) {
            return Math.floor(value.getTime() / 1000);
        }
        throw new SeedInputError(`invalid DateTime seed field '${path}'`);
    }

    if (typeof value === "number") {
        if (Number.isSafeInteger(value)) {
            return value;
        }
        throw new SeedInputError(`invalid DateTime seed field '${path}': expected whole Unix seconds`);
    }

    if (typeof value === "string") {
        const trimmed = value.trim();
        if (/^[+-]?\d+$/.test(trimmed)) {
            const asNumber = Number(trimmed);
            if (Number.isSafeInteger(asNumber)) {
                return asNumber;
            }
            throw new SeedInputError(`invalid DateTime seed field '${path}': invalid Unix seconds`);
        }

        const parsed = parseRfc3339(trimmed);
        if (parsed) {
            return Math.floor(parsed.getTime() / 1000);
        }
    }

    throw new SeedInputError(`invalid DateTime seed field '${path}'`);
}

function toJsonSqlValue(value: SeedValue): SeedPrimitive {
    if (value == null || typeof value === "string" || value instanceof Uint8Array) {
        return value;
    }
    return JSON.stringify(normalizeSeedDates(value));
}

function normalizeSeedDates(value: SeedValue): unknown {
    if (value instanceof Date) {
        if (Number.isNaN(value.getTime())) {
            throw new SeedInputError("invalid Date seed value");
        }
        return Math.floor(value.getTime() / 1000);
    }
    if (Array.isArray(value)) {
        return value.map(normalizeSeedDates);
    }
    if (value !== null && typeof value === "object" && !(value instanceof Uint8Array)) {
        return Object.fromEntries(
            Object.entries(value).map(([key, nested]) => [key, normalizeSeedDates(nested as SeedValue)])
        );
    }
    return value;
}

function parseRfc3339(value: string): Date | null {
    const match = /^(\d{4})-(\d{2})-(\d{2})[Tt](?:[01]\d|2[0-3]):[0-5]\d:[0-5]\d(?:\.\d+)?(?:[Zz]|[+-](?:[01]\d|2[0-3]):[0-5]\d)$/.exec(value);
    if (!match) {
        return null;
    }

    const year = Number(match[1]);
    const month = Number(match[2]);
    const day = Number(match[3]);
    const leapYear = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
    const daysInMonth = [31, leapYear ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if (month < 1 || month > 12 || day < 1 || day > daysInMonth[month - 1]) {
        return null;
    }

    const parsed = new Date(value);
    return Number.isNaN(parsed.getTime()) ? null : parsed;
}

function parseJsonSqlValue(value: unknown): unknown {
    if (typeof value !== "string") {
        return value;
    }
    const trimmed = value.trim();
    if (!trimmed.startsWith("{") && !trimmed.startsWith("[")) {
        return value;
    }
    try {
        return JSON.parse(value);
    } catch (_) {
        return value;
    }
}

function isParentToChildLink(table: TableMetadata, link: LinkInfo): boolean {
    return (table.columns ?? []).some((column) => column.name === link.from && column.primary);
}

function quoteIdentifier(identifier: string): string {
    return `"${identifier.replace(/"/g, '""')}"`;
}
