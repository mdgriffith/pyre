// @ts-nocheck
import { beforeEach, expect, mock, test } from "bun:test";
import { z } from "zod";
import { createClient } from "@libsql/client";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

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

function syncDb(removed = false) {
  let revision = 0;
  const executedSql: string[] = [];
  return {
    batch: mock(async (statements: any[]) => {
      executedSql.push(statements.at(-1));
      revision += 1;
      return [{
      columns: ["_affectedRows"],
      rows: [{
        _affectedRows: JSON.stringify([{
          table_name: "maps",
          headers: ["id", "name", "tiling", "tiling__tileRootKey", "tiling__tileWidth", "tiling__format", ...(removed ? ["_pyre_removed"] : [])],
          rows: [[1, "World", "Tiling", "tiles/root", 256, "Png", ...(removed ? [true] : [])]],
        }]),
      }],
    }, { rows: [{ database_epoch: "test-epoch", server_revision: revision }] }];
    }),
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

const queryMap = {
  "query-id": {
    id: "query-id",
    sql: [{ include: true, params: [], sql: "select _affectedRows" }],
    session_args: [],
    optional_input_args: [],
    json_input_args: [],
    InputValidator: z.object({}),
    SessionValidator: z.object({}),
  },
};

const schemaDb = {
  execute: mock(async (sql: string) => {
    if (sql.includes("is_initialized")) {
      return { rows: [{ is_initialized: 1 }] };
    }

    return { rows: [{ result: JSON.stringify(introspectionResult) }] };
  }),
};

test('deleted preimages produce only authorized identity removals, including the HTTP origin', async () => {
  await loadSchemaFromDatabase(schemaDb as any);
  sessionIds = ['origin', 'peer'];
  const result = await runWithSync(syncDb(true) as any, queryMap, 'query-id', {}, {},
    new Map(['origin', 'peer', 'hidden'].map(id => [id, { session: {} }])), undefined, 'origin');
  const sent = new Map();
  const sync = await result.sync((id, message) => sent.set(id, message));
  expect(sync.originMessage.type).toBe('delta');
  expect(sync.originMessage.data).toEqual([{ table_name: 'maps', headers: ['id', '_pyre_removed'], rows: [[1, true]] }]);
  expect(sent.get('peer')).toEqual(sync.originMessage);
  expect(sent.get('hidden').type).toBe('delta');
  expect(sent.get('hidden').data).toEqual([]);
  expect(JSON.stringify(sent.get('hidden'))).not.toContain('maps');
});

test('commit order determines revisions even when fanout is reversed or repeated', async () => {
  await loadSchemaFromDatabase(schemaDb as any);
  const db = syncDb();
  const first = await runWithSync(db as any, queryMap, 'query-id', {}, {}, new Map([['s1', { session: {} }]]));
  const second = await runWithSync(db as any, queryMap, 'query-id', {}, {}, new Map([['s1', { session: {} }]]));
  const sent: any[] = [];
  const newer = await second.sync((_id, message) => sent.push(message));
  const older = await first.sync((_id, message) => sent.push(message));
  expect([newer.serverRevision, older.serverRevision]).toEqual([2, 1]);
  await first.sync((_id, message) => sent.push(message));
  expect(sent).toHaveLength(2);
  expect(first.response.result).not.toHaveProperty('serverRevision');
});

test("runWithSync publishes an atomic repeated-operation batch once to origin and peer", async () => {
  await loadSchemaFromDatabase(schemaDb as any);
  sessionIds = ["origin", "peer"];
  const directory = mkdtempSync(join(tmpdir(), "pyre-sync-batch-"));
  const db = createClient({ url: `file:${join(directory, "test.db")}` });
  try {
    await db.batch([
      "create table maps (id integer primary key, name text)",
      "insert into maps values (1, 'Initial')",
      "create table _pyre_sync (id integer primary key, database_epoch text, server_revision integer)",
      "insert into _pyre_sync values (1, 'test-epoch', 0)",
    ]);
    const queries = { edit: {
      ...queryMap["query-id"], id: "edit", generatedEdit: { writeStatement: 0 },
      InputValidator: z.object({ name: z.string() }),
      sql: [
        { include: false, params: ["name"], sql: "update maps set name = $name where id = 1" },
        { include: true, params: [], sql: "select json_array(json_object('table_name', 'maps', 'headers', json_array('id', 'name'), 'rows', json_array(json_array(id, name)))) as _affectedRows from maps" },
      ],
    } };
    const result = await runWithSync(db, queries, [
      { queryId: "edit", input: { name: "Intermediate" } },
      { queryId: "edit", input: { name: "World" } },
    ], undefined, {}, new Map([["origin", { session: {} }], ["peer", { session: {} }]]), undefined, "origin");
    expect(result.kind).toBe("success");
    expect((await db.execute("select * from maps")).rows).toEqual([{ id: 1, name: "World" }]);
    const sent = [];
    const sync = await result.sync((id, message) => sent.push({ id, message }));
    expect(sync.serverRevision).toBe(1);
    expect(sync.originMessage.type).toBe("delta");
    expect(sent).toHaveLength(1);
    expect(sent[0].id).toBe("peer");
    expect(sent[0].message).toEqual(sync.originMessage);
    expect(result.response.result.map(operation => operation.index)).toEqual([0, 1]);
    await result.sync(() => { throw new Error("must not republish"); });
  } finally {
    db.close();
    rmSync(directory, { recursive: true, force: true });
  }
});

test('never-visible recipients get an empty delta while visible edits remain incremental', async () => {
  await loadSchemaFromDatabase(schemaDb as any);
  const result = await runWithSync(syncDb() as any, queryMap, 'query-id', {}, {}, new Map([
    ['s1', { session: {} }], ['denied', { session: {} }],
  ]));
  const sent: any[] = [];
  await result.sync((id, message) => sent.push({ id, message }));
  expect(sent.find((entry) => entry.id === 's1').message.type).toBe('delta');
  expect(withoutServerRevision(sent.find((entry) => entry.id === 'denied').message)).toEqual({ type: 'delta', data: [] });
});

test("runWithSync sends reshaped sync deltas", async () => {
  await loadSchemaFromDatabase(schemaDb as any);

  const result = await runWithSync(
    syncDb() as any,
    queryMap,
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
    queryMap,
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
    queryMap,
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
    queryMap,
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
    queryMap,
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
    queryMap,
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
    queryMap,
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
    queryMap,
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
    queryMap,
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
    queryMap,
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
    queryMap,
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
  expect(syncResult.originMessage.type).toBe("invalidate");
  expect(sent).toEqual([
    {
      sessionId: "recipient",
      message: {
        type: "invalidate",
        serverRevision: 1,
        databaseEpoch: "test-epoch",
      },
    },
  ]);
  expect(db.executedSql.some((sql) => sql.includes("returning database_epoch, server_revision"))).toBe(true);
});
