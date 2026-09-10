import { Client } from "@libsql/client";
import { z } from "zod";
import { readReplacementTables } from "./sync";
import * as wasm from "./wasm/pyre_wasm.js";
import { normalizeForWasmJson } from "./wasm-json";
import { requireDatabaseId, type DatabaseId } from "./database-id";
import { activateSchemaForDatabase, captureReplacementSchema } from "./schema";
import {
  run,
  runBatch,
  nextLiveSyncRevision,
  type BatchAuthority,
  type BatchManifest,
  type BatchRequest,
  type BatchResult,
  type QueryMap,
  type QueryResult,
  type Session,
  type SessionValue,
  type SyncDeltasFn,
} from "./query";

export const MAX_LIVE_SYNC_DELTA_ROWS = 5000;
export const MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES = 1024 * 1024;
export const MAX_LIVE_SYNC_FANOUT_RECIPIENTS = 1000;

const fenceValidator = z.strictObject({
  databaseId: z.string().min(1), instance: z.string().min(1),
  authGeneration: z.number().int().nonnegative(), namespace: z.string().min(1),
  manifest: z.string().min(1), databaseEpoch: z.string().min(1),
});

export type SyncFence = z.infer<typeof fenceValidator>;
/** Trusted current registration, established by database authorization and authentication. */
export interface BatchSyncRecipient { session: Session; fence: SyncFence }
export interface ReplacementRequest extends SyncFence { version: 1; requestId: string; target: number }
export interface ReplacementResponse extends SyncFence {
  type: "replacement";
  requestId: string;
  target: number;
  scope: "database";
  complete: true;
  serverRevision: number;
  /** Replace the entire database scope, not just these table keys. Empty means clear. */
  tables: Record<string, { rows: unknown[] }>;
}
export type ReplacementResult = { kind: "success"; response: ReplacementResponse }
  | { kind: "error"; error: { errorType: "InvalidRequest" | "InvalidSession" | "ReplacementUnavailable"; message: string } };

const replacementRequestValidator = fenceValidator.extend({
  version: z.literal(1), requestId: z.string().min(1), target: z.number().int().nonnegative(),
});

/** Read-only, route-independent catchup. Never expose timestamp pages as replacement evidence. */
export async function catchupReplacement(
  db: Client, manifest: BatchManifest, authority: BatchAuthority,
  request: ReplacementRequest, executingSession: Session,
): Promise<ReplacementResult> {
  const failure = (errorType: Extract<ReplacementResult, { kind: "error" }>["error"]["errorType"]): ReplacementResult =>
    ({ kind: "error", error: { errorType, message: errorType } });
  let captured: ReplacementRequest;
  let session: Session;
  let restoreSchema: () => void;
  try {
    captured = replacementRequestValidator.parse(structuredClone(request));
    if (manifest.version !== 1 || manifest.manifestVersion !== authority.manifest
      || !["databaseId", "instance", "authGeneration", "namespace", "manifest"].every(
        key => captured[key as keyof BatchAuthority] === authority[key as keyof BatchAuthority])) return failure("InvalidRequest");
    const decoded = manifest.SessionValidator.safeParse(structuredClone(executingSession));
    if (!decoded.success) return failure("InvalidSession");
    session = decoded.data;
    restoreSchema = captureReplacementSchema(captured.databaseId, manifest.compiledContract!);
  } catch { return failure("InvalidRequest"); }

  let tx: Awaited<ReturnType<Client["transaction"]>> | undefined;
  try {
    // The file adapter detaches transaction connections, losing in-memory databases.
    if (db.protocol === "file") {
      const databases = await db.execute("pragma database_list");
      if (!databases.rows.find(row => row.name === "main")?.file) return failure("ReplacementUnavailable");
    }
    tx = await db.transaction("read");
    const databases = await tx.execute("pragma database_list");
    if (databases.rows.some(row => row.name !== "main" && row.name !== "temp")) return failure("InvalidRequest");
    const state = (await tx.execute("select database_epoch, server_revision from _pyre_sync where id = 1")).rows[0];
    if (state?.database_epoch !== captured.databaseEpoch) return failure("InvalidRequest");
    const revision = Number(state.server_revision);
    if ((typeof state.server_revision !== "number" && typeof state.server_revision !== "bigint")
      || !Number.isSafeInteger(revision) || revision < captured.target) return failure("ReplacementUnavailable");
    const tables = await readReplacementTables(tx, session, captured.namespace, restoreSchema);
    // Bound wire materialization without ever turning truncation into completeness.
    if (new TextEncoder().encode(JSON.stringify(tables)).byteLength > 64 * 1024 * 1024)
      return failure("ReplacementUnavailable");
    await tx.commit();
    const { version: _version, ...correlation } = captured;
    return { kind: "success", response: { ...correlation, type: "replacement", scope: "database", complete: true,
      serverRevision: revision, tables } };
  } catch { return failure("ReplacementUnavailable"); }
  finally {
    try { if (tx && !tx.closed) await tx.rollback(); } catch { /* No partial snapshot is published. */ }
    try { tx?.close(); } catch { /* Read transaction only. */ }
  }
}

/** Batch acceptance never depends on origin registration or successful SSE delivery. */
export function runBatchWithSync(
  db: Client,
  manifest: BatchManifest,
  authority: BatchAuthority,
  request: BatchRequest,
  executingSession: Session,
  connectedSessions: Map<string, BatchSyncRecipient> = new Map(),
  sendToSession: (sessionId: string, message: unknown) => void | Promise<void> = () => {},
): Promise<BatchResult> {
  return runBatch(db, manifest, authority, request, executingSession, result => {
    const response = result.response;
    // Resolve current registrations after commit, never reuse an origin/client-supplied fence.
    for (const [sessionId, recipient] of connectedSessions) {
      const parsed = fenceValidator.safeParse(recipient.fence);
      if (!parsed.success) continue;
      const fence = parsed.data;
      if (fence.databaseId !== response.databaseId || fence.databaseEpoch !== response.databaseEpoch
        || fence.namespace !== response.namespace || fence.manifest !== response.manifest) continue;
      try { void Promise.resolve(sendToSession(sessionId, { type: "syncRequired", ...fence,
        serverRevision: response.commitRevision, reconciliation: structuredClone(response.reconciliation) }))
        .catch(() => { /* Async delivery cannot delay or reject the origin response. */ });
      } catch { /* A failed recipient must not suppress the others. */ }
    }
  });
}

function countRows(tableGroups: unknown): number {
  if (!Array.isArray(tableGroups)) {
    return 0;
  }

  return tableGroups.reduce((total, tableGroup) => {
    if (typeof tableGroup !== "object" || tableGroup == null || !("rows" in tableGroup)) {
      return total;
    }

    return total + (Array.isArray(tableGroup.rows) ? tableGroup.rows.length : 0);
  }, 0);
}

function liveSyncRequiresCatchup(message: unknown, rowCount: number, recipientCount: number): boolean {
  if (rowCount > MAX_LIVE_SYNC_DELTA_ROWS) {
    return true;
  }

  if (recipientCount > MAX_LIVE_SYNC_FANOUT_RECIPIENTS) {
    return true;
  }

  return new TextEncoder().encode(JSON.stringify(message)).byteLength > MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES;
}

function sessionsWithoutOrigin(
  connectedSessions: Map<string, { session: Record<string, SessionValue>; [key: string]: any }>,
  originSessionId?: string,
): Map<string, { session: Record<string, SessionValue>; [key: string]: any }> {
  if (!originSessionId || !connectedSessions.has(originSessionId)) {
    return connectedSessions;
  }

  const recipients = new Map(connectedSessions);
  recipients.delete(originSessionId);
  return recipients;
}

function singleOriginSession(
  connectedSessions: Map<string, { session: Record<string, SessionValue>; [key: string]: any }>,
  originSessionId?: string,
): Map<string, { session: Record<string, SessionValue>; [key: string]: any }> | undefined {
  if (!originSessionId) {
    return undefined;
  }

  const origin = connectedSessions.get(originSessionId);
  return origin ? new Map([[originSessionId, origin]]) : undefined;
}

function syncWithWasmForDatabase(db: Client, databaseId?: DatabaseId): SyncDeltasFn {
  const normalizedDatabaseId = databaseId ? requireDatabaseId(databaseId) : undefined;

  return async (affectedRowGroups, connectedSessions, sendToSession, originSessionId, committedRevision) => {
    const revision = committedRevision ?? await nextLiveSyncRevision(db);
    const { databaseEpoch, serverRevision } = revision;
    // Replacement registrations never receive legacy deltas, including an origin registration.
    const legacySessions = new Map(connectedSessions);
    for (const [id, recipient] of connectedSessions) {
      if (!Object.hasOwn(recipient, "fence")) continue;
      legacySessions.delete(id);
      const parsed = fenceValidator.safeParse(recipient.fence);
      if (!parsed.success || parsed.data.databaseId !== normalizedDatabaseId || parsed.data.databaseEpoch !== databaseEpoch) continue;
      try { void Promise.resolve(sendToSession(id, { type: "syncRequired", ...parsed.data, serverRevision,
        reconciliation: { kind: "replaceRequired", atLeast: serverRevision, invalidate: true, minimumSafeRevision: serverRevision } }))
        .catch(() => {}); } catch { /* Independent recipient delivery. */ }
    }
    if (legacySessions.size === 0) return revision;
    activateSchemaForDatabase(normalizedDatabaseId);

    const broadcastSessions = sessionsWithoutOrigin(legacySessions, originSessionId);
    const originSession = singleOriginSession(legacySessions, originSessionId);
    const normalizeSessions = (sessions: typeof broadcastSessions) => new Map(
      Array.from(sessions, ([id, data]) => [
        id,
        { ...data, session: normalizeForWasmJson(data.session) },
      ]),
    );
    const deltasResult = wasm.calculate_sync_deltas(
      affectedRowGroups,
      normalizeSessions(broadcastSessions),
    );

    if (typeof deltasResult === "string" && deltasResult.startsWith("Error:")) {
      console.error("[SyncDeltas] Failed to calculate sync deltas:", deltasResult);
      const message = {
        type: "syncRequired",
        serverRevision,
        databaseEpoch,
        ...(normalizedDatabaseId ? { databaseId: normalizedDatabaseId } : {}),
      };
      for (const sessionId of broadcastSessions.keys()) {
        sendToSession(sessionId, message);
      }
      return {
        databaseEpoch,
        serverRevision,
        ...(originSession ? { originMessage: message } : {}),
      };
    }

    const result = typeof deltasResult === "string" ? JSON.parse(deltasResult) : deltasResult;

    if ((!Array.isArray(result.groups) || result.groups.length === 0) && !originSession) {
      return { databaseEpoch, serverRevision };
    }

    for (const group of Array.isArray(result.groups) ? result.groups : []) {
      const reshapedTableGroupsResult = wasm.reshape_sync_table_groups(normalizeForWasmJson(group.table_groups));

      if (typeof reshapedTableGroupsResult === "string" && reshapedTableGroupsResult.startsWith("Error:")) {
        console.error("[SyncDeltas] Failed to reshape sync deltas:", reshapedTableGroupsResult);
        continue;
      }

      const data = typeof reshapedTableGroupsResult === "string"
        ? JSON.parse(reshapedTableGroupsResult)
        : reshapedTableGroupsResult;

      const deltaMessage = {
        type: "delta",
        serverRevision,
        databaseEpoch,
        ...(normalizedDatabaseId ? { databaseId: normalizedDatabaseId } : {}),
        data,
      };

      const message = liveSyncRequiresCatchup(deltaMessage, countRows(data), group.session_ids.length)
        ? {
          type: "syncRequired",
          serverRevision,
          databaseEpoch,
          ...(normalizedDatabaseId ? { databaseId: normalizedDatabaseId } : {}),
        }
        : deltaMessage;

      for (const sessionId of group.session_ids) {
        sendToSession(sessionId, message);
      }
    }

    let originMessage: unknown;
    if (originSession) {
      const originDeltasResult = wasm.calculate_sync_deltas(
        affectedRowGroups,
        normalizeSessions(originSession),
      );

      if (typeof originDeltasResult === "string" && originDeltasResult.startsWith("Error:")) {
        console.error("[SyncDeltas] Failed to calculate origin sync delta:", originDeltasResult);
      } else {
        const originResult = typeof originDeltasResult === "string" ? JSON.parse(originDeltasResult) : originDeltasResult;
        const originGroup = Array.isArray(originResult.groups) ? originResult.groups[0] : undefined;

        if (originGroup) {
          const reshapedTableGroupsResult = wasm.reshape_sync_table_groups(normalizeForWasmJson(originGroup.table_groups));

          if (typeof reshapedTableGroupsResult === "string" && reshapedTableGroupsResult.startsWith("Error:")) {
            console.error("[SyncDeltas] Failed to reshape origin sync delta:", reshapedTableGroupsResult);
          } else {
            const data = typeof reshapedTableGroupsResult === "string"
              ? JSON.parse(reshapedTableGroupsResult)
              : reshapedTableGroupsResult;
            const deltaMessage = {
              type: "delta",
              serverRevision,
              databaseEpoch,
              ...(normalizedDatabaseId ? { databaseId: normalizedDatabaseId } : {}),
              data,
            };
            originMessage = liveSyncRequiresCatchup(deltaMessage, countRows(data), originGroup.session_ids.length)
              ? {
                type: "syncRequired",
                serverRevision,
                databaseEpoch,
                ...(normalizedDatabaseId ? { databaseId: normalizedDatabaseId } : {}),
              }
              : deltaMessage;
          }
        }
      }
    }

    return { databaseEpoch, serverRevision, ...(originMessage === undefined ? {} : { originMessage }) };
  };
}

export async function runWithSync(
  db: Client,
  queryMap: QueryMap,
  queryId: string,
  args: any,
  executingSession: Session,
  connectedSessions?: Map<string, { session: Record<string, SessionValue>; [key: string]: any }>,
  databaseId?: DatabaseId,
  originSessionId?: string,
): Promise<QueryResult> {
  const originSession = structuredClone(executingSession);
  const publish = syncWithWasmForDatabase(db, databaseId);
  const sync: SyncDeltasFn = (rows, sessions, send, origin, revision) => {
    const current = new Map(sessions);
    if (origin && !current.has(origin)) current.set(origin, { session: originSession });
    return publish(rows, current, send, origin, revision);
  };
  return run(db, queryMap, queryId, args, executingSession, connectedSessions, sync, originSessionId, {
    mode: "sync", commitSyncRevision: ["insert", "update", "delete", "transaction"].includes(queryMap[queryId]?.operation ?? ""),
  });
}
