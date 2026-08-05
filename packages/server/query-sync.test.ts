// @ts-nocheck
import { beforeEach, expect, mock, test } from "bun:test";

let introspectionResult = { schema_source: "test schema" };
let sessionIds = ["s1"];
let reshapedRows = [[1, "World", { _type: "Tiling", tileRootKey: "tiles/root", tileWidth: 256, format: { _type: "Png" } }]];
let deltaError: string | undefined;

mock.module("./wasm/pyre_wasm.js", () => ({
  sql_is_initialized: () => "select 1 as is_initialized",
  sql_introspect: () => "select introspection",
  sql_introspect_uninitialized: () => "select uninitialized introspection",
  migrate_with_introspection: (_name: string, _source: string, introspection: any) => ({
    Ok: {
      sql: introspection.schema_source ? [] : ["create table notes (id integer primary key)"],
      mark_success: "record migration",
    },
  }),
  set_schema: () => undefined,
  get_sync_status_sql: () => "select 1",
  get_sync_sql: () => ({ tables: [] }),
  calculate_sync_deltas: (_affectedRows: unknown, connectedSessions: Map<string, unknown>) => {
    if (deltaError) return deltaError;
    const recipientIds = Array.from(connectedSessions.keys()).filter((sessionId) => sessionIds.includes(sessionId));
    return {
      groups: recipientIds.length === 0 ? [] : [
        {
          session_ids: recipientIds,
          table_groups: [
            {
              table_name: "maps",
              headers: [
                "id",
                "name",
                "tiling",
                "tiling__tileRootKey",
                "tiling__tileWidth",
                "tiling__format",
              ],
              rows: [[1, "World", "Tiling", "tiles/root", 256, "Png"]],
            },
          ],
        },
      ],
    };
  },
  reshape_sync_table_groups: () => ([
    {
      table_name: "maps",
      headers: ["id", "name", "tiling"],
      rows: reshapedRows,
    },
  ]),
}));

mock.module("./query", () => ({
  run: async (
    _db: unknown,
    _queryMap: unknown,
    _queryId: string,
    _args: unknown,
    _executingSession: unknown,
    connectedSessions: Map<string, { session: Record<string, unknown> }>,
    syncDeltas: (affectedRowGroups: unknown[], connectedSessions: Map<string, { session: Record<string, unknown> }>, sendToSession: (sessionId: string, message: unknown) => void) => Promise<{ serverRevision?: number; originMessage?: unknown } | void>,
    originSessionId?: string,
    options?: { mode?: string },
  ) => {
    const queryResult: { kind: "success"; response: unknown; sync: (sendToSession: (sessionId: string, message: unknown) => void) => Promise<unknown> } = {
      kind: "success",
      response: {},
      sync: async (sendToSession: (sessionId: string, message: unknown) => void) => {
        const syncResult = await syncDeltas(
          [
            {
              table_name: "maps",
              headers: [
                "id",
                "name",
                "tiling",
                "tiling__tileRootKey",
                "tiling__tileWidth",
                "tiling__format",
              ],
              rows: [[1, "World", "Tiling", "tiles/root", 256, "Png"]],
            },
          ],
          connectedSessions,
          sendToSession,
          originSessionId,
        );
        if (syncResult && typeof syncResult.serverRevision === "number") {
          queryResult.response = {
            serverRevision: syncResult.serverRevision,
            ...(syncResult.originMessage === undefined ? {} : { sync: syncResult.originMessage }),
            ...(options?.mode === "sync" ? {} : { result: queryResult.response }),
          };
        }
        return syncResult;
      },
    };
    return queryResult;
  },
}));

const { runWithSync } = await import("./query-sync");
const { MAX_LIVE_SYNC_DELTA_ROWS, MAX_LIVE_SYNC_FANOUT_RECIPIENTS, MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES } = await import("./query-sync");
const { loadSchemaFromDatabase } = await import("./schema");

beforeEach(() => {
  introspectionResult = { schema_source: "test schema" };
  sessionIds = ["s1"];
  reshapedRows = [[1, "World", { _type: "Tiling", tileRootKey: "tiles/root", tileWidth: 256, format: { _type: "Png" } }]];
  deltaError = undefined;
});

function withoutServerRevision(message: unknown): unknown {
  if (typeof message !== "object" || message === null || !("serverRevision" in message)) {
    return message;
  }

  const { serverRevision: _serverRevision, databaseEpoch: _databaseEpoch, ...rest } = message as Record<string, unknown>;
  return rest;
}

function syncDb() {
  let revision = 0;
  const executedSql: string[] = [];
  return {
    execute: mock(async (sql: string) => {
      executedSql.push(sql);
      if (sql.includes("returning database_epoch, server_revision")) {
        revision += 1;
        return { rows: [{ database_epoch: "test-epoch", server_revision: revision }] };
      }

      return { rows: [] };
    }),
    executedSql,
  };
}

const schemaDb = {
  execute: mock(async (sql: string) => {
    if (sql.includes("is_initialized")) {
      return { rows: [{ is_initialized: 1 }] };
    }

    return { rows: [{ result: JSON.stringify(introspectionResult) }] };
  }),
};

test("runWithSync sends reshaped sync deltas", async () => {
  await loadSchemaFromDatabase(schemaDb as any);

  const result = await runWithSync(
    syncDb() as any,
    {} as any,
    "query-id",
    {},
    {},
    new Map([["s1", { session: {} }]]),
  );

  expect(result.kind).toBe("success");

  const sent: Array<{ sessionId: string; message: unknown }> = [];
  const syncResult = await result.sync((sessionId, message) => {
    sent.push({ sessionId, message });
  });

  expect(syncResult.serverRevision).toBe(1);
  expect(sent.map((entry) => ({ ...entry, message: withoutServerRevision(entry.message) }))).toEqual([
    {
      sessionId: "s1",
      message: {
        type: "delta",
        data: [
          {
            table_name: "maps",
            headers: ["id", "name", "tiling"],
            rows: [[1, "World", { _type: "Tiling", tileRootKey: "tiles/root", tileWidth: 256, format: { _type: "Png" } }]],
          },
        ],
      },
    },
  ]);
});

test("runWithSync stamps sync deltas with databaseId", async () => {
  introspectionResult = { schema_source: "campaign schema" };
  await loadSchemaFromDatabase("campaign:123", schemaDb as any);

  const result = await runWithSync(
    syncDb() as any,
    {} as any,
    "query-id",
    {},
    {},
    new Map([["s1", { session: {} }]]),
    "campaign:123",
  );

  const sent: Array<{ sessionId: string; message: any }> = [];
  await result.sync((sessionId, message) => {
    sent.push({ sessionId, message });
  });

  expect(sent[0].message.databaseId).toBe("campaign:123");
  expect(typeof sent[0].message.serverRevision).toBe("number");
});

test("runWithSync allocates live sync revisions from _pyre_sync", async () => {
  await loadSchemaFromDatabase(schemaDb as any);
  const db = syncDb();

  const result = await runWithSync(
    db as any,
    {} as any,
    "query-id",
    {},
    {},
    new Map([["s1", { session: {} }]]),
  );

  await result.sync(() => {});

  expect(db.executedSql).toEqual([
    expect.stringContaining("update _pyre_sync"),
  ]);
});

test("runWithSync allocates a revision even with no live recipients", async () => {
  sessionIds = [];
  await loadSchemaFromDatabase(schemaDb as any);

  const result = await runWithSync(
    syncDb() as any,
    {} as any,
    "query-id",
    {},
    {},
    new Map(),
  );

  const sent: Array<{ sessionId: string; message: unknown }> = [];
  const syncResult = await result.sync((sessionId, message) => {
    sent.push({ sessionId, message });
  });

  expect(sent).toHaveLength(0);
  expect(syncResult.serverRevision).toBe(1);
});

test("runWithSync skips the origin session when provided", async () => {
  sessionIds = ["s1", "s2"];
  await loadSchemaFromDatabase(schemaDb as any);

  const result = await runWithSync(
    syncDb() as any,
    {} as any,
    "query-id",
    {},
    {},
    new Map(sessionIds.map((sessionId) => [sessionId, { session: {} }])),
    undefined,
    "s1",
  );

  const sent: Array<{ sessionId: string; message: unknown }> = [];
  await result.sync((sessionId, message) => {
    sent.push({ sessionId, message });
  });

  expect(sent.map((entry) => entry.sessionId)).toEqual(["s2"]);
});

test("runWithSync includes origin authoritative sync in mutation response envelope", async () => {
  sessionIds = ["s1", "s2"];
  await loadSchemaFromDatabase(schemaDb as any);

  const result = await runWithSync(
    syncDb() as any,
    {} as any,
    "query-id",
    {},
    {},
    new Map(sessionIds.map((sessionId) => [sessionId, { session: {} }])),
    "campaign:123",
    "s1",
  );

  const sent: Array<{ sessionId: string; message: any }> = [];
  await result.sync((sessionId, message) => {
    sent.push({ sessionId, message });
  });

  expect(sent.map((entry) => entry.sessionId)).toEqual(["s2"]);
  expect((result.response as any).serverRevision).toBe(1);
  expect((result.response as any).sync).toEqual(sent[0].message);
  expect((result.response as any).sync.databaseId).toBe("campaign:123");
});

test("runWithSync builds origin sync from executing session when origin is not live-connected", async () => {
  sessionIds = ["s1"];
  await loadSchemaFromDatabase(schemaDb as any);

  const result = await runWithSync(
    syncDb() as any,
    {} as any,
    "query-id",
    {},
    {},
    new Map(),
    "campaign:123",
    "s1",
  );

  const sent: Array<{ sessionId: string; message: any }> = [];
  await result.sync((sessionId, message) => {
    sent.push({ sessionId, message });
  });

  expect(sent).toHaveLength(0);
  expect((result.response as any).serverRevision).toBe(1);
  expect((result.response as any).sync).toEqual({
    type: "delta",
    serverRevision: 1,
    databaseEpoch: "test-epoch",
    databaseId: "campaign:123",
    data: [
      {
        table_name: "maps",
        headers: ["id", "name", "tiling"],
        rows: [[1, "World", { _type: "Tiling", tileRootKey: "tiles/root", tileWidth: 256, format: { _type: "Png" } }]],
      },
    ],
  });
});

test("runWithSync sends syncRequired when delta row count exceeds cap", async () => {
  reshapedRows = Array.from({ length: MAX_LIVE_SYNC_DELTA_ROWS + 1 }, (_, index) => [index, "World", null]);
  await loadSchemaFromDatabase(schemaDb as any);

  const result = await runWithSync(
    syncDb() as any,
    {} as any,
    "query-id",
    {},
    {},
    new Map([["s1", { session: {} }]]),
  );

  const sent: Array<{ sessionId: string; message: any }> = [];
  await result.sync((sessionId, message) => {
    sent.push({ sessionId, message });
  });

  expect(sent).toHaveLength(1);
  expect(sent[0].sessionId).toBe("s1");
  expect(sent[0].message.type).toBe("syncRequired");
  expect(typeof sent[0].message.serverRevision).toBe("number");
});

test("runWithSync sends syncRequired when fanout recipient count exceeds cap", async () => {
  sessionIds = Array.from({ length: MAX_LIVE_SYNC_FANOUT_RECIPIENTS + 1 }, (_, index) => `s${index}`);
  await loadSchemaFromDatabase(schemaDb as any);

  const result = await runWithSync(
    syncDb() as any,
    {} as any,
    "query-id",
    {},
    {},
    new Map(sessionIds.map((sessionId) => [sessionId, { session: {} }])),
  );

  const sent: Array<{ sessionId: string; message: any }> = [];
  await result.sync((sessionId, message) => {
    sent.push({ sessionId, message });
  });

  expect(sent).toHaveLength(MAX_LIVE_SYNC_FANOUT_RECIPIENTS + 1);
  expect(sent.every((entry) => entry.message.type === "syncRequired")).toBe(true);
});

test("runWithSync sends syncRequired when payload bytes exceed cap", async () => {
  reshapedRows = [[1, "x".repeat(MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES), null]];
  await loadSchemaFromDatabase(schemaDb as any);

  const result = await runWithSync(
    syncDb() as any,
    {} as any,
    "query-id",
    {},
    {},
    new Map([["s1", { session: {} }]]),
    "campaign:123",
  );

  const sent: Array<{ sessionId: string; message: any }> = [];
  await result.sync((sessionId, message) => {
    sent.push({ sessionId, message });
  });

  expect(sent).toHaveLength(1);
  expect(sent[0].sessionId).toBe("s1");
  expect(sent[0].message.type).toBe("syncRequired");
  expect(sent[0].message.databaseId).toBe("campaign:123");
  expect(typeof sent[0].message.serverRevision).toBe("number");
});

test("runWithSync advances revision and requires catchup when delta calculation fails", async () => {
  deltaError = "Error: invalid connected session";
  await loadSchemaFromDatabase(schemaDb as any);
  const db = syncDb();
  const result = await runWithSync(
    db as any,
    {} as any,
    "query-id",
    {},
    {},
    new Map([
      ["origin", { session: {} }],
      ["recipient", { session: { invalid: true } }],
    ]),
    undefined,
    "origin",
  );
  const sent: Array<{ sessionId: string; message: any }> = [];
  const syncResult = await result.sync((sessionId, message) => sent.push({ sessionId, message }));

  expect(syncResult.serverRevision).toBe(1);
  expect(syncResult.originMessage.type).toBe("syncRequired");
  expect(sent).toEqual([
    {
      sessionId: "recipient",
      message: {
        type: "syncRequired",
        serverRevision: 1,
        databaseEpoch: "test-epoch",
      },
    },
  ]);
  expect(db.executedSql.some((sql) => sql.includes("returning database_epoch, server_revision"))).toBe(true);
});
