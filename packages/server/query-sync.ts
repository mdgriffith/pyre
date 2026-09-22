import { Client } from "@libsql/client";
import * as wasm from "./wasm/pyre_wasm.js";
import { normalizeForWasmJson } from "./wasm-json";
import { requireDatabaseId, type DatabaseId } from "./database-id";
import { activateSchemaForDatabase } from "./schema";
import { run, type QueryMap, type OperationDescriptor, type QueryResult, type Session, type SessionValue, type SyncDeltasFn } from "./query";

export const MAX_LIVE_SYNC_DELTA_ROWS = 5000;
export const MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES = 1024 * 1024;
export const MAX_LIVE_SYNC_FANOUT_RECIPIENTS = 1000;

function countRows(groups: any[]): number {
  return groups.reduce((total, group) => total + group.rows.length, 0);
}

function parseWasm(value: any): any {
  if (typeof value === "string" && value.startsWith("Error:")) throw new Error(value);
  return typeof value === "string" ? JSON.parse(value) : value;
}

function rowKey(group: any, row: any[], primaryKeys: Map<string, string>): string {
  return JSON.stringify([group.table_name, row[group.headers.indexOf(primaryKeys.get(group.table_name) ?? "id")]]);
}

// Authorize original committed preimages separately from final row values.
// Intermediate permission values must never grant even identity-only delivery.
function visibleBySession(groups: any[], sessions: Map<string, any>): Map<string, any[]> {
  if (!groups.length) return new Map();
  const result = parseWasm(wasm.calculate_sync_deltas(groups, sessions));
  const visible = new Map<string, any[]>();
  for (const group of result.groups ?? []) {
    const data = parseWasm(wasm.reshape_sync_table_groups(normalizeForWasmJson(group.table_groups)));
    for (const id of group.session_ids) visible.set(id, [...(visible.get(id) ?? []), ...data]);
  }
  return visible;
}

function syncWithWasmForDatabase(databaseId?: DatabaseId): SyncDeltasFn {
  const normalizedDatabaseId = databaseId ? requireDatabaseId(databaseId) : undefined;
  return async (affected, connectedSessions, sendToSession, originSessionId, committedRevision) => {
    activateSchemaForDatabase(normalizedDatabaseId);
    if (!committedRevision) throw new Error("Missing committed sync revision");
    const stamp = { ...committedRevision, ...(normalizedDatabaseId ? { databaseId: normalizedDatabaseId } : {}) };
    const sessions = new Map(Array.from(connectedSessions, ([id, data]) => [id, { ...data, session: normalizeForWasmJson(data.session) }]));
    const primaryKeys = new Map<string, string>(affected.map(group => [group.table_name, group.primary_key ?? "id"]));
    let before: Map<string, any[]>;
    let after: Map<string, any[]>;
    try {
      before = visibleBySession(affected.filter(group => group.headers.includes("_pyre_preimage")), sessions);
      after = visibleBySession(affected.filter(group => !group.headers.includes("_pyre_preimage") && !group.headers.includes("_pyre_removed")), sessions);
    } catch (error) {
      console.error("[SyncDeltas] Failed to calculate sync deltas:", error);
      const message = { type: "invalidate", ...stamp };
      for (const id of sessions.keys()) if (id !== originSessionId) sendToSession(id, message);
      return { ...committedRevision, ...(sessions.has(originSessionId!) ? { originMessage: message } : {}) };
    }
    let originMessage: unknown;
    for (const id of sessions.keys()) {
      const rows = after.get(id) ?? [];
      const finalKeys = new Set(rows.flatMap(group => group.rows.map((row: any[]) => rowKey(group, row, primaryKeys))));
      const removals = (before.get(id) ?? []).flatMap(group => {
        const key = primaryKeys.get(group.table_name) ?? "id";
        const removed = group.rows.filter((row: any[]) => !finalKeys.has(rowKey(group, row, primaryKeys)));
        return removed.length ? [{ table_name: group.table_name, headers: [key, "_pyre_removed"], rows: removed.map((row: any[]) => [row[group.headers.indexOf(key)], true]) }] : [];
      });
      const data = [...rows, ...removals];
      const delta = { type: "delta", ...stamp, data };
      const message = countRows(data) > MAX_LIVE_SYNC_DELTA_ROWS
        || sessions.size > MAX_LIVE_SYNC_FANOUT_RECIPIENTS
        || new TextEncoder().encode(JSON.stringify(delta)).byteLength > MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES
        ? { type: "syncRequired", ...stamp } : delta;
      if (id === originSessionId) originMessage = message;
      else sendToSession(id, message);
    }
    return { ...committedRevision, ...(originMessage === undefined ? {} : { originMessage }) };
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
