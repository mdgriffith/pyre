import type { Client } from "@libsql/client";
import * as wasm from "./wasm/pyre_wasm.js";
import { normalizeForWasmJson } from "./wasm-json";
import { activateSchemaForDatabase } from "./schema";
import { requireDatabaseId, type DatabaseId } from "./database-id";
import { run, type QueryMap, type QueryResult, type Session, type SessionValue, type SyncDeltasFn } from "./query";

export const MAX_LIVE_SYNC_DELTA_ROWS = 5000;
export const MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES = 1024 * 1024;
export const MAX_LIVE_SYNC_FANOUT_RECIPIENTS = 1000;

/**
 * The host must authenticate connections and assign their server-selected
 * databaseId at registration. Unscoped or other-database connections never
 * receive a broadcast. Rows are permission-filtered; tombstone IDs are not.
 */
export async function runWithSync(
  db: Client,
  queryMap: QueryMap,
  queryId: string,
  args: unknown,
  executingSession: Session,
  connectedSessions?: Map<string, { session: Record<string, SessionValue>; databaseId: DatabaseId; [key: string]: any }>,
  databaseId: DatabaseId = "main",
  originSessionId?: string,
): Promise<QueryResult> {
  const id = requireDatabaseId(databaseId);
  const publish: SyncDeltasFn = async (rows, sessions, send, origin, committed) => {
    if (!committed) return {};
    activateSchemaForDatabase(id);
    const recipients = new Map(Array.from(sessions).filter(([, connection]) => connection.databaseId === id));
    if (origin) recipients.set(origin, { session: executingSession, databaseId: id });
    const deletes = rows.filter(group => group.headers?.[0] === "$delete");
    const upserts = rows.filter(group => group.headers?.[0] !== "$delete");
    const decode = (value: any) => {
      if (typeof value === "string" && value.startsWith("Error:")) throw new Error(value);
      return typeof value === "string" ? JSON.parse(value) : value;
    };
    const messages = new Map<string, any>();
    const wake = { type: "syncRequired", syncVersion: 2, databaseId: id, ...committed };
    try {
      const filtered = decode(wasm.calculate_sync_deltas(normalizeForWasmJson(upserts), new Map(Array.from(recipients, ([key, connection]) => [key, { ...connection, session: normalizeForWasmJson(connection.session) }]))));
      for (const group of filtered.groups ?? []) {
        const data = [...deletes, ...decode(wasm.reshape_sync_table_groups(normalizeForWasmJson(group.table_groups)))];
        for (const key of group.session_ids) messages.set(key, data);
      }
    } catch {
      for (const key of recipients.keys()) messages.set(key, null);
    }
    let originMessage: unknown;
    for (const key of recipients.keys()) {
      const data = messages.has(key) ? messages.get(key) : deletes;
      let message = data == null ? wake : { type: "delta", syncVersion: 2, databaseId: id, ...committed, data };
      if (data?.reduce((count: number, group: any) => count + group.rows.length, 0) > MAX_LIVE_SYNC_DELTA_ROWS
        || recipients.size > MAX_LIVE_SYNC_FANOUT_RECIPIENTS
        || new TextEncoder().encode(JSON.stringify(message)).length > MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES) message = wake;
      if (key === origin) originMessage = message;
      else send(key, message);
    }
    return { ...committed, originMessage };
  };
  const origin = originSessionId ?? crypto.randomUUID();
  // Keep the live registry by reference until publication. A pre-mutation copy
  // can omit a stream that registered before this transaction committed.
  return run(db, queryMap, queryId, args, executingSession, connectedSessions, publish, origin, { mode: "sync" });
}
