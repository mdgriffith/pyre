import { Client } from "@libsql/client";
import { z } from "zod";
import { MAX_REPLACEMENT_PAYLOAD_BYTES, readReplacementTables } from "./sync";
import * as wasm from "./wasm/pyre_wasm.js";
import { normalizeForWasmJson } from "./wasm-json";
import { requireDatabaseId, type DatabaseId } from "./database-id";
import { assertPersistentTransaction, assertSupportedIntegerMode, internalSafeInteger } from "./runtime/libsql";
import { assertSchemaContracts, captureReplacementSchema } from "./schema";
import {
  run,
  runBatch,
  nextLiveSyncRevision,
  executionSchemaContract,
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
export const LIVE_SYNC_DELIVERY_TIMEOUT_MS = 5000;

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
  let schema: ReturnType<typeof captureReplacementSchema>;
  let contracts: Readonly<Record<string, string>>;
  try {
    captured = replacementRequestValidator.parse(structuredClone(request));
    if (manifest.version !== 1 || manifest.manifestVersion !== authority.manifest
      || !["databaseId", "instance", "authGeneration", "namespace", "manifest"].every(
        key => captured[key as keyof BatchAuthority] === authority[key as keyof BatchAuthority])) return failure("InvalidRequest");
    const decoded = manifest.SessionValidator.safeParse(structuredClone(executingSession));
    if (!decoded.success) return failure("InvalidSession");
    session = decoded.data;
    contracts = { [captured.namespace]: manifest.replacementContracts?.[captured.namespace]! };
    schema = captureReplacementSchema(captured.databaseId, contracts[captured.namespace]);
  } catch { return failure("InvalidRequest"); }

  let tx: Awaited<ReturnType<Client["transaction"]>> | undefined;
  try {
    await assertPersistentTransaction(db);
    await assertSupportedIntegerMode(db);
    tx = await db.transaction("read");
    const databases = await tx.execute("pragma database_list");
    if (databases.rows.some(row => row.name !== "main" && row.name !== "temp")) return failure("InvalidRequest");
    const state = (await tx.execute("select database_epoch, server_revision from _pyre_sync where id = 1")).rows[0];
    if (state?.database_epoch !== captured.databaseEpoch) return failure("InvalidRequest");
    const revision = internalSafeInteger(state.server_revision, "Pyre sync server revision");
    if (revision < captured.target) return failure("ReplacementUnavailable");
    // Permission evidence and rows must belong to the same snapshot, not just
    // to the same cache entry before asynchronous transaction acquisition.
    try { await assertSchemaContracts(tx, contracts, captured.namespace); }
    catch { return failure("InvalidRequest"); }
    const tables = await readReplacementTables(tx, session, captured.namespace, schema.restore);
    // Bound wire materialization without ever turning truncation into completeness.
    if (new TextEncoder().encode(JSON.stringify(tables)).byteLength > MAX_REPLACEMENT_PAYLOAD_BYTES)
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
  /** In-process binding only; network requests must retain their epoch fence. */
  captureDatabaseEpoch = false,
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
      void sendBestEffort(sendToSession, sessionId, recipient, { type: "syncRequired", ...fence,
        serverRevision: response.commitRevision, reconciliation: structuredClone(response.reconciliation) },
        response.databaseId, () => connectedSessions.get(sessionId) === recipient);
    }
  }, captureDatabaseEpoch);
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

interface Publication {
  send(sessionId: string, message: any): void | Promise<void>;
  isCurrent(): boolean;
  sessionId: string;
  message: any;
}
interface DeliveryState { inFlight: boolean; next?: Publication }
const recipientDeliveries = new WeakMap<object, Map<DatabaseId | Client, DeliveryState>>();

// Drop row payloads when delivery is uncertain. A single coalesced invalidation
// repairs all skipped revisions, without letting newer rows overtake an old send.
function queueReplacement(state: DeliveryState, publication: Publication): void {
  if (state.next && state.next.message.databaseEpoch === publication.message.databaseEpoch
    && state.next.message.serverRevision > publication.message.serverRevision) return;
  const { type: _type, data: _data, ...metadata } = publication.message;
  const revision = metadata.serverRevision;
  state.next = { ...publication, message: { ...metadata, type: "syncRequired",
    reconciliation: { kind: "replaceRequired", atLeast: revision, invalidate: true, minimumSafeRevision: revision } } };
}

async function sendBestEffort(
  sendToSession: (sessionId: string, message: any) => void | Promise<void>,
  sessionId: string,
  recipient: object,
  message: unknown,
  database: DatabaseId | Client,
  isCurrent: () => boolean,
): Promise<void> {
  if (!isCurrent()) return;
  const publication = { send: sendToSession, sessionId, message, isCurrent };
  const deliveries = recipientDeliveries.get(recipient) ?? new Map<DatabaseId | Client, DeliveryState>();
  recipientDeliveries.set(recipient, deliveries);
  let state = deliveries.get(database);
  if (state) {
    queueReplacement(state, publication);
    if (state.inFlight) return;
  } else {
    state = { inFlight: false };
    deliveries.set(database, state);
  }
  const delivery = state;
  let current: Publication | undefined = delivery.next ?? publication;
  delivery.next = undefined;
  delivery.inFlight = true;
  const completed = (async () => {
    try {
      while (current) {
        if (!current.isCurrent()) { delivery.next = undefined; return; }
        try { await current.send(current.sessionId, current.message); }
        catch {
          // Retry only a replacement hint on a future publication, never a write
          // or a row delta whose delivery outcome is unknown.
          queueReplacement(delivery, current);
          return;
        }
        current = delivery.next;
        delivery.next = undefined;
      }
    } finally {
      delivery.inFlight = false;
      if (!delivery.next) {
        deliveries.delete(database);
        if (!deliveries.size) recipientDeliveries.delete(recipient);
      }
    }
  })();
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      completed,
      new Promise<void>(resolve => {
        timer = setTimeout(() => {
          if (current) queueReplacement(delivery, current);
          resolve();
        }, LIVE_SYNC_DELIVERY_TIMEOUT_MS);
      }),
    ]);
  }
  catch { /* Independent recipient delivery. */ }
  finally { clearTimeout(timer); }
}

// Database IDs coordinate pooled handles in this process; legacy callers without
// an ID retain per-handle ordering. Settled queues are removed below.
const namedMutationQueues = new Map<DatabaseId | Client, Promise<void>>();

function syncWithWasmForDatabase(
  db: Client,
  databaseId?: DatabaseId,
  namespace?: string,
  currentManifest?: string,
  registrations?: Map<string, object>,
): SyncDeltasFn {
  const normalizedDatabaseId = databaseId ? requireDatabaseId(databaseId) : undefined;

  return async (affectedRowGroups, connectedSessions, sendToSession, originSessionId, committedRevision) => {
    const revision = committedRevision ?? await nextLiveSyncRevision(db);
    const { databaseEpoch, serverRevision } = revision;
    const sends: Promise<void>[] = [];
    const queueSend = (sessionId: string, message: unknown) => {
      const recipient = connectedSessions.get(sessionId);
      if (recipient) sends.push(sendBestEffort(sendToSession, sessionId, recipient, message,
        normalizedDatabaseId ?? db, () => !registrations || registrations.get(sessionId) === recipient));
    };
    // Replacement registrations never receive legacy deltas, including an origin registration.
    const legacySessions = new Map(connectedSessions);
    for (const [id, recipient] of connectedSessions) {
      if (!Object.hasOwn(recipient, "fence")) continue;
      legacySessions.delete(id);
      const parsed = fenceValidator.safeParse(recipient.fence);
      if (!parsed.success || parsed.data.databaseId !== normalizedDatabaseId || parsed.data.databaseEpoch !== databaseEpoch
        || parsed.data.namespace !== namespace || parsed.data.manifest !== currentManifest) continue;
      queueSend(id, { type: "syncRequired", ...parsed.data, serverRevision,
        reconciliation: { kind: "replaceRequired", atLeast: serverRevision, invalidate: true, minimumSafeRevision: serverRevision } });
    }
    if (legacySessions.size === 0) {
      await Promise.all(sends);
      return revision;
    }
    const broadcastSessions = sessionsWithoutOrigin(legacySessions, originSessionId);
    const originSession = singleOriginSession(legacySessions, originSessionId);
    const syncRequiredMessage = {
      type: "syncRequired",
      serverRevision,
      databaseEpoch,
      ...(normalizedDatabaseId ? { databaseId: normalizedDatabaseId } : {}),
    };
    const requireSync = (sessionIds: Iterable<string>) => {
      for (const sessionId of sessionIds) queueSend(sessionId, syncRequiredMessage);
    };
    const normalizeSessions = (sessions: typeof broadcastSessions) => new Map(
      Array.from(sessions, ([id, data]) => [
        id,
        { ...data, session: normalizeForWasmJson(data.session) },
      ]),
    );
    let result: any;
    let schema: ReturnType<typeof captureReplacementSchema>;
    try {
      const contract = committedRevision && executionSchemaContract(committedRevision);
      if (!contract) throw new Error("Missing execution schema authority");
      schema = captureReplacementSchema(normalizedDatabaseId, contract);
      schema.restore();
      const deltasResult = wasm.calculate_sync_deltas(
        affectedRowGroups,
        normalizeSessions(broadcastSessions),
      );
      if (typeof deltasResult === "string" && deltasResult.startsWith("Error:")) throw new Error(deltasResult);
      result = typeof deltasResult === "string" ? JSON.parse(deltasResult) : deltasResult;
    } catch (error) {
      console.error("[SyncDeltas] Failed to calculate sync deltas:", error);
      requireSync(broadcastSessions.keys());
      await Promise.all(sends);
      return {
        databaseEpoch,
        serverRevision,
        ...(originSession ? { originMessage: syncRequiredMessage } : {}),
      };
    }

    if ((!Array.isArray(result.groups) || result.groups.length === 0) && !originSession) {
      await Promise.all(sends);
      return { databaseEpoch, serverRevision };
    }

    for (const group of Array.isArray(result.groups) ? result.groups : []) {
      let data: any;
      try {
        schema.restore();
        const reshapedTableGroupsResult = wasm.reshape_sync_table_groups(normalizeForWasmJson(group.table_groups));
        if (typeof reshapedTableGroupsResult === "string" && reshapedTableGroupsResult.startsWith("Error:")) {
          throw new Error(reshapedTableGroupsResult);
        }
        data = typeof reshapedTableGroupsResult === "string"
          ? JSON.parse(reshapedTableGroupsResult)
          : reshapedTableGroupsResult;
      } catch (error) {
        console.error("[SyncDeltas] Failed to reshape sync deltas:", error);
        requireSync(group.session_ids);
        continue;
      }

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
        queueSend(sessionId, message);
      }
    }

    let originMessage: unknown;
    if (originSession) {
      try {
        schema.restore();
        const originDeltasResult = wasm.calculate_sync_deltas(
          affectedRowGroups,
          normalizeSessions(originSession),
        );
        if (typeof originDeltasResult === "string" && originDeltasResult.startsWith("Error:")) {
          throw new Error(originDeltasResult);
        }
        const originResult = typeof originDeltasResult === "string" ? JSON.parse(originDeltasResult) : originDeltasResult;
        const originGroup = Array.isArray(originResult.groups) ? originResult.groups[0] : undefined;

        if (originGroup) {
          try {
            const reshapedTableGroupsResult = wasm.reshape_sync_table_groups(normalizeForWasmJson(originGroup.table_groups));
            if (typeof reshapedTableGroupsResult === "string" && reshapedTableGroupsResult.startsWith("Error:")) {
              throw new Error(reshapedTableGroupsResult);
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
            originMessage = liveSyncRequiresCatchup(deltaMessage, countRows(data), originGroup.session_ids.length)
              ? {
                type: "syncRequired",
                serverRevision,
                databaseEpoch,
                ...(normalizedDatabaseId ? { databaseId: normalizedDatabaseId } : {}),
              }
              : deltaMessage;
          } catch (error) {
            console.error("[SyncDeltas] Failed to reshape origin sync delta:", error);
            originMessage = syncRequiredMessage;
          }
        }
      } catch (error) {
        console.error("[SyncDeltas] Failed to calculate origin sync delta:", error);
        originMessage = syncRequiredMessage;
      }
    }

    await Promise.all(sends);
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
  sendToSession?: (sessionId: string, message: any) => void | Promise<void>,
  /** Trusted manifest fingerprint for fenced recipients. Omit to suppress fenced hints. */
  currentManifest?: string,
): Promise<QueryResult> {
  const capturedArgs = structuredClone(args);
  const capturedSession = structuredClone(executingSession);
  const originSession = capturedSession;
  const publish = syncWithWasmForDatabase(db, databaseId, queryMap[queryId]?.primary_db, currentManifest, connectedSessions);
  const sync: SyncDeltasFn = (rows, sessions, send, origin, revision) => {
    const current = new Map(sessions);
    if (origin && !current.has(origin)) current.set(origin, { session: originSession });
    return publish(rows, current, send, origin, revision);
  };
  const namedMutation = ["insert", "update", "delete", "transaction"].includes(queryMap[queryId]?.operation ?? "");
  if (namedMutation && !sendToSession) {
    throw new Error("runWithSync requires sendToSession for named mutations");
  }
  const execute = async () => {
    const result = await run(db, queryMap, queryId, capturedArgs, capturedSession, connectedSessions, sync, originSessionId, {
      mode: "sync", commitSyncRevision: namedMutation,
    });
    if (!namedMutation || result.kind !== "success") return result;

    let published: Awaited<ReturnType<QueryResult["sync"]>> = {};
    try { published = await result.sync(sendToSession!); }
    catch { /* Publication cannot change the committed mutation outcome. */ }
    result.sync = async () => published;
    return result;
  };

  if (!namedMutation) return execute();
  const queueKey = databaseId ? requireDatabaseId(databaseId) : db;
  const previous = namedMutationQueues.get(queueKey) ?? Promise.resolve();
  const execution = previous.then(execute);
  const settled = execution.then(() => undefined, () => undefined);
  namedMutationQueues.set(queueKey, settled);
  void settled.then(() => { if (namedMutationQueues.get(queueKey) === settled) namedMutationQueues.delete(queueKey); });
  return execution;
}
