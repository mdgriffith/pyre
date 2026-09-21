import { Client } from "@libsql/client";
import * as wasm from "./wasm/pyre_wasm.js";
import { normalizeForWasmJson } from "./wasm-json";
import { requireDatabaseId, type DatabaseId } from "./database-id";
import { activateSchemaForDatabase } from "./schema";
import {
  run,
  type QueryMap,
  type OperationDescriptor,
  type QueryResult,
  type Session,
  type SessionValue,
  type SyncDeltasFn,
} from "./query";

export const MAX_LIVE_SYNC_DELTA_ROWS = 5000;
export const MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES = 1024 * 1024;
export const MAX_LIVE_SYNC_FANOUT_RECIPIENTS = 1000;

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

// Authorization runs against deleted preimages. Only then strip their contents
// and publish identity-only tombstones to sessions that could read those rows.
function removalDeltas(data: any[], affected: any[]): any[] {
  const removed = new Set(affected.filter(group => group.headers.includes("_pyre_removed")).flatMap(group =>
    group.rows.map((row: unknown[]) => JSON.stringify([group.table_name, row[group.headers.indexOf("id")]]))));
  return data.flatMap(group => {
    const idIndex = group.headers.indexOf("id");
    const deleted = group.rows.filter((row: unknown[]) => removed.has(JSON.stringify([group.table_name, row[idIndex]])));
    const rows = group.rows.filter((row: unknown[]) => !removed.has(JSON.stringify([group.table_name, row[idIndex]])));
    return [
      ...(rows.length ? [{ ...group, rows }] : []),
      ...(deleted.length ? [{ table_name: group.table_name, headers: ["id", "_pyre_removed"], rows: deleted.map((row: unknown[]) => [row[idIndex], true]) }] : []),
    ];
  });
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

function syncWithWasmForDatabase(databaseId?: DatabaseId): SyncDeltasFn {
  const normalizedDatabaseId = databaseId ? requireDatabaseId(databaseId) : undefined;

  return async (affectedRowGroups, connectedSessions, sendToSession, originSessionId, committedRevision) => {
    activateSchemaForDatabase(normalizedDatabaseId);

    const broadcastSessions = sessionsWithoutOrigin(connectedSessions, originSessionId);
    const originSession = singleOriginSession(connectedSessions, originSessionId);
    const normalizeSessions = (sessions: typeof broadcastSessions) => new Map(
      Array.from(sessions, ([id, data]) => [
        id,
        { ...data, session: normalizeForWasmJson(data.session) },
      ]),
    );
    if (!committedRevision) throw new Error("Missing committed sync revision");
    const { databaseEpoch, serverRevision } = committedRevision;
    // The affected-row format contains only post-update rows. If a session
    // cannot see all of them, it may have lost access to a cached row. Do not
    // disclose that row's identity: invalidate the cache and catch up instead.
    const invalidation = {
      type: "invalidate",
      serverRevision,
      databaseEpoch,
      ...(normalizedDatabaseId ? { databaseId: normalizedDatabaseId } : {}),
    };
    const affectedCount = countRows(affectedRowGroups.filter(group => !group.headers.includes("_pyre_removed")));
    const deltasResult = wasm.calculate_sync_deltas(
      affectedRowGroups,
      normalizeSessions(broadcastSessions),
    );

    if (typeof deltasResult === "string" && deltasResult.startsWith("Error:")) {
      console.error("[SyncDeltas] Failed to calculate sync deltas:", deltasResult);
      const message = invalidation;
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

    const delivered = new Set<string>();

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
        data: removalDeltas(data, affectedRowGroups),
      };

      const message = countRows(removalDeltas(group.table_groups, affectedRowGroups).filter(group => !group.headers.includes("_pyre_removed"))) < affectedCount
        ? invalidation
        : liveSyncRequiresCatchup(deltaMessage, countRows(data), group.session_ids.length)
        ? {
          type: "syncRequired",
          serverRevision,
          databaseEpoch,
          ...(normalizedDatabaseId ? { databaseId: normalizedDatabaseId } : {}),
        }
        : deltaMessage;

      for (const sessionId of group.session_ids) {
        sendToSession(sessionId, message);
        delivered.add(sessionId);
      }
    }

    for (const sessionId of broadcastSessions.keys()) {
      if (!delivered.has(sessionId)) sendToSession(sessionId, affectedCount ? invalidation : { ...invalidation, type: "delta", data: [] });
    }

    let originMessage: unknown = originSession ? (affectedCount ? invalidation : { ...invalidation, type: "delta", data: [] }) : undefined;
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
              data: removalDeltas(data, affectedRowGroups),
            };
            originMessage = countRows(removalDeltas(originGroup.table_groups, affectedRowGroups).filter(group => !group.headers.includes("_pyre_removed"))) < affectedCount
              ? invalidation
              : liveSyncRequiresCatchup(deltaMessage, countRows(data), originGroup.session_ids.length)
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
  queryId: string | readonly OperationDescriptor[],
  args: any,
  executingSession: Session,
  connectedSessions?: Map<string, { session: Record<string, SessionValue>; [key: string]: any }>,
  databaseId?: DatabaseId,
  originSessionId?: string,
): Promise<QueryResult> {
  const syncSessions = connectedSessions ? new Map(connectedSessions) : new Map();
  const responseSessionId = originSessionId ?? `__pyre_response_${crypto.randomUUID()}`;
  syncSessions.set(responseSessionId, { session: executingSession as Record<string, SessionValue> });

  return run(db, queryMap, queryId, args, executingSession, syncSessions, syncWithWasmForDatabase(databaseId), responseSessionId, { mode: "sync", allocateSyncRevision: true });
}
