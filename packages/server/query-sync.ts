import { Client } from "@libsql/client";
import * as wasm from "./wasm/pyre_wasm.js";
import { normalizeForWasmJson } from "./wasm-json";
import { requireDatabaseId, type DatabaseId } from "./database-id";
import { activateSchemaForDatabase } from "./schema";
import {
  run,
  type QueryMap,
  type QueryResult,
  type Session,
  type SessionValue,
  type SyncDeltasFn,
} from "./query";

export const MAX_LIVE_SYNC_DELTA_ROWS = 5000;
export const MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES = 1024 * 1024;
export const MAX_LIVE_SYNC_FANOUT_RECIPIENTS = 1000;

async function nextLiveSyncRevision(db: Client): Promise<{ databaseEpoch: string; serverRevision: number }> {
  const result = await db.execute("update _pyre_sync set server_revision = server_revision + 1 where id = 1 returning database_epoch, server_revision");
  const databaseEpoch = result.rows[0]?.database_epoch;
  const value = result.rows[0]?.server_revision;

  if (typeof databaseEpoch !== "string" || (typeof value !== "number" && typeof value !== "bigint")) {
    throw new Error("Failed to allocate Pyre sync server revision");
  }

  return { databaseEpoch, serverRevision: Number(value) };
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

  return async (affectedRowGroups, connectedSessions, sendToSession, originSessionId) => {
    activateSchemaForDatabase(normalizedDatabaseId);

    const broadcastSessions = sessionsWithoutOrigin(connectedSessions, originSessionId);
    const originSession = singleOriginSession(connectedSessions, originSessionId);
    const normalizeSessions = (sessions: typeof broadcastSessions) => new Map(
      Array.from(sessions, ([id, data]) => [
        id,
        { ...data, session: normalizeForWasmJson(data.session) },
      ]),
    );
    const { databaseEpoch, serverRevision } = await nextLiveSyncRevision(db);
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
  const syncSessions = connectedSessions ? new Map(connectedSessions) : new Map();
  if (originSessionId && !syncSessions.has(originSessionId)) {
    syncSessions.set(originSessionId, { session: executingSession as Record<string, SessionValue> });
  }

  return run(db, queryMap, queryId, args, executingSession, syncSessions, syncWithWasmForDatabase(db, databaseId), originSessionId, { mode: "sync" });
}
