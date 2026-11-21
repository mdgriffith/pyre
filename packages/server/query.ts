import { Client, InStatement, type Transaction } from "@libsql/client";
import type { SchemaMetadata } from "@pyre/core";
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
    namespace?: string;
    /** @deprecated Seed no longer batches inserts independently. */
    batchSize?: number;
    /** @deprecated Only a single write transaction is supported. */
    transactionMode?: "manual" | "batch" | "none";
}

export type SeedResult = {
    kind: "success";
    response: Record<string, unknown[]>;
    error?: never;
} | {
    kind: "error";
    response?: never;
    error: {
        errorType: "InvalidInput" | "DatabaseError" | "OutcomeUnknown";
        message: string;
    };
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
    let operationIndex: number | undefined;
    let revision: { databaseEpoch: string; serverRevision: number } | undefined;
    try {
        const statements: InStatement[] = [];
        const prepared = operations.map((operation, index) => {
            operationIndex = index;
            const metadata = queryMap[operation.queryId];
            const input = metadata.InputValidator.safeParse(operation.input);
            const session = metadata.SessionValidator.safeParse(executingSession);
            if (!input.success || !session.success) throw new Error("Operation validation failed");
            const args = buildArgs(input.data ?? {}, session.data ?? {}, metadata.session_args, metadata.optional_input_args, metadata.json_input_args);
            const sql = options.mode === "sync" ? metadata.syncSql ?? metadata.sql : metadata.sql;
            const bound = toSqlStatements(sql, args);
            const checked = metadata.generatedEdit && (options.mode === "sync"
                ? metadata.generatedEdit.syncWriteStatement ?? metadata.generatedEdit.writeStatement
                : metadata.generatedEdit.writeStatement);
            if (checked !== undefined && (!Number.isInteger(checked) || checked < 0 || checked >= bound.length)) {
                throw new Error("Invalid generated edit metadata");
            }
            const resultIndexes: number[] = [];
            let countIndex: number | undefined;
            for (const [statementIndex, statement] of bound.entries()) {
                resultIndexes.push(statements.length);
                statements.push(statement);
                if (statementIndex === checked) {
                    // Capture immediately: later writes would overwrite changes().
                    // RETURNING rowsAffected is not reliable in every adapter.
                    countIndex = statements.length;
                    statements.push("select changes() as count");
                }
            }
            return { operation, sql, resultIndexes, countIndex };
        });
        const allocateRevision = options.allocateSyncRevision && operations.some(op => {
            const query = queryMap[op.queryId];
            return hasSyncEffect(query, options.mode);
        });
        const revisionIndex = statements.length;
        if (allocateRevision) statements.push("update _pyre_sync set server_revision = server_revision + 1 where id = 1 returning database_epoch, server_revision");
        // Database-level batch errors may not carry a statement index. Do not
        // attribute those to an arbitrary operation; our own checks retain it.
        operationIndex = undefined;
        await writeTransaction(db, async tx => {
            // One execution request, then one commit/rollback request. Keep all
            // checks and result formatting before commit, including sync metadata.
            const results = await tx.batch(statements);
            if (results.length !== statements.length) throw new Error("Invalid batch result count");
            for (const [index, item] of prepared.entries()) {
                operationIndex = index;
                if (item.countIndex !== undefined && Number(results[item.countIndex].rows[0]?.count) !== 1) {
                    throw new Error("Generated edit must affect exactly one row");
                }
                const output = item.resultIndexes.map(index => results[index]);
                responses.push({ index, queryId: item.operation.queryId, result: formatResultData(item.sql, output) });
                affected.push(...extractAffectedRowGroups(item.sql, output));
            }
            operationIndex = undefined;
            finalGroups = combineAffectedRows(affected);
            if (allocateRevision) {
                const stamp = results[revisionIndex].rows[0];
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

type SeedBind = { type: "null" } | { type: "blob"; value: number[] }
    | { type: "integer" | "real" | "text"; value: bigint | number | string };

/** Insert trusted fixtures in one write transaction using the target's installed schema. */
export function seed(db: Client, input: SeedInput, options?: SeedOptions): Promise<SeedResult>;
/** @deprecated Pass input and options only. The target schema replaces metadata and validators. */
export function seed(db: Client, schema: SchemaMetadata, input: SeedInput, validators?: SeedValidators, options?: SeedOptions): Promise<SeedResult>;
export async function seed(
    db: Client,
    inputOrSchema: SeedInput | SchemaMetadata,
    optionsOrInput?: SeedOptions | SeedInput,
    _validators?: SeedValidators,
    legacyOptions?: SeedOptions,
): Promise<SeedResult> {
    const fail = (errorType: NonNullable<SeedResult["error"]>["errorType"], cause: unknown): SeedResult => ({
        kind: "error", error: { errorType, message: seedErrorMessage(cause) },
    });
    let entries: [string, unknown][];
    let options: SeedOptions;
    try {
        const legacy = inputOrSchema != null && typeof inputOrSchema === "object" && "queryFieldToTable" in inputOrSchema
            && !Array.isArray(inputOrSchema.queryFieldToTable) && "tables" in inputOrSchema;
        const input = (legacy ? optionsOrInput : inputOrSchema) as SeedInput;
        options = (legacy ? legacyOptions : optionsOrInput as SeedOptions | undefined) ?? {};
        if (options.transactionMode !== undefined && options.transactionMode !== "manual") {
            return fail("InvalidInput", "Seed requires one write transaction; batch and none modes are unsupported");
        }
        const value = fixtureValue(input);
        if (value === null || typeof value !== "object" || Array.isArray(value)) {
            return fail("InvalidInput", "seed input must be an object");
        }
        // A JSON map in Rust sorts its keys. Send pairs to keep JS table order.
        entries = Object.entries(value);
    } catch (error) {
        return fail("InvalidInput", error);
    }

    let tx: Transaction | undefined;
    let committing = false;
    try {
        const [{ introspectDatabase }, wasm] = await Promise.all([
            import("./schema"), import("./wasm/pyre_wasm.js"),
        ]);
        // The local libSQL adapter detaches the connection when opening a transaction.
        if (db.protocol === "file") {
            const databases = await db.execute("pragma database_list");
            if (!databases.rows.some(row => row.name === "main" && typeof row.file === "string" && row.file.length > 0)) {
                return fail("DatabaseError", "Seed requires a file-backed local database");
            }
        }
        const transaction = tx = await db.transaction("write");
        const { initialized, introspection } = await introspectDatabase(transaction);
        if (!initialized || !introspection.schema_source) {
            throw new Error("Database does not contain an initialized Pyre schema");
        }
        const response = await wasm.ingest_fixture(introspection, entries, async ({ sql, args, path }: {
            sql: string; args: SeedBind[]; path: string;
        }) => {
            try {
                const result = await transaction.execute({
                    sql,
                    args: args.map(arg => arg.type === "null" ? null
                        : arg.type === "blob" ? Uint8Array.from(arg.value) : arg.value),
                });
                if (result.rows.length !== 1) throw new Error("insert did not return one row");
                return Object.fromEntries(result.columns.map(column => {
                    const cell = result.rows[0][column];
                    return [column, cell instanceof ArrayBuffer ? Array.from(new Uint8Array(cell)) : cell];
                }));
            } catch (error) {
                throw new Error(`${path}: ${seedErrorMessage(error)}`);
            }
        }, options.namespace);
        committing = true;
        await transaction.commit();
        return { kind: "success", response };
    } catch (error) {
        if (tx) {
            if (committing) {
                return fail("OutcomeUnknown", `Seed commit outcome unknown: ${seedErrorMessage(error)}`);
            }
            try {
                if (tx.closed) throw new Error("transaction closed without confirming rollback");
                await tx.rollback();
            } catch (rollback) {
                return fail("OutcomeUnknown", `${seedErrorMessage(error)}; seed rollback outcome unknown: ${seedErrorMessage(rollback)}`);
            }
        }
        return fail(typeof error === "string" ? "InvalidInput" : "DatabaseError", error);
    } finally {
        try { tx?.close(); } catch { /* The commit or rollback result determines the outcome. */ }
    }
}

function seedErrorMessage(error: unknown): string {
    return error instanceof Error ? error.message : String(error);
}

// Convert legacy JS conveniences to the canonical JSON representation. Column
// interpretation and validation belong to the shared fixture engine.
function fixtureValue(value: unknown): unknown {
    if (value instanceof Date) {
        const seconds = Math.floor(value.getTime() / 1000);
        if (!Number.isFinite(seconds)) throw new Error("Invalid Date seed value");
        return seconds;
    }
    if (value instanceof Uint8Array) return Array.from(value);
    if (Array.isArray(value)) return value.map(fixtureValue);
    if (value != null && typeof value === "object") {
        return Object.fromEntries(Object.entries(value)
            .filter(([, field]) => field !== undefined)
            .map(([key, field]) => [key, fixtureValue(field)]));
    }
    return value;
}
