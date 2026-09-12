import { Client, InStatement, type Transaction } from "@libsql/client";
import type { LinkInfo, SchemaMetadata, TableMetadata } from "@pyre/core";
import { z, type ZodType } from "zod";
import { buildArgs, executeStatements, formatResultData, TargetNotWritable, toSqlStatements, type GeneratedEdit, type JsonSessionValidators, type SqlInfo } from "./runtime/sql";

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
    session_args: string[];
    json_session_args?: string[];
    json_session_validators?: JsonSessionValidators;
    optional_input_args: string[];
    json_input_args: string[];
    InputValidator: Validator<any>;
    SessionValidator: Validator<any>;
    ReturnData?: Validator<any>;
    generatedEdit?: GeneratedEdit;
}

/**
 * Map of query IDs to their metadata.
 */
export interface QueryMap {
    [queryId: string]: QueryMetadata;
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
    };
    /**
     * Broadcast sync deltas to connected clients.
     * Always present. Revisioned named mutations also publish catchup hints for zero-row writes.
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
    committedRevision?: SyncResult,
) => Promise<SyncResult | void>;

export interface RunOptions {
    mode?: "normal" | "sync";
    /** Named mutation sync revisions must commit atomically with their writes. */
    commitSyncRevision?: boolean;
}

export const MAX_BATCH_OPERATIONS = 100;
export const MAX_BATCH_PAYLOAD_BYTES = 1024 * 1024;

/** Trusted server-side allowlist, never supplied by the request. */
export interface BatchManifest {
    version: 1;
    /** Trusted fingerprint emitted alongside the compiled queries. */
    manifestVersion: string;
    /** Compiler-owned global schema/session/permission digest. */
    compiledContract?: string;
    /** Namespace-scoped permission/schema digests. Missing entries cannot replace. */
    replacementContracts?: Readonly<Record<string, string>>;
    queries: QueryMap;
    SessionValidator: Validator<any>;
}

/** The route/binding must resolve these authorities independently of the request. */
export interface BatchAuthority {
    databaseId: string;
    namespace: string;
    manifest: string;
    instance: string;
    authGeneration: number;
}

export interface BatchRequest {
    version: 1;
    databaseId: string;
    namespace: string;
    manifest: string;
    databaseEpoch: string;
    instance: string;
    authGeneration: number;
    requestId: string;
    sequence: number;
    operations: readonly { operation: string; input: unknown }[];
}

export interface BatchResponse {
    databaseId: string;
    namespace: string;
    manifest: string;
    databaseEpoch: string;
    instance: string;
    authGeneration: number;
    requestId: string;
    status: "accepted" | "confirmed";
    results: { index: number; operation: string; value: unknown }[];
    commitRevision?: number;
    reconciliation?: { kind: "replaceRequired"; atLeast: number; invalidate: true; minimumSafeRevision: number };
}

export type BatchResult = { kind: "success"; response: BatchResponse } | {
    kind: "error";
    error: { errorType: string; message: string; index?: number };
} | {
    kind: "unknown";
    error: { errorType: "OutcomeUnknown"; message: string };
};

const batchRequestValidator = z.strictObject({
    version: z.literal(1), databaseId: z.string().min(1), namespace: z.string().min(1),
    manifest: z.string().min(1), databaseEpoch: z.string(), instance: z.string().min(1),
    authGeneration: z.number().int().nonnegative(), requestId: z.string().min(1),
    sequence: z.number().int().positive(),
    operations: z.array(z.strictObject({ operation: z.string(), input: z.unknown().nonoptional() })).max(MAX_BATCH_OPERATIONS),
});

class BatchError extends Error {
    constructor(readonly errorType: string, readonly index?: number) { super(errorType); }
}

// Reject fields removed by strip-mode object codecs, including nested protected fields.
function assertNoStrippedFields(input: unknown, decoded: unknown): void {
    if (input === null || typeof input !== "object" || input instanceof Date || input instanceof Uint8Array) return;
    // Generated enum codecs accept both a string and the canonical tag-only object.
    if (typeof decoded === "string" && Object.keys(input).length === 1 && Object.hasOwn(input, "_type") && (input as any)._type === decoded) return;
    if (decoded === null || typeof decoded !== "object") throw new BatchError("InvalidInput");
    for (const key of Object.keys(input)) {
        if (!Object.hasOwn(decoded, key)) throw new BatchError("InvalidInput");
        assertNoStrippedFields((input as any)[key], (decoded as any)[key]);
    }
}

const batchQueues = new Map<string, Promise<unknown>>();

/** Executes captured compiled operations, never public runners with independent commits. */
export function runBatch(
    db: Client,
    manifest: BatchManifest,
    authority: BatchAuthority,
    request: BatchRequest,
    executingSession: Session,
    publish?: (result: Extract<BatchResult, { kind: "success" }>) => void | Promise<void>,
    /** Trusted in-process submissions capture their epoch inside the queued transaction.
     * Never enable this for a client-supplied request. */
    captureDatabaseEpoch = false,
): Promise<BatchResult> {
    let prepared: { operation: string; query: QueryMetadata; statements: ReturnType<typeof toSqlStatements> }[];
    let capturedAuthority: BatchAuthority;
    let captured: BatchRequest;
    try {
        // Capture and validate synchronously, before queue/transaction acquisition or any I/O.
        capturedAuthority = structuredClone(authority);
        authority = capturedAuthority;
        const parsedRequest = batchRequestValidator.safeParse(structuredClone(request));
        if (!parsedRequest.success) throw new BatchError("InvalidInput");
        captured = parsedRequest.data;
        let session: Session;
        try { session = structuredClone(executingSession); }
        catch { throw new BatchError("InvalidSession"); }
        const serialized = JSON.stringify(captured);
        if (!serialized || new TextEncoder().encode(serialized).byteLength > MAX_BATCH_PAYLOAD_BYTES)
            throw new BatchError("InvalidInput");
        if (typeof authority.databaseId !== "string" || !authority.databaseId.trim()
            || typeof authority.namespace !== "string" || !authority.namespace
            || typeof authority.manifest !== "string" || !authority.manifest
            || captured.databaseId !== authority.databaseId || captured.namespace !== authority.namespace
            || captured.manifest !== authority.manifest || captured.instance !== authority.instance
            || captured.authGeneration !== authority.authGeneration || manifest.version !== 1
            || manifest.manifestVersion !== authority.manifest)
            throw new BatchError("InvalidInput");
        const effectiveSession = manifest.SessionValidator.safeParse(session);
        if (!effectiveSession.success) throw new BatchError("InvalidSession");
        // Only declared, decoded session fields are SQL authority; application claims are ignored.
        prepared = captured.operations.map((member, index) => {
            try {
                if (!member || typeof member.operation !== "string" || !Object.hasOwn(manifest.queries, member.operation))
                    throw new BatchError("UnknownQuery");
                const source = manifest.queries[member.operation];
                const query = { ...source, sql: structuredClone(source.sql), generatedEdit: structuredClone(source.generatedEdit) };
                if (query.id !== member.operation || query.primary_db !== authority.namespace || (query.attached_dbs?.length ?? 0) > 0
                    || !["insert", "update", "delete", "transaction"].includes(query.operation ?? ""))
                    throw new BatchError("InvalidInput");
                const edit = query.generatedEdit;
                if (edit) {
                    if (!["create", "update", "delete"].includes(edit.kind) || edit.writeStatementIndices.length !== 1
                        || edit.writeStatementIndices.some(i => !Number.isInteger(i) || i < 0 || i >= query.sql.length
                            || query.sql[i].include !== true))
                        throw new BatchError("InvalidInput");
                    if (edit.kind === "update" && !edit.writableInputs.some(key => member.input !== null && typeof member.input === "object" && Object.hasOwn(member.input, key) && (member.input as any)[key] !== undefined))
                        throw new BatchError("InvalidEdit");
                }
                const input = query.InputValidator.safeParse(member.input);
                if (!input.success) throw new BatchError("InvalidInput");
                assertNoStrippedFields(member.input, input.data);
                return { operation: member.operation, query, statements: toSqlStatements(query.sql, buildArgs(
                    input.data, effectiveSession.data, query.session_args, query.optional_input_args, query.json_input_args, query.json_session_args, query.json_session_validators,
                )) };
            } catch (error) {
                throw new BatchError(error instanceof BatchError ? error.errorType : "InvalidInput", index);
            }
        });
    } catch (error) {
        return Promise.resolve(batchFailure(error instanceof BatchError ? error : new BatchError("InvalidInput")));
    }

    const queueKey = capturedAuthority.databaseId;
    const previous = batchQueues.get(queueKey) ?? Promise.resolve();
    const execution = previous.then(async (): Promise<BatchResult> => {
        const response: BatchResponse = {
            databaseId: captured.databaseId, namespace: captured.namespace, manifest: captured.manifest,
            databaseEpoch: captured.databaseEpoch, instance: captured.instance, authGeneration: captured.authGeneration,
            requestId: captured.requestId, status: "confirmed", results: [],
        };
        const result: Extract<BatchResult, { kind: "success" }> = { kind: "success", response };
        if (prepared.length === 0) return result;
        let tx: Transaction | undefined;
        let index: number | undefined;
        let committing = false;
        try {
            if (db.protocol === "file") {
                // Local libsql transaction() detaches the client's connection. Check before
                // detachment: SQLite reports an empty main.file for memory/temporary databases.
                const databases = await db.execute("pragma database_list");
                const file = databases.rows.find(row => row.name === "main")?.file;
                if (typeof file !== "string" || file.length === 0) throw new BatchError("UnsupportedRuntime");
            }
            tx = await db.transaction("write");
            const databases = await tx.execute("pragma database_list");
            if (databases.rows.some(row => row.name !== "main" && row.name !== "temp")) throw new BatchError("InvalidInput");
            const epoch = await tx.execute("select database_epoch from _pyre_sync where id = 1");
            const databaseEpoch = epoch.rows[0]?.database_epoch;
            if (typeof databaseEpoch !== "string" || !databaseEpoch) throw new BatchError("InvalidInput");
            if (captureDatabaseEpoch) {
                captured.databaseEpoch = databaseEpoch;
                response.databaseEpoch = databaseEpoch;
            } else if (databaseEpoch !== captured.databaseEpoch) throw new BatchError("InvalidInput");
            for (index = 0; index < prepared.length; index++) {
                const { operation, query, statements } = prepared[index];
                // Pass only execute, even though libsql Transaction also exposes batch.
                const sets = await executeStatements({ execute: tx.execute.bind(tx) }, statements, query.generatedEdit);
                let value: unknown = query.generatedEdit ? undefined : formatResultData(query.sql, sets);
                if (query.generatedEdit) {
                    const writes = query.generatedEdit.writeStatementIndices.map(i => sets[i]);
                    // Identity is authorized by the write, never inferred from read-filtered projections.
                    const rows = writes.filter(set => set.rowsAffected === 1).flatMap(set => set.rows);
                    const rawId = rows[0]?._pyreEditId;
                    const id = typeof rawId === "bigint" ? Number(rawId) : rawId;
                    if (rows.length !== 1 || (typeof id === "string"
                        ? id.length !== 36 || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id)
                        : typeof id !== "number" || !Number.isSafeInteger(id)))
                        throw new BatchError("TargetNotWritable", index);
                    value = { id };
                }
                // Generated batch results have the identity contract, not the legacy named row codec.
                if (!query.generatedEdit && query.ReturnData) {
                    const decoded = query.ReturnData.safeParse(value);
                    if (!decoded.success) throw new BatchError("InvalidResult", index);
                    // Validate without replacing wire values with TS-only Date/enum transformations.
                }
                response.results.push({ index, operation, value });
            }
            index = undefined;
            const revision = await nextLiveSyncRevision(tx);
            if (revision.databaseEpoch !== captured.databaseEpoch) throw new BatchError("InvalidInput");
            response.status = "accepted";
            response.commitRevision = revision.serverRevision;
            response.reconciliation = { kind: "replaceRequired", atLeast: revision.serverRevision, invalidate: true, minimumSafeRevision: revision.serverRevision };
            committing = true;
            await tx.commit();
        } catch (error) {
            try { await tx?.rollback(); } catch { /* Preserve the original failure. */ }
            if (committing) return { kind: "unknown", error: { errorType: "OutcomeUnknown", message: "OutcomeUnknown" } };
            return batchFailure(error, index);
        } finally {
            try { tx?.close(); } catch { /* Closing cannot erase commit evidence. */ }
        }
        return result;
    });
    const settled = execution.then(() => undefined, () => undefined);
    batchQueues.set(queueKey, settled);
    void settled.then(() => { if (batchQueues.get(queueKey) === settled) batchQueues.delete(queueKey); });
    return execution.then(async result => {
        // Publication is outside both the transaction and its queue slot.
        if (result.kind === "success" && result.response.commitRevision !== undefined) {
            try { await publish?.(structuredClone(result)); } catch { /* Replacement remains possible without publication. */ }
        }
        return result;
    });
}

function batchFailure(error: unknown, index?: number): BatchResult {
    const internal = error instanceof BatchError ? error.errorType : error instanceof TargetNotWritable ? "TargetNotWritable"
        : error instanceof SyntaxError ? "InvalidInput" : "DatabaseError";
    const errorType = ["InvalidInput", "UnknownQuery", "InvalidResult"].includes(internal) ? "InvalidRequest"
        : ["InvalidSession", "InvalidEdit", "TargetNotWritable"].includes(internal) ? internal : "TransactionFailed";
    if (error instanceof BatchError) index = error.index ?? index;
    return { kind: "error", error: { errorType, message: errorType, ...(index === undefined ? {} : { index }) } };
}

export async function nextLiveSyncRevision(db: Pick<Client, "execute">): Promise<{ databaseEpoch: string; serverRevision: number }> {
    const result = await db.execute("update _pyre_sync set server_revision = server_revision + 1 where id = 1 returning database_epoch, server_revision");
    const databaseEpoch = result.rows[0]?.database_epoch;
    const rawRevision = result.rows[0]?.server_revision;
    const serverRevision = Number(rawRevision);
    if (typeof databaseEpoch !== "string" || (typeof rawRevision !== "number" && typeof rawRevision !== "bigint")
        || !Number.isSafeInteger(serverRevision) || serverRevision < 1)
        throw new Error("Failed to allocate Pyre sync server revision");
    return { databaseEpoch, serverRevision };
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
    queryId: string,
    args: any,
    executingSession: Session,
    connectedSessions?: Map<string, { session: Record<string, SessionValue>;[key: string]: any }>,
    syncDeltas?: SyncDeltasFn,
    originSessionId?: string,
    options: RunOptions = {},
): Promise<QueryResult> {
    // Look up query metadata
    const query = queryMap[queryId];
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
        query.json_session_args,
        query.json_session_validators,
    );

    // Prepare SQL statements
    const useSyncMode = options.mode === "sync";
    const activeSql = useSyncMode ? query.syncSql ?? query.sql : query.sql;
    const sqlStatements = toSqlStatements(activeSql, validArgs);

    // Execute query
    let resultSets;
    let committedRevision: SyncResult | undefined;
    if (options.commitSyncRevision) {
        if (db.protocol === "file") {
            const databases = await db.execute("pragma database_list");
            if (!databases.rows.find(row => row.name === "main")?.file) throw new Error("Unsupported in-memory transaction");
        }
        const tx = await db.transaction("write");
        try {
            resultSets = await executeStatements({ execute: tx.execute.bind(tx) }, sqlStatements);
            committedRevision = await nextLiveSyncRevision(tx);
            await tx.commit();
        } catch (error) {
            try { await tx.rollback(); } catch { /* Preserve the execution/commit failure. */ }
            throw error;
        } finally { try { tx.close(); } catch { /* Closing cannot erase a committed revision. */ } }
    } else {
        resultSets = await executeStatements(db, sqlStatements);
    }
    const affectedRowGroups: unknown[] = extractAffectedRowGroups(activeSql, resultSets);
    const response = formatResultData(activeSql, resultSets);

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
    async function sync(sendToSession: (sessionId: string, message: any) => void): Promise<SyncResult> {
        // Early return if nothing to sync
        if (affectedRowGroups.length === 0 && !committedRevision) {
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
                result: response,
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
