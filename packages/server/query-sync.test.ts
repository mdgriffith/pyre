// @ts-nocheck
import { beforeEach, expect, mock, test } from "bun:test";
import { z } from "zod";
import { createClient } from "@libsql/client";
import { existsSync, mkdtempSync, rmSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { meta as compiledCreate } from "./fixtures/compiled-batch/generated/queries/metadata/entryCreate";
import { sql as compiledCreateSql, syncSql as compiledCreateSyncSql } from "./fixtures/compiled-batch/generated/queries/sql/entryCreate";
import { compiledContract } from "./fixtures/compiled-batch/generated/manifest";

const createUuidV7 = "01890f2e-7b5c-7cc8-98c4-dc0c0c07398f";
const compiledSource = readFileSync(new URL("./fixtures/compiled-batch/schema.pyre", import.meta.url), "utf8");
const queryAuthority = { primary_db: "Main", attached_dbs: [], schemaContracts: { Main: "contract-1" } };
const manifestAuthority = { compiledContract: "contract-1", replacementContracts: { Main: "contract-1" } };
const compiledCreateMetadata = {
  ...compiledCreate,
  generatedEdit: { ...compiledCreate.generatedEdit, createUuidInput: "id" },
};

let introspectionResult = { schema_source: "test schema" };
let sessionIds = ["s1"];
let reshapedRows = [[1, "World", { _type: "Tiling", tileRootKey: "tiles/root", tileWidth: 256, format: { _type: "Png" } }]];
let deltaError: string | undefined;
let reshapeError: string | undefined;
let replacementPlan: any;
let replacementReshape = false;
let replacementSession: any;
let activeSchema: any;
let replacementRowsValid = true;
let replacementValidatedSchemas: any[] = [];
let deltaSchemas: any[] = [];

mock.module("./wasm/pyre_wasm.js", () => ({
  sql_is_initialized: () => "select 1 as is_initialized",
  sql_introspect: () => "select json_object('schema_source', schema) as result from _pyre_migrations where finished_at is not null and error is null order by id desc limit 1",
  sql_introspect_uninitialized: () => "select uninitialized introspection",
  migrate_with_introspection: (_name: string, _source: string, introspection: any) => ({
    Ok: {
      sql: introspection.schema_source ? [] : ["create table notes (id integer primary key)"],
      mark_success: "record migration",
    },
  }),
  set_schema: schema => { activeSchema = schema; },
  process_introspection: introspection => introspection,
  get_schema_compiled_contract: () => schemaContract(activeSchema?.schema_source),
  get_schema_manifest_contract: () => activeSchema?.schema_source === compiledSource ? compiledContract : schemaContract(activeSchema?.schema_source),
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
    deltaSchemas.push(structuredClone(activeSchema));
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
  reshape_sync_table_groups: (groups: any) => reshapeError ?? (replacementReshape ? groups : ([
    {
      table_name: "maps",
      headers: ["id", "name", "tiling"],
      rows: reshapedRows,
    },
  ])),
}));

const { runWithSync, runBatchWithSync, catchupReplacement } = await import("./query-sync");
const { readReplacementTables, MAX_REPLACEMENT_PAYLOAD_BYTES, MAX_REPLACEMENT_ROWS } = await import("./sync");
const { MAX_LIVE_SYNC_DELTA_ROWS, MAX_LIVE_SYNC_FANOUT_RECIPIENTS, MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES } = await import("./query-sync");
const { loadSchemaFromDatabase } = await import("./schema");

function schemaContract(source: string) {
  if (source === compiledSource) return compiledCreate.schemaContracts._default;
  if (source === "test schema") return "contract-1";
  if (source === "campaign schema") return "campaign-contract";
  return `unknown-contract:${source}`;
}

async function persistAuthority(db, databaseId, source = "test schema") {
  await db.execute("create table if not exists _pyre_migrations(id integer primary key, finished_at integer, error text, schema text)");
  await db.execute({ sql: "insert or replace into _pyre_migrations values(1,1,NULL,?)", args: [source] });
  await loadSchemaFromDatabase(databaseId, db);
}

test.skipIf(!existsSync(new URL("./wasm/pyre_wasm_bg.wasm", import.meta.url)))("actual WASM compiler contract, replacement codecs and linked legacy fallback", () => {
  const result = spawnSync(process.execPath, [new URL("./fixtures/replacement-wasm.ts", import.meta.url).pathname], { encoding: "utf8" });
  if (result.status !== 0) throw new Error(result.stderr + result.stdout);
}, 30000);

test("batch sync publishes only after atomic commit and needs no registered origin", async () => {
  const directory = mkdtempSync(new URL("../../target/pyre-batch-sync-", import.meta.url));
  const db = createClient({ url: `file:${join(directory, "test.db")}` });
  try {
    await db.execute("create table notes(id integer primary key)");
    await db.execute("create table _pyre_sync(id integer primary key, database_epoch text, server_revision integer)");
    await db.execute("insert into _pyre_sync values(1,'e1',0)");
    await persistAuthority(db, "tenant-1");
    const manifest = { ...manifestAuthority, version: 1, manifestVersion: "m1", SessionValidator: z.object({ userId: z.number() }), queries: {
      create: { ...queryAuthority, id: "create", operation: "insert", InputValidator: z.object({ id: z.string() }), SessionValidator: z.object({ userId: z.number() }),
        generatedEdit: { kind: "create", createUuidInput: "id", writableInputs: ["id"], writeStatementIndices: [0] },
        session_args: [], optional_input_args: [], json_input_args: [],
        sql: [{ include: true, params: [], sql: "insert into notes default values returning id as _pyreEditId" }],
      },
    } };
    const authority = { databaseId: "tenant-1", namespace: "Main", manifest: "m1", instance: "tab-1", authGeneration: 2 };
    const request = { version: 1, ...authority, databaseEpoch: "e1", requestId: "request-1", sequence: 1, operations: [{ operation: "create", input: { id: createUuidV7 } }] };
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
  reshapeError = undefined;
  replacementPlan = undefined;
  replacementReshape = false;
  replacementSession = undefined;
  replacementRowsValid = true;
  replacementValidatedSchemas = [];
  deltaSchemas = [];
});

async function replacementDatabase(run: (fixture: any) => Promise<void>) {
  const directory = mkdtempSync(new URL("../../target/pyre-replacement-", import.meta.url));
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
  const manifest = { version: 1, manifestVersion: "m1", compiledContract: "contract-1", replacementContracts: { Main: "contract-1" }, SessionValidator: z.object({ userId: z.number().int() }), queries:
    Object.fromEntries(Object.entries(commands).map(([id, [InputValidator, sql]]) => [id, {
      ...queryAuthority, id, operation: "transaction", InputValidator, session_args: ["userId"],
      SessionValidator: z.object({ userId: z.number().int() }),
      optional_input_args: [], json_input_args: [], ReturnData: z.object({}),
      sql: [{ include: false, params: ["id", "body", "next", "session_userId"].filter(key => sql.includes(`$${key}`)), sql }],
    }])) };
  replacementReshape = true;
  // Execute permission-filtered compiler-shaped SQL on real libsql, not canned row results.
  replacementPlan = session => ({ tables: [
    { table_name: "notes", headers: ["id", "body", "owner", "project", "updatedAt"], json_columns: [], params: [[session.userId, session.userId]],
      replacement_bounds_sql: "select count(*) as _pyre_row_count, coalesce(sum(length(cast(json_array(id, body, owner, project, updatedAt) as blob))), 0) + case when count(*) = 0 then 2 else count(*) + 1 end as _pyre_byte_count from notes where owner = ? or exists(select 1 from memberships m where m.project = notes.project and m.userId = ?)",
      sql: ["select * from notes where owner = ? or exists(select 1 from memberships m where m.project = notes.project and m.userId = ?) order by id"] },
    { table_name: "memberships", headers: ["project", "userId"], params: [[session.userId]],
      replacement_bounds_sql: "select count(*) as _pyre_row_count, coalesce(sum(length(cast(json_array(project, userId) as blob))), 0) + case when count(*) = 0 then 2 else count(*) + 1 end as _pyre_byte_count from memberships where userId = ?",
      sql: ["select * from memberships where userId = ? order by project"] },
  ] });
  try {
    await db.execute("pragma journal_mode = WAL");
    await db.execute("create table _pyre_sync(id integer primary key, database_epoch text, server_revision integer)");
    await db.execute("insert into _pyre_sync values(1,'e1',0)");
    await db.execute("create table _pyre_migrations(id integer primary key, finished_at integer, error text, schema text)");
    await db.execute("insert into _pyre_migrations values(1,1,NULL,'test schema')");
    await db.execute("create table notes(id integer primary key, body text, owner integer, project integer, updatedAt integer)");
    await db.execute("insert into notes values(1,'own',7,1,0),(2,'linked',8,2,0),(3,'private',8,3,0)");
    await db.execute("create table memberships(project integer, userId integer)");
    await db.execute("insert into memberships values(2,7)");
    await loadSchemaFromDatabase(authority.databaseId, db);
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

test("replacement rejects missing or changed snapshot schema before compiling row reads", async () => {
  await replacementDatabase(async ({ db, replace }) => {
    await db.execute("update _pyre_migrations set schema = 'revoked permissions'");
    expect(await replace()).toMatchObject({ kind: "error", error: { errorType: "InvalidRequest" } });
    expect(replacementSession).toBeUndefined();
    await db.execute("delete from _pyre_migrations");
    expect(await replace()).toMatchObject({ kind: "error", error: { errorType: "InvalidRequest" } });
    expect(replacementSession).toBeUndefined();
    await db.execute("insert into _pyre_migrations values(2,1,NULL,'test schema')");
    expect((await replace()).kind).toBe("success");
  });
});

test("replacement preflights every table before materializing rows and enforces byte bounds", async () => {
  replacementPlan = { tables: [
    { table_name: "first", headers: ["id"], replacement_bounds_sql: "small", sql: ["materialize first"] },
    { table_name: "second", headers: ["id"], replacement_bounds_sql: "oversized", sql: ["materialize second"] },
  ] };
  const materialized: string[] = [];
  const db = { execute: mock(async ({ sql }: { sql: string }) => {
    if (sql.startsWith("materialize")) {
      materialized.push(sql);
      throw Error("row materialization ran");
    }
    return { rows: [{ _pyre_row_count: 0, _pyre_byte_count: sql === "small" ? 2 : MAX_REPLACEMENT_PAYLOAD_BYTES + 1 }] };
  }) };

  await expect(readReplacementTables(db as any, {}, "Main", () => {})).rejects.toThrow("too large");
  expect(materialized).toEqual([]);
  expect(db.execute).toHaveBeenCalledTimes(2);

  replacementPlan = { tables: [
    { table_name: "oversized", headers: ["id"], replacement_bounds_sql: "too many", sql: ["materialize oversized"] },
  ] };
  db.execute = mock(async ({ sql }: { sql: string }) => {
    if (sql.startsWith("materialize")) throw Error("row materialization ran");
    return { rows: [{ _pyre_row_count: MAX_REPLACEMENT_ROWS + 1, _pyre_byte_count: 0 }] };
  });
  await expect(readReplacementTables(db as any, {}, "Main", () => {})).rejects.toThrow("too large");
});

test("replacement does not reuse live-delta limits and rejects malformed aggregate completeness", async () => {
  await replacementDatabase(async ({ replace, db }) => {
    await db.execute("with recursive ids(n) as (select 10 union all select n + 1 from ids where n < 5010) insert into notes select n, 'many', 7, 1, 0 from ids");
    expect((await replace()).response.tables.notes.rows).toHaveLength(5003);
    for (const sql of ["select null as _pyre_rows", "select '{}' as _pyre_rows", "select '[[]]' as _pyre_rows", "select id as wrong from notes"]) {
      replacementPlan = { tables: [{ table_name: "notes", headers: ["id"], replacement_bounds_sql: "select 0 as _pyre_row_count, 2 as _pyre_byte_count", sql: [sql] }] };
      expect((await replace()).error.errorType).toBe("ReplacementUnavailable");
    }
    replacementPlan = { tables: [{ table_name: "notes", headers: ["id"], replacement_bounds_sql: "select 0 as _pyre_row_count, 2 as _pyre_byte_count", sql: ["select '[]' as _pyre_rows"] }] };
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
    const recipients = new Map([
      ["reader", { session: { userId: 7 }, fence: { ...authority, databaseEpoch: "e1" } }],
      ["other-namespace", { session: { userId: 7 }, fence: {
        ...authority, namespace: "Archive", manifest: "archive-m1", databaseEpoch: "e1",
      } }],
    ]);
    const sent = [];
    const revoked = await runWithSync(db, manifest.queries, "revoke", {}, { userId: 7 }, recipients, authority.databaseId, "reader",
      (id, message) => sent.push([id, message]), manifest.manifestVersion);
    expect(revoked.kind).toBe("success");
    expect(revoked.response).toEqual({ databaseEpoch: "e1", serverRevision: 1, result: {} });
    expect((await db.execute("select count(*) as n from memberships")).rows[0].n).toBe(0);
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(1);
    await revoked.sync(() => {});
    expect(sent).toEqual([["reader", { ...authority, databaseEpoch: "e1", type: "syncRequired", serverRevision: 1,
      reconciliation: { kind: "replaceRequired", atLeast: 1, invalidate: true, minimumSafeRevision: 1 } }]]);
    expect(revoked.response).toEqual({ databaseEpoch: "e1", serverRevision: 1, result: {} });
    expect((await replace({ target: 1 })).response.tables.notes.rows.map(row => row.id)).toEqual([1]);
    await revoked.sync(() => {});
    expect(revoked.response).toEqual({ databaseEpoch: "e1", serverRevision: 1, result: {} });
    recipients.set("reader", { session: { userId: 8 }, fence: { ...authority, databaseEpoch: "e1", instance: "new", authGeneration: 3 } });
    const hints = [];
    const noop = await runWithSync(db, manifest.queries, "noop", {}, { userId: 7 }, recipients, authority.databaseId, undefined,
      (id, message) => hints.push(message), manifest.manifestVersion);
    await noop.sync(() => {});
    expect(hints).toHaveLength(1);
    expect(hints[0]).toMatchObject({ instance: "new", authGeneration: 3, serverRevision: 2 });
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(2);
    await db.execute("create trigger deny_revision before update on _pyre_sync begin select raise(abort, 'revision denied'); end");
    await expect(runWithSync(db, manifest.queries, "delete", { id: 1 }, { userId: 7 }, recipients, authority.databaseId, undefined,
      () => {}, manifest.manifestVersion)).rejects.toThrow();
    expect((await db.execute("select id from notes where id = 1")).rows).toEqual([{ id: 1 }]);
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(2);
  });
});

test("named mutations suppress fenced hints without matching current manifest evidence", async () => {
  await replacementDatabase(async ({ db, manifest, authority }) => {
    const sent = [];
    const stale = new Map([["stale", { session: { userId: 7 }, fence: {
      ...authority, manifest: "stale-manifest", databaseEpoch: "e1",
    } }]]);
    await runWithSync(db, manifest.queries, "noop", {}, { userId: 7 }, stale, authority.databaseId, undefined,
      (id, message) => sent.push([id, message]), manifest.manifestVersion);

    const matching = new Map([["matching", { session: { userId: 7 }, fence: {
      ...authority, databaseEpoch: "e1",
    } }], ["s1", { session: { userId: 7 } }]]);
    await runWithSync(db, manifest.queries, "noop", {}, { userId: 7 }, matching, authority.databaseId, undefined,
      (id, message) => sent.push([id, message]));

    expect(sent).toEqual([["s1", expect.objectContaining({ type: "delta", serverRevision: 2 })]]);
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(2);
  });
});

test("replacement requires the manifest contract and restores its captured schema across cache refresh", async () => {
  await replacementDatabase(async ({ db, manifest, authority, request, replace }) => {
    const transaction = db.transaction.bind(db);
    db.transaction = mock(transaction);
    for (const replacementContracts of [undefined, {}, { Main: "" }, { Main: "different-contract" }, { Other: "contract-1" }]) {
      expect(await catchupReplacement(db, { ...manifest, replacementContracts }, authority, request, { userId: 7 }))
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
    const send = mock(() => {});
    const result = await runWithSync(db, manifest.queries, "noop", {}, { userId: 7 }, undefined, authority.databaseId, undefined, send);
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(1);
    expect(await result.sync(() => {})).toEqual({ databaseEpoch: "e1", serverRevision: 1 });
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

test("named mutation publication is required and follows commit order", async () => {
  await replacementDatabase(async ({ db, manifest, authority }) => {
    await expect(runWithSync(db, manifest.queries, "noop", {}, { userId: 7 }, undefined, authority.databaseId))
      .rejects.toThrow("requires sendToSession");
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(0);

    const fence = { ...authority, databaseEpoch: "e1" };
    const recipients = new Map([["reader", { session: { userId: 7 }, fence }]]);
    const published: number[] = [];
    let finishFirst!: () => void;
    const firstDelivery = new Promise<void>(resolve => { finishFirst = resolve; });
    const publish = (_id, message) => {
      published.push(message.serverRevision);
      if (message.serverRevision === 1) return firstDelivery;
    };
    const firstPromise = runWithSync(db, manifest.queries, "noop", {}, { userId: 7 }, recipients, authority.databaseId, undefined,
      publish, manifest.manifestVersion);
    const secondPromise = runWithSync(db, manifest.queries, "noop", {}, { userId: 7 }, recipients, authority.databaseId, undefined,
      publish, manifest.manifestVersion);
    while (published.length === 0) await Bun.sleep(0);
    await Bun.sleep(0);
    expect(published).toEqual([1]);
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(1);
    finishFirst();
    const [first, second] = await Promise.all([firstPromise, secondPromise]);

    expect(published).toEqual([1, 2]);
    expect(first.response.serverRevision).toBe(1);
    expect(second.response.serverRevision).toBe(2);
    expect((await first.sync(() => {})).serverRevision).toBe(1);
    expect((await second.sync(() => {})).serverRevision).toBe(2);
  });
});

test("queued named mutations capture input and execution session at invocation", async () => {
  await replacementDatabase(async ({ db, manifest, authority }) => {
    const fence = { ...authority, databaseEpoch: "e1" };
    const recipients = new Map([["reader", { session: { userId: 7 }, fence }]]);
    let finishFirst!: () => void;
    const firstDelivery = new Promise<void>(resolve => { finishFirst = resolve; });
    const publish = (_id, message) => message.serverRevision === 1 ? firstDelivery : undefined;
    const first = runWithSync(db, manifest.queries, "noop", {}, { userId: 7 }, recipients, authority.databaseId, undefined,
      publish, manifest.manifestVersion);
    const input = { id: 1, body: "captured" };
    const update = runWithSync(db, manifest.queries, "update", input, { userId: 7 }, recipients, authority.databaseId, undefined,
      publish, manifest.manifestVersion);
    const session = { userId: 7 };
    const revoke = runWithSync(db, manifest.queries, "revoke", {}, session, recipients, authority.databaseId, undefined,
      publish, manifest.manifestVersion);
    input.id = 3;
    input.body = "mutated";
    session.userId = 8;

    while ((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision === 0) await Bun.sleep(0);
    finishFirst();
    await Promise.all([first, update, revoke]);

    expect((await db.execute("select body from notes where id = 1")).rows[0].body).toBe("captured");
    expect((await db.execute("select body from notes where id = 3")).rows[0].body).toBe("private");
    expect((await db.execute("select * from memberships")).rows).toEqual([]);
  });
});

test("named sync executes actual generated SQL, retains declared rows, and never sends deltas to fenced readers", async () => {
  await replacementDatabase(async ({ db, authority }) => {
    await db.execute("create table entries(id text primary key, release text, enabled integer, count integer, role text, details blob, updatedAt integer)");
    await persistAuthority(db, authority.databaseId, compiledSource);
    const query = { ...compiledCreateMetadata, sql: compiledCreateSql, syncSql: compiledCreateSyncSql };
    const input = { id: createUuidV7, release: "release", enabled: true, count: 1, role: { _type: "Member" }, details: { _type: "Note", count: 2, enabled: false } };
    const fence = { ...authority, namespace: query.primary_db, databaseEpoch: "e1", instance: "other-tab", authGeneration: 8 };
    const sent = [];
    const result = await runWithSync(db, { [query.id]: query }, query.id, input,
      { userId: 7, role: { _type: "Member" }, unrelated: "required" },
      new Map([["reader", { session: { userId: 9 }, fence }]]), authority.databaseId, undefined,
      (id, message) => sent.push(message), authority.manifest);
    expect(result.kind).toBe("success");
    expect(result.response.result.entry[0]).toMatchObject(input);
    const original = structuredClone(result.response.result);
    await result.sync(() => {});
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

test("memory batch sync rejects without detaching or publishing, including empty batches", async () => {
  const db = createClient({ url: "file::memory:" });
  try {
    await db.execute("create table notes(id integer primary key)");
    await db.execute("insert into notes values(1)");
    await db.execute("create table _pyre_sync(id integer primary key, database_epoch text, server_revision integer)");
    await db.execute("insert into _pyre_sync values(1,'e1',0)");
    await persistAuthority(db, "memory-sync");
    const manifest = { ...manifestAuthority, version: 1, manifestVersion: "m1", SessionValidator: z.object({}), queries: {
      create: { ...queryAuthority, id: "create", operation: "insert", InputValidator: z.object({ id: z.string() }), SessionValidator: z.object({}),
        generatedEdit: { kind: "create", createUuidInput: "id", writableInputs: ["id"], writeStatementIndices: [0] },
        session_args: [], optional_input_args: [], json_input_args: [],
        sql: [{ include: true, params: [], sql: "insert into notes default values returning id as _pyreEditId" }],
      },
    } };
    const authority = { databaseId: "memory-sync", namespace: "Main", manifest: "m1", instance: "tab-1", authGeneration: 2 };
    const request = { version: 1, ...authority, databaseEpoch: "e1", requestId: "request-1", sequence: 1, operations: [{ operation: "create", input: { id: createUuidV7 } }] };
    const send = mock(() => {});
    const recipients = new Map([["subscriber", { session: {} }]]);
    expect(await runBatchWithSync(db, manifest, authority, request, {}, recipients, send))
      .toEqual({ kind: "error", error: { errorType: "TransactionFailed", message: "TransactionFailed" } });
    expect(await runBatchWithSync(db, manifest, authority, { ...request, operations: [] }, {}, recipients, send))
      .toEqual({ kind: "error", error: { errorType: "TransactionFailed", message: "TransactionFailed" } });
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
  const source = introspectionResult.schema_source;
  const executedSql: string[] = [];
  const affectedRows = {
      columns: ["_affectedRows"],
      rows: [{
        _affectedRows: JSON.stringify([{
          table_name: "maps",
          headers: ["id", "name", "tiling", "tiling__tileRootKey", "tiling__tileWidth", "tiling__format"],
          rows: [[1, "World", "Tiling", "tiles/root", 256, "Png"]],
        }]),
      }],
    };
  const execute = mock(async (statement: any) => {
      const sql = typeof statement === "string" ? statement : statement.sql;
      executedSql.push(sql);
      if (sql === "select cast(1 as integer) as _pyre_integer_mode") return { rows: [{ _pyre_integer_mode: 1 }] };
      if (sql.startsWith("SELECT schema")) return { rows: [{ schema: source }] };
      if (sql === "select _affectedRows") return affectedRows;
      if (sql.includes("returning database_epoch, server_revision")) {
        revision += 1;
        return { rows: [{ database_epoch: "test-epoch", server_revision: revision }] };
      }

      return { rows: [] };
    });
  const tx = { execute, commit: mock(async () => {}), rollback: mock(async () => {}), close: mock(() => {}) };
  return {
    execute,
    transaction: mock(async () => tx),
    executedSql,
  };
}

const queryMap = {
  "query-id": {
    ...queryAuthority,
    operation: "transaction",
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

  const sent: Array<{ sessionId: string; message: unknown }> = [];
  const result = await runWithSync(
    syncDb() as any,
    queryMap,
    "query-id",
    {},
    {},
    new Map([["s1", { session: {} }]]),
    undefined,
    undefined,
    (sessionId, message) => { sent.push({ sessionId, message }); },
  );

  expect(result.kind).toBe("success");

  const syncResult = await result.sync((sessionId, message) => { sent.push({ sessionId, message }); });

  expect(syncResult.serverRevision).toBe(1);
  expect(deltaSchemas.map(schema => schema.schema_source)).toEqual(["test schema"]);
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

test("unverified publication emits only hints for every legacy recipient and the origin", async () => {
  await loadSchemaFromDatabase(schemaDb as any);
  const db = syncDb();
  const sent: any[] = [];
  // Undeclared legacy operations have no transaction-bound revision proof.
  const result = await runWithSync(db as any,
    { "query-id": { ...queryMap["query-id"], operation: undefined } }, "query-id", {}, {},
    new Map(["origin", "s1", "other"].map(id => [id, { session: {} }])), undefined, "origin");
  const revision = await result.sync((id, message) => { sent.push([id, message]); });
  const hint = { type: "syncRequired", serverRevision: 1, databaseEpoch: "test-epoch" };
  expect(sent).toEqual([["s1", hint], ["other", hint]]);
  expect(revision.originMessage).toEqual(hint);
  expect(result.response).toEqual({ serverRevision: 1, databaseEpoch: "test-epoch", sync: hint, result: {} });
  expect(deltaSchemas).toEqual([]);
});

test("runWithSync stamps sync deltas with databaseId", async () => {
  introspectionResult = { schema_source: "campaign schema" };
  await loadSchemaFromDatabase("campaign:123", schemaDb as any);

  const sent: Array<{ sessionId: string; message: any }> = [];
  const result = await runWithSync(
    syncDb() as any,
    { "query-id": { ...queryMap["query-id"], schemaContracts: { Main: "campaign-contract" } } },
    "query-id",
    {},
    {},
    new Map([["s1", { session: {} }]]),
    "campaign:123",
    undefined,
    (sessionId, message) => { sent.push({ sessionId, message }); },
  );

  await result.sync((sessionId, message) => { sent.push({ sessionId, message }); });

  expect(sent[0].message.databaseId).toBe("campaign:123");
  expect(sent[0].message.type).toBe("delta");
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
    undefined,
    undefined,
    () => {},
  );

  await result.sync(() => {});

  expect(db.executedSql).toEqual([
    "select cast(1 as integer) as _pyre_integer_mode",
    expect.stringContaining('SELECT schema FROM "main"._pyre_migrations'),
    "select _affectedRows",
    expect.stringContaining("update _pyre_sync"),
  ]);
});

test("runWithSync allocates a revision even with no live recipients", async () => {
  sessionIds = [];
  await loadSchemaFromDatabase(schemaDb as any);

  const sent: Array<{ sessionId: string; message: unknown }> = [];
  const result = await runWithSync(
    syncDb() as any,
    queryMap,
    "query-id",
    {},
    {},
    new Map(),
    undefined,
    undefined,
    (sessionId, message) => { sent.push({ sessionId, message }); },
  );

  const syncResult = await result.sync((sessionId, message) => { sent.push({ sessionId, message }); });

  expect(sent).toHaveLength(0);
  expect(syncResult.serverRevision).toBe(1);
});

test("runWithSync skips the origin session when provided", async () => {
  sessionIds = ["s1", "s2"];
  await loadSchemaFromDatabase(schemaDb as any);

  const sent: Array<{ sessionId: string; message: unknown }> = [];
  const result = await runWithSync(
    syncDb() as any,
    queryMap,
    "query-id",
    {},
    {},
    new Map(sessionIds.map((sessionId) => [sessionId, { session: {} }])),
    undefined,
    "s1",
    (sessionId, message) => { sent.push({ sessionId, message }); },
  );

  await result.sync((sessionId, message) => { sent.push({ sessionId, message }); });

  expect(sent.map((entry) => entry.sessionId)).toEqual(["s2"]);
});

test("runWithSync includes origin authoritative sync in mutation response envelope", async () => {
  sessionIds = ["s1", "s2"];
  await loadSchemaFromDatabase("campaign:123", schemaDb as any);

  const sent: Array<{ sessionId: string; message: any }> = [];
  const result = await runWithSync(
    syncDb() as any,
    queryMap,
    "query-id",
    {},
    {},
    new Map(sessionIds.map((sessionId) => [sessionId, { session: {} }])),
    "campaign:123",
    "s1",
    (sessionId, message) => { sent.push({ sessionId, message }); },
  );

  await result.sync((sessionId, message) => { sent.push({ sessionId, message }); });

  expect(sent.map((entry) => entry.sessionId)).toEqual(["s2"]);
  expect((result.response as any).serverRevision).toBe(1);
  expect((result.response as any).sync).toEqual(sent[0].message);
  expect((result.response as any).sync.databaseId).toBe("campaign:123");
  expect((result.response as any).sync.type).toBe("delta");
});

test("runWithSync builds origin sync from executing session when origin is not live-connected", async () => {
  sessionIds = ["s1"];
  await loadSchemaFromDatabase("campaign:123", schemaDb as any);

  const sent: Array<{ sessionId: string; message: any }> = [];
  const result = await runWithSync(
    syncDb() as any,
    queryMap,
    "query-id",
    {},
    {},
    new Map(),
    "campaign:123",
    "s1",
    (sessionId, message) => { sent.push({ sessionId, message }); },
  );

  await result.sync((sessionId, message) => { sent.push({ sessionId, message }); });

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

  const sent: Array<{ sessionId: string; message: any }> = [];
  const result = await runWithSync(
    syncDb() as any,
    queryMap,
    "query-id",
    {},
    {},
    new Map([["s1", { session: {} }]]),
    undefined,
    undefined,
    (sessionId, message) => { sent.push({ sessionId, message }); },
  );

  await result.sync((sessionId, message) => { sent.push({ sessionId, message }); });

  expect(sent).toHaveLength(1);
  expect(sent[0].sessionId).toBe("s1");
  expect(sent[0].message.type).toBe("syncRequired");
  expect(deltaSchemas).toHaveLength(1);
  expect(typeof sent[0].message.serverRevision).toBe("number");
});

test("runWithSync sends syncRequired when fanout recipient count exceeds cap", async () => {
  sessionIds = Array.from({ length: MAX_LIVE_SYNC_FANOUT_RECIPIENTS + 1 }, (_, index) => `s${index}`);
  await loadSchemaFromDatabase(schemaDb as any);

  const sent: Array<{ sessionId: string; message: any }> = [];
  const result = await runWithSync(
    syncDb() as any,
    queryMap,
    "query-id",
    {},
    {},
    new Map(sessionIds.map((sessionId) => [sessionId, { session: {} }])),
    undefined,
    undefined,
    (sessionId, message) => { sent.push({ sessionId, message }); },
  );

  await result.sync((sessionId, message) => { sent.push({ sessionId, message }); });

  expect(sent).toHaveLength(MAX_LIVE_SYNC_FANOUT_RECIPIENTS + 1);
  expect(sent.every((entry) => entry.message.type === "syncRequired")).toBe(true);
  expect(deltaSchemas).toHaveLength(1);
});

test("runWithSync sends syncRequired when payload bytes exceed cap", async () => {
  reshapedRows = [[1, "x".repeat(MAX_LIVE_SYNC_DELTA_PAYLOAD_BYTES), null]];
  await loadSchemaFromDatabase("campaign:123", schemaDb as any);

  const sent: Array<{ sessionId: string; message: any }> = [];
  const result = await runWithSync(
    syncDb() as any,
    queryMap,
    "query-id",
    {},
    {},
    new Map([["s1", { session: {} }]]),
    "campaign:123",
    undefined,
    (sessionId, message) => { sent.push({ sessionId, message }); },
  );

  await result.sync((sessionId, message) => { sent.push({ sessionId, message }); });

  expect(sent).toHaveLength(1);
  expect(sent[0].sessionId).toBe("s1");
  expect(sent[0].message.type).toBe("syncRequired");
  expect(sent[0].message.databaseId).toBe("campaign:123");
  expect(typeof sent[0].message.serverRevision).toBe("number");
  expect(deltaSchemas).toHaveLength(1);
});

test("runWithSync advances revision and requires catchup when delta calculation fails", async () => {
  deltaError = "Error: invalid connected session";
  await loadSchemaFromDatabase(schemaDb as any);
  const db = syncDb();
  const sent: Array<{ sessionId: string; message: any }> = [];
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
    (sessionId, message) => { sent.push({ sessionId, message }); },
  );
  const syncResult = await result.sync((sessionId, message) => { sent.push({ sessionId, message }); });

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

test("runWithSync requires revisioned catchup for reshape failures, including the origin", async () => {
  await replacementDatabase(async ({ db, manifest, authority }) => {
    sessionIds = ["origin", "first", "second"];
    reshapeError = "Error: invalid table groups";
    const sent: Array<{ sessionId: string; message: any }> = [];
    const result = await runWithSync(
      db,
      manifest.queries,
      "noop",
      {},
      { userId: 7 },
      new Map(sessionIds.map((sessionId) => [sessionId, { session: { userId: 7 } }])),
      authority.databaseId,
      "origin",
      (sessionId, message) => { sent.push({ sessionId, message }); },
    );
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(1);
    const syncResult = await result.sync(() => {});
    const fallback = {
      type: "syncRequired",
      serverRevision: 1,
      databaseEpoch: "e1",
      databaseId: authority.databaseId,
    };

    expect(sent).toEqual([
      { sessionId: "first", message: fallback },
      { sessionId: "second", message: fallback },
    ]);
    expect(syncResult.originMessage).toEqual(fallback);
    expect((result.response as any).sync).toEqual(fallback);
  });
});

test("runWithSync isolates thrown and rejected recipient sends during fanout", async () => {
  await replacementDatabase(async ({ db, manifest, authority }) => {
    sessionIds = ["throws", "rejects", "later"];
    const sent: Array<{ sessionId: string; message: any }> = [];
    const result = await runWithSync(
      db,
      manifest.queries,
      "noop",
      {},
      { userId: 7 },
      new Map(sessionIds.map((sessionId) => [sessionId, { session: { userId: 7 } }])),
      authority.databaseId,
      undefined,
      (sessionId, message) => {
        if (sessionId === "throws") throw new Error("disconnected");
        if (sessionId === "rejects") return Promise.reject(new Error("closed asynchronously"));
        sent.push({ sessionId, message });
      },
    );
    const syncResult = await result.sync(() => {});

    expect(syncResult.serverRevision).toBe(1);
    expect(sent).toEqual([{
      sessionId: "later",
      message: expect.objectContaining({ type: "delta", serverRevision: 1, databaseEpoch: "e1" }),
    }]);
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(1);
  });
});
