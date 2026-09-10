// @ts-nocheck
import { beforeEach, expect, mock, test } from "bun:test";
import { z } from "zod";
import { createClient } from "@libsql/client";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { meta as compiledCreate } from "./fixtures/compiled-batch/generated/queries/metadata/entryCreate";
import { sql as compiledCreateSql, syncSql as compiledCreateSyncSql } from "./fixtures/compiled-batch/generated/queries/sql/entryCreate";

let introspectionResult = { schema_source: "test schema" };
let sessionIds = ["s1"];
let reshapedRows = [[1, "World", { _type: "Tiling", tileRootKey: "tiles/root", tileWidth: 256, format: { _type: "Png" } }]];
let deltaError: string | undefined;
let replacementPlan: any;
let replacementReshape = false;
let replacementSession: any;
let activeSchema: any;
let replacementRowsValid = true;
let replacementValidatedSchemas: any[] = [];

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
  set_schema: schema => { activeSchema = schema; },
  get_schema_compiled_contract: () => activeSchema?.compiledContract ?? "contract-1",
  validate_replacement_table_groups: () => {
    replacementValidatedSchemas.push(structuredClone(activeSchema));
    return replacementRowsValid ? true : "Error: Invalid union discriminator";
  },
  get_sync_status_sql: () => "select 1",
  get_sync_sql: () => ({ tables: [] }),
  get_replacement_sql: (session: any, namespace: string) => {
    replacementSession = session;
    if (namespace !== "Main") throw Error("unexpected namespace");
    return typeof replacementPlan === "function" ? replacementPlan(session) : replacementPlan;
  },
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
  reshape_sync_table_groups: (groups: any) => replacementReshape ? groups : ([
    {
      table_name: "maps",
      headers: ["id", "name", "tiling"],
      rows: reshapedRows,
    },
  ]),
}));

const { runWithSync, runBatchWithSync, catchupReplacement } = await import("./query-sync");
const { MAX_LIVE_SYNC_DELTA_ROWS, MAX_LIVE_SYNC_FANOUT_RECIPIENTS, MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES } = await import("./query-sync");
const { loadSchemaFromDatabase } = await import("./schema");

test.skipIf(!existsSync(new URL("./wasm/pyre_wasm_bg.wasm", import.meta.url)))("actual WASM compiler contract, replacement codecs and linked legacy fallback", () => {
  const result = spawnSync(process.execPath, [new URL("./fixtures/replacement-wasm.ts", import.meta.url).pathname], { encoding: "utf8" });
  if (result.status !== 0) throw new Error(result.stderr + result.stdout);
}, 30000);

test("batch sync publishes only after atomic commit and needs no registered origin", async () => {
  const directory = mkdtempSync(join(tmpdir(), "pyre-batch-sync-"));
  const db = createClient({ url: `file:${join(directory, "test.db")}` });
  try {
    await db.execute("create table notes(id integer primary key)");
    await db.execute("create table _pyre_sync(id integer primary key, database_epoch text, server_revision integer)");
    await db.execute("insert into _pyre_sync values(1,'e1',0)");
    const manifest = { version: 1, manifestVersion: "m1", SessionValidator: z.object({ userId: z.number() }), queries: {
      create: { id: "create", operation: "insert", primary_db: "Main", InputValidator: z.object({}), SessionValidator: z.object({ userId: z.number() }),
        generatedEdit: { kind: "create", writableInputs: [], writeStatementIndices: [0] },
        session_args: [], optional_input_args: [], json_input_args: [],
        sql: [{ include: true, params: [], sql: "insert into notes default values returning id as _pyreEditId" }],
      },
    } };
    const authority = { databaseId: "tenant-1", namespace: "Main", manifest: "m1", instance: "tab-1", authGeneration: 2 };
    const request = { version: 1, ...authority, databaseEpoch: "e1", requestId: "request-1", sequence: 1, operations: [{ operation: "create", input: {} }] };
    const sent = [];
    const observedAtPublication = [];
    const result = await runBatchWithSync(db, manifest, authority, request, { userId: 7 }, new Map([
      ["broken", { session: {}, fence: { ...authority, databaseEpoch: "e1", instance: "broken" } }],
      ["subscriber", { session: {}, fence: { ...authority, databaseEpoch: "e1", instance: "recipient-tab", authGeneration: 8 } }],
      ...["databaseId", "namespace", "manifest", "databaseEpoch"].map(key => [key, { session: {}, fence: { ...authority, databaseEpoch: "e1", [key]: "wrong" } }]),
      ["unfenced", { session: {} }],
    ]), (id, message) => {
      if (id === "broken") throw Error("disconnected");
      observedAtPublication.push(db.execute("select server_revision, (select count(*) from notes) as n from _pyre_sync"));
      sent.push([id, message]);
    });
    expect(result).toEqual({ kind: "success", response: {
      ...authority, databaseEpoch: "e1", requestId: "request-1", status: "accepted", commitRevision: 1,
      results: [{ index: 0, operation: "create", value: { id: 1 } }],
      reconciliation: { kind: "replaceRequired", atLeast: 1, invalidate: true, minimumSafeRevision: 1 },
    } });
    expect(sent).toEqual([["subscriber", {
      type: "syncRequired", databaseId: "tenant-1", databaseEpoch: "e1", namespace: "Main", manifest: "m1", serverRevision: 1,
      instance: "recipient-tab", authGeneration: 8,
      reconciliation: { kind: "replaceRequired", atLeast: 1, invalidate: true, minimumSafeRevision: 1 },
    }]]);
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(1);
    expect((await observedAtPublication[0]).rows[0]).toEqual({ server_revision: 1, n: 1 });
    const withoutOrigin = await runBatchWithSync(db, manifest, authority, request, { userId: 7 });
    expect(withoutOrigin).toMatchObject({ kind: "success", response: { commitRevision: 2 } });
  } finally { db.close(); rmSync(directory, { recursive: true, force: true }); }
});

beforeEach(() => {
  introspectionResult = { schema_source: "test schema" };
  sessionIds = ["s1"];
  reshapedRows = [[1, "World", { _type: "Tiling", tileRootKey: "tiles/root", tileWidth: 256, format: { _type: "Png" } }]];
  deltaError = undefined;
  replacementPlan = undefined;
  replacementReshape = false;
  replacementSession = undefined;
  replacementRowsValid = true;
  replacementValidatedSchemas = [];
});

async function replacementDatabase(run: (fixture: any) => Promise<void>) {
  const directory = mkdtempSync(join(tmpdir(), "pyre-replacement-"));
  const url = `file:${join(directory, "test.db")}`;
  const db = createClient({ url });
  const authority = { databaseId: directory, namespace: "Main", manifest: "m1", instance: "tab", authGeneration: 2 };
  const request = { version: 1, ...authority, databaseEpoch: "e1", requestId: "read-1", target: 0 };
  const commands = {
    update: [z.object({ id: z.number(), body: z.string() }), "update notes set body = $body where id = $id"],
    delete: [z.object({ id: z.number() }), "delete from notes where id = $id"],
    key: [z.object({ id: z.number(), next: z.number() }), "update notes set id = $next where id = $id"],
    hide: [z.object({ id: z.number() }), "update notes set owner = 8 where id = $id"],
    revoke: [z.object({}), "delete from memberships where userId = $session_userId"],
    noop: [z.object({}), "delete from notes where id = 9999"],
  };
  const manifest = { version: 1, manifestVersion: "m1", compiledContract: "contract-1", SessionValidator: z.object({ userId: z.number().int() }), queries:
    Object.fromEntries(Object.entries(commands).map(([id, [InputValidator, sql]]) => [id, {
      id, operation: "transaction", primary_db: "Main", InputValidator, session_args: ["userId"],
      SessionValidator: z.object({ userId: z.number().int() }),
      optional_input_args: [], json_input_args: [], ReturnData: z.object({}),
      sql: [{ include: false, params: ["id", "body", "next", "session_userId"].filter(key => sql.includes(`$${key}`)), sql }],
    }])) };
  replacementReshape = true;
  // Execute permission-filtered compiler-shaped SQL on real libsql, not canned row results.
  replacementPlan = session => ({ tables: [
    { table_name: "notes", headers: ["id", "body", "owner", "project", "updatedAt"], json_columns: [], params: [[session.userId, session.userId]],
      sql: ["select * from notes where owner = ? or exists(select 1 from memberships m where m.project = notes.project and m.userId = ?) order by id"] },
    { table_name: "memberships", headers: ["project", "userId"], params: [[session.userId]], sql: ["select * from memberships where userId = ? order by project"] },
  ] });
  try {
    await db.execute("pragma journal_mode = WAL");
    await db.execute("create table _pyre_sync(id integer primary key, database_epoch text, server_revision integer)");
    await db.execute("insert into _pyre_sync values(1,'e1',0)");
    await db.execute("create table notes(id integer primary key, body text, owner integer, project integer, updatedAt integer)");
    await db.execute("insert into notes values(1,'own',7,1,0),(2,'linked',8,2,0),(3,'private',8,3,0)");
    await db.execute("create table memberships(project integer, userId integer)");
    await db.execute("insert into memberships values(2,7)");
    await loadSchemaFromDatabase(authority.databaseId, schemaDb);
    const replace = (overrides = {}, session = { userId: 7 }) => catchupReplacement(db, manifest, authority, { ...request, ...overrides }, session);
    const batch = (operations, recipients?, send?) => {
      const { target, ...fence } = request;
      return runBatchWithSync(db, manifest, authority, { ...fence, sequence: 1, operations }, { userId: 7 }, recipients, send);
    };
    await run({ db, url, authority, manifest, request, replace, batch });
  } finally { db.close(); rmSync(directory, { recursive: true, force: true }); }
}

test("no-SSE replacement removes deletes, visibility losses, linked revocations and old primary keys", async () => {
  await replacementDatabase(async ({ replace, batch }) => {
    let base = (await replace()).response.tables;
    expect(base.notes.rows.map(row => row.id)).toEqual([1, 2]);
    const accepted = await batch([
      { operation: "update", input: { id: 1, body: "first" } },
      { operation: "update", input: { id: 1, body: "last" } },
      { operation: "key", input: { id: 1, next: 10 } },
      { operation: "revoke", input: {} },
    ]);
    expect(accepted.response.results).toEqual(["update", "update", "key", "revoke"].map((operation, index) => ({ index, operation, value: {} })));
    expect(accepted.response.reconciliation).toEqual({ kind: "replaceRequired", atLeast: 1, invalidate: true, minimumSafeRevision: 1 });
    const snapshot = await replace({ target: 1 });
    expect(snapshot).toMatchObject({ kind: "success", response: { type: "replacement", target: 1, serverRevision: 1, complete: true, scope: "database" } });
    base = snapshot.response.tables;
    expect(base.notes.rows).toEqual([{ id: 10, body: "last", owner: 7, project: 1, updatedAt: 0 }]);
    expect(base.memberships.rows).toEqual([]);
    await batch([{ operation: "hide", input: { id: 10 } }]);
    base = (await replace({ target: 2 })).response.tables;
    expect(base.notes.rows).toEqual([]);
    await batch([{ operation: "delete", input: { id: 10 } }, { operation: "delete", input: { id: 2 } }]);
    expect((await replace({ target: 3 })).response.tables.notes.rows).toEqual([]);
    expect(replacementSession).toEqual({ userId: 7 });
  });
});

test("physical delete, no-op commits and wholly empty scope remain complete replacements", async () => {
  await replacementDatabase(async ({ replace, batch }) => {
    await batch([{ operation: "delete", input: { id: 1 } }]);
    expect((await replace({ target: 1 })).response.tables.notes.rows.map(row => row.id)).toEqual([2]);
    const noop = await batch([{ operation: "noop", input: {} }]);
    expect(noop.response.commitRevision).toBe(2);
    expect((await replace({ target: 2 })).response.serverRevision).toBe(2);
    replacementPlan = { tables: [] };
    expect(await replace({ target: 2 })).toMatchObject({ kind: "success", response: { complete: true, scope: "database", tables: {} } });
  });
});

test("replacement validates all request fences and target before reads, rejects epoch and future target", async () => {
  await replacementDatabase(async ({ db, manifest, authority, request, replace }) => {
    const transaction = db.transaction.bind(db);
    db.transaction = mock(transaction);
    for (const key of Object.keys(request)) {
      const missing = { ...request }; delete missing[key];
      expect((await catchupReplacement(db, manifest, authority, missing, { userId: 7 })).kind).toBe("error");
    }
    for (const key of ["databaseId", "instance", "namespace", "manifest", "authGeneration"]) {
      expect((await replace({ [key]: key === "authGeneration" ? 99 : "wrong" })).kind).toBe("error");
    }
    for (const target of [-1, 0.5, "0", NaN, Infinity, Number.MAX_SAFE_INTEGER + 1]) expect((await replace({ target })).kind).toBe("error");
    expect((await replace({ extra: true })).kind).toBe("error");
    expect((await replace({}, {})).error.errorType).toBe("InvalidSession");
    expect(db.transaction).not.toHaveBeenCalled();
    expect((await replace({ databaseEpoch: "old" })).error.errorType).toBe("InvalidRequest");
    expect((await replace({ target: 1 })).error.errorType).toBe("ReplacementUnavailable");
    expect((await replace({}, { userId: 8, ignoredClaim: true })).response.tables.notes.rows.map(row => row.id)).toEqual([2, 3]);
  });
});

test("replacement is pinned across concurrent commits and captures the request and effective session", async () => {
  await replacementDatabase(async ({ db, url, request, manifest, authority }) => {
    const writer = createClient({ url });
    const transaction = db.transaction.bind(db);
    let changed = false;
    db.transaction = async mode => {
      const tx = await transaction(mode);
      const execute = tx.execute.bind(tx);
      tx.execute = async statement => {
        const result = await execute(statement);
        if (!changed && typeof statement === "string" && statement.startsWith("select database_epoch")) {
          changed = true;
          await writer.batch(["update notes set body = 'new' where id = 1", "delete from memberships", "update _pyre_sync set server_revision = 1"], "write");
        }
        return result;
      };
      return tx;
    };
    try {
      const session = { userId: 7 };
      const pending = catchupReplacement(db, manifest, authority, request, session);
      request.target = 99; request.instance = "mutated"; session.userId = 8;
      const snapshot = await pending;
      expect(snapshot.response).toMatchObject({ target: 0, instance: "tab", serverRevision: 0 });
      expect(snapshot.response.tables.notes.rows.map(row => row.body)).toEqual(["own", "linked"]);
      expect(snapshot.response.tables.memberships.rows).toEqual([{ project: 2, userId: 7 }]);
      expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(1);
    } finally { writer.close(); }
  });
});

test("replacement errors never publish successful prefixes and read-only retry recovers", async () => {
  await replacementDatabase(async ({ replace, batch, db }) => {
    const valid = replacementPlan;
    await batch([{ operation: "noop", input: {} }]);
    replacementPlan = session => ({ tables: [...valid(session).tables, { table_name: "bad", headers: ["id"], sql: ["select private_missing from notes"] }] });
    expect(await replace({ target: 1 })).toEqual({ kind: "error", error: { errorType: "ReplacementUnavailable", message: "ReplacementUnavailable" } });
    replacementPlan = valid;
    expect((await replace({ target: 1 })).response.serverRevision).toBe(1);
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(1);
  });
});

test("replacement never truncates at legacy page limits and rejects malformed aggregate completeness", async () => {
  await replacementDatabase(async ({ replace, db }) => {
    await db.execute("with recursive ids(n) as (select 10 union all select n + 1 from ids where n < 5010) insert into notes select n, 'many', 7, 1, 0 from ids");
    expect((await replace()).response.tables.notes.rows).toHaveLength(5003);
    for (const sql of ["select null as _pyre_rows", "select '{}' as _pyre_rows", "select '[[]]' as _pyre_rows", "select id as wrong from notes"]) {
      replacementPlan = { tables: [{ table_name: "notes", headers: ["id"], sql: [sql] }] };
      expect((await replace()).error.errorType).toBe("ReplacementUnavailable");
    }
    replacementPlan = { tables: [{ table_name: "notes", headers: ["id"], sql: ["select '[]' as _pyre_rows"] }] };
    expect((await replace()).response.tables.notes.rows).toEqual([]);
    const transaction = db.transaction.bind(db);
    db.transaction = async mode => {
      const tx = await transaction(mode);
      tx.commit = async () => { throw Error("read completion lost"); };
      return tx;
    };
    expect(await replace()).toEqual({ kind: "error", error: { errorType: "ReplacementUnavailable", message: "ReplacementUnavailable" } });
  });
});

test("named mutations publish fenced replacement hints at their committed revision, including no-ops", async () => {
  await replacementDatabase(async ({ db, manifest, authority, replace }) => {
    const recipients = new Map([["reader", { session: { userId: 7 }, fence: { ...authority, databaseEpoch: "e1" } }]]);
    const revoked = await runWithSync(db, manifest.queries, "revoke", {}, { userId: 7 }, recipients, authority.databaseId, "reader");
    expect(revoked.kind).toBe("success");
    expect(revoked.response).toEqual({});
    expect((await db.execute("select count(*) as n from memberships")).rows[0].n).toBe(0);
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(1);
    const sent = [];
    await revoked.sync((id, message) => sent.push([id, message]));
    expect(sent).toEqual([["reader", { ...authority, databaseEpoch: "e1", type: "syncRequired", serverRevision: 1,
      reconciliation: { kind: "replaceRequired", atLeast: 1, invalidate: true, minimumSafeRevision: 1 } }]]);
    expect(revoked.response).toEqual({ databaseEpoch: "e1", serverRevision: 1, result: {} });
    expect((await replace({ target: 1 })).response.tables.notes.rows.map(row => row.id)).toEqual([1]);
    await revoked.sync(() => {});
    expect(revoked.response).toEqual({ databaseEpoch: "e1", serverRevision: 1, result: {} });
    const noop = await runWithSync(db, manifest.queries, "noop", {}, { userId: 7 }, recipients, authority.databaseId);
    recipients.set("reader", { session: { userId: 8 }, fence: { ...authority, databaseEpoch: "e1", instance: "new", authGeneration: 3 } });
    const hints = [];
    await noop.sync((id, message) => hints.push(message));
    expect(hints).toHaveLength(1);
    expect(hints[0]).toMatchObject({ instance: "new", authGeneration: 3, serverRevision: 2 });
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(2);
    await db.execute("create trigger deny_revision before update on _pyre_sync begin select raise(abort, 'revision denied'); end");
    await expect(runWithSync(db, manifest.queries, "delete", { id: 1 }, { userId: 7 }, recipients, authority.databaseId)).rejects.toThrow();
    expect((await db.execute("select id from notes where id = 1")).rows).toEqual([{ id: 1 }]);
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(2);
  });
});

test("replacement requires the manifest contract and restores its captured schema across cache refresh", async () => {
  await replacementDatabase(async ({ db, manifest, authority, request, replace }) => {
    const transaction = db.transaction.bind(db);
    db.transaction = mock(transaction);
    for (const compiledContract of [undefined, "", "different-contract"]) {
      expect(await catchupReplacement(db, { ...manifest, compiledContract }, authority, request, { userId: 7 }))
        .toMatchObject({ kind: "error", error: { errorType: "InvalidRequest" } });
    }
    expect(db.transaction).not.toHaveBeenCalled();
    db.transaction = async mode => {
      const tx = await transaction(mode);
      const execute = tx.execute.bind(tx);
      tx.execute = async statement => {
        const result = await execute(statement);
        introspectionResult = { schema_source: "different permissions and union", compiledContract: "other-contract" };
        await loadSchemaFromDatabase(authority.databaseId, schemaDb);
        return result;
      };
      return tx;
    };
    expect((await replace()).kind).toBe("success");
    expect(replacementValidatedSchemas).toHaveLength(2);
    expect(replacementValidatedSchemas.every(schema => schema.schema_source === "test schema")).toBe(true);
    // New requests cannot silently use that newly cached schema under the old manifest.
    expect((await replace()).error.errorType).toBe("InvalidRequest");
  });
});

test("named no-op revision commits without subscribers, while declared reads allocate no revision", async () => {
  await replacementDatabase(async ({ db, manifest, authority }) => {
    const result = await runWithSync(db, manifest.queries, "noop", {}, { userId: 7 }, undefined, authority.databaseId);
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(1);
    const send = mock(() => {});
    expect(await result.sync(send)).toEqual({ databaseEpoch: "e1", serverRevision: 1 });
    expect(send).not.toHaveBeenCalled();
    const read = { ...manifest.queries.noop, id: "read", operation: "query", sql: [
      { include: true, params: [], sql: "select json_object('count', count(*)) as notes from notes" },
    ] };
    const found = await runWithSync(db, { read }, "read", {}, { userId: 7 }, undefined, authority.databaseId);
    expect(found.response).toEqual({ notes: [{ count: 3 }] });
    expect(await found.sync(send)).toEqual({});
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(1);
  });
});

test("named sync executes actual generated SQL, retains declared rows, and never sends deltas to fenced readers", async () => {
  await replacementDatabase(async ({ db, authority }) => {
    await db.execute("create table entries(id text primary key, release text, enabled integer, count integer, role text, details blob, updatedAt integer)");
    const query = { ...compiledCreate, sql: compiledCreateSql, syncSql: compiledCreateSyncSql };
    const input = { id: "generated", release: "release", enabled: true, count: 1, role: { _type: "Member" }, details: { _type: "Note", count: 2, enabled: false } };
    const fence = { ...authority, namespace: query.primary_db, databaseEpoch: "e1", instance: "other-tab", authGeneration: 8 };
    const result = await runWithSync(db, { [query.id]: query }, query.id, input,
      { userId: 7, role: { _type: "Member" }, unrelated: "required" },
      new Map([["reader", { session: { userId: 9 }, fence }]]), authority.databaseId);
    expect(result.kind).toBe("success");
    expect(result.response.entry[0]).toMatchObject(input);
    const original = structuredClone(result.response);
    const sent = [];
    await result.sync((id, message) => sent.push(message));
    expect(sent).toEqual([{ type: "syncRequired", ...fence, serverRevision: 1,
      reconciliation: { kind: "replaceRequired", atLeast: 1, invalidate: true, minimumSafeRevision: 1 } }]);
    expect(result.response).toEqual({ databaseEpoch: "e1", serverRevision: 1, result: original });
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(1);
  });
});

test("invalid union/scalar row validation fails closed without publishing a complete snapshot", async () => {
  await replacementDatabase(async ({ replace, db }) => {
    await db.execute("update notes set body = 'UnknownUnionVariant' where id = 1");
    replacementRowsValid = false;
    expect(await replace()).toEqual({ kind: "error", error: { errorType: "ReplacementUnavailable", message: "ReplacementUnavailable" } });
    expect(replacementValidatedSchemas).toHaveLength(1);
    replacementRowsValid = true;
    expect((await replace()).kind).toBe("success");
  });
});

test("publication uses current recipient lifetime after commit and failures never erase acceptance", async () => {
  await replacementDatabase(async ({ batch, authority, db }) => {
    const recipients = new Map([["reader", { session: { userId: 8 }, fence: { ...authority, databaseEpoch: "e1", instance: "old", authGeneration: 3 } }]]);
    const sent = [];
    const pending = batch([{ operation: "noop", input: {} }], recipients, (id, message) => {
      sent.push([id, message]);
      throw Error("delivery unavailable");
    });
    recipients.set("reader", { session: { userId: 9 }, fence: { ...authority, databaseEpoch: "e1", instance: "new", authGeneration: 4 } });
    expect((await pending).response.commitRevision).toBe(1);
    expect(sent[0][1]).toMatchObject({ ...authority, databaseEpoch: "e1", instance: "new", authGeneration: 4, serverRevision: 1 });
    const send = mock(() => {});
    await db.execute("create trigger deny before delete on notes begin select raise(abort, 'private denial'); end");
    expect((await batch([{ operation: "delete", input: { id: 1 } }], recipients, send)).kind).toBe("error");
    expect(send).not.toHaveBeenCalled();
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(1);
    await db.execute("drop trigger deny");
    expect((await batch([{ operation: "noop", input: {} }], recipients, async () => { throw Error("async delivery failed"); })).response.commitRevision).toBe(2);
    expect((await batch([{ operation: "noop", input: {} }], recipients, () => new Promise(() => {}))).response.commitRevision).toBe(3);
  });
});

test("memory batch sync rejects without detaching or publishing, while empty batches confirm", async () => {
  const db = createClient({ url: "file::memory:" });
  try {
    await db.execute("create table notes(id integer primary key)");
    await db.execute("insert into notes values(1)");
    await db.execute("create table _pyre_sync(id integer primary key, database_epoch text, server_revision integer)");
    await db.execute("insert into _pyre_sync values(1,'e1',0)");
    const manifest = { version: 1, manifestVersion: "m1", SessionValidator: z.object({}), queries: {
      create: { id: "create", operation: "insert", primary_db: "Main", InputValidator: z.object({}), SessionValidator: z.object({}),
        generatedEdit: { kind: "create", writableInputs: [], writeStatementIndices: [0] },
        session_args: [], optional_input_args: [], json_input_args: [],
        sql: [{ include: true, params: [], sql: "insert into notes default values returning id as _pyreEditId" }],
      },
    } };
    const authority = { databaseId: "memory-sync", namespace: "Main", manifest: "m1", instance: "tab-1", authGeneration: 2 };
    const request = { version: 1, ...authority, databaseEpoch: "e1", requestId: "request-1", sequence: 1, operations: [{ operation: "create", input: {} }] };
    const send = mock(() => {});
    const recipients = new Map([["subscriber", { session: {} }]]);
    expect(await runBatchWithSync(db, manifest, authority, request, {}, recipients, send))
      .toEqual({ kind: "error", error: { errorType: "TransactionFailed", message: "TransactionFailed" } });
    expect(await runBatchWithSync(db, manifest, authority, { ...request, operations: [] }, {}, recipients, send))
      .toMatchObject({ kind: "success", response: { status: "confirmed", results: [] } });
    expect(send).not.toHaveBeenCalled();
    expect((await db.execute("select * from notes")).rows).toEqual([{ id: 1 }]);
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(0);
  } finally { db.close(); }
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
    batch: mock(async () => [{
      columns: ["_affectedRows"],
      rows: [{
        _affectedRows: JSON.stringify([{
          table_name: "maps",
          headers: ["id", "name", "tiling", "tiling__tileRootKey", "tiling__tileWidth", "tiling__format"],
          rows: [[1, "World", "Tiling", "tiles/root", 256, "Png"]],
        }]),
      }],
    }]),
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
