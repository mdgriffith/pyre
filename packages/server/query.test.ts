import { expect, mock, test } from "bun:test";
import { createClient } from "@libsql/client";
import { z } from "zod";
import { run, seed } from "./query";
import { toRunner } from "./runtime/runner";
import { buildArgs, toSqlStatements } from "./runtime/sql";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ensureDatabase } from "./schema";
import { initSync } from "./wasm/pyre_wasm.js";

const fixtureFile = (name: string) => readFileSync(new URL(`../../tests/fixtures/seed/${name}`, import.meta.url), "utf8");
const fixtureSchema = fixtureFile("schema.pyre");

function seedDatabase(intMode: "number" | "bigint" = "number") {
  initSync({ module: readFileSync(new URL("./wasm/pyre_wasm_bg.wasm", import.meta.url)) });
  const directory = mkdtempSync(join(tmpdir(), "pyre-fixture-"));
  const db = createClient({ url: `file:${join(directory, "test.db")}`, intMode });
  return { db, close() { db.close(); rmSync(directory, { recursive: true, force: true }); } };
}

test("WASM seed matches the native fixture and persists both directions of nested links", async () => {
  const { db, close } = seedDatabase();
  try {
    await ensureDatabase(db, "Fixtures", fixtureSchema);
    const input = JSON.parse(fixtureFile("input.json"));
    expect(await seed(db, input, { namespace: "Wrong" })).toMatchObject({
      kind: "error", error: { message: expect.stringContaining("found 'Fixtures'") },
    });
    expect(await seed(db, input, { namespace: "Fixtures" })).toEqual({
      kind: "success", response: JSON.parse(fixtureFile("expected.json")),
    });
    expect((await db.execute("select id, authorId from posts order by id")).rows.map(row => [row.id, row.authorId]))
      .toEqual([[4, 1], [5, 2]]);
    // Replaying the explicit post ID fails, including the to-one author inserted first.
    expect(await seed(db, input)).toMatchObject({ kind: "error", error: { errorType: "DatabaseError", message: expect.stringContaining("posts[0]") } });
    expect((await db.execute("select count(*) as n from users")).rows[0].n).toBe(2);
  } finally { close(); }
});

test("seed rejects an uninitialized target and rolls back late validation errors", async () => {
  const { db, close } = seedDatabase();
  try {
    expect(await seed(db, {})).toMatchObject({ kind: "error", error: { message: expect.stringContaining("initialized") } });
    expect((await db.execute("select name from sqlite_schema")).rows).toHaveLength(0);
    await ensureDatabase(db, "Fixtures", fixtureSchema);
    const result = await seed(db, { users: [{ name: "Discard", posts: [{ title: null }] }] });
    expect(result).toMatchObject({ kind: "error", error: { errorType: "InvalidInput", message: expect.stringContaining("users[0].posts[0].title") } });
    expect((await db.execute("select count(*) as n from users")).rows[0].n).toBe(0);
  } finally { close(); }
});

test("WASM seed preserves exact returned integer keys and generates UUIDv7 without assigning sequences", async () => {
  const { db, close } = seedDatabase("bigint");
  try {
    await ensureDatabase(db, "Fixtures", `${fixtureSchema}
record Event {
    key Id.Uuid @id
    sequence Sequence.Int
    payload String
    @public
}
`);
    await db.execute("insert into users (id, name) values (9007199254740992, 'Existing')");
    expect(await seed(db, { users: [{ posts: [{ title: "Nested", metadata: {}, status: "Draft" }] }] })).toMatchObject({ kind: "success" });
    expect((await db.execute("select authorId from posts")).rows[0].authorId).toBe(9007199254740993n);
    const result = await seed(db, { events: [{ payload: "First" }] });
    expect(result).toMatchObject({ kind: "success", response: { events: [{
      key: expect.stringMatching(/^[\da-f]{8}-[\da-f]{4}-7[\da-f]{3}-[89ab][\da-f]{3}-[\da-f]{12}$/), sequence: 1,
    }] } });
    expect(await seed(db, { events: [{ payload: "Forged", sequence: 99 }] })).toMatchObject({
      kind: "error", error: { errorType: "InvalidInput", message: expect.stringContaining("events[0].sequence") },
    });
  } finally { close(); }
});

test("seed reports uncertain commit and rollback and rejects non-atomic transaction modes", async () => {
  const { db, close } = seedDatabase();
  try {
    await ensureDatabase(db, "Fixtures", fixtureSchema);
    const client = (failure: "commit" | "rollback") => ({
      transaction: async () => {
        const tx = await db.transaction("write");
        return {
          get closed() { return tx.closed; },
          execute: tx.execute.bind(tx),
          close: tx.close.bind(tx),
          commit: failure === "commit" ? async () => { await tx.commit(); throw new Error("lost response"); } : tx.commit.bind(tx),
          rollback: failure === "rollback" ? async () => { throw new Error("lost response"); } : tx.rollback.bind(tx),
        };
      },
    }) as unknown as typeof db;
    expect(await seed(client("commit"), { users: [{}] })).toMatchObject({
      kind: "error", error: { errorType: "OutcomeUnknown", message: expect.stringContaining("commit outcome unknown") },
    });
    expect(await seed(client("rollback"), { users: [{}, { missing: 1 }] })).toMatchObject({
      kind: "error", error: { errorType: "OutcomeUnknown", message: expect.stringContaining("rollback outcome unknown") },
    });
    for (const transactionMode of ["batch", "none"] as const) {
      expect(await seed(db, {}, { transactionMode })).toMatchObject({ kind: "error", error: { errorType: "InvalidInput" } });
    }
  } finally { close(); }
});

test("composed operations commit once, retain indexed results, and publish only final row authority", async () => {
  const directory = mkdtempSync(join(tmpdir(), "pyre-batch-"));
  const db = createClient({ url: `file:${join(directory, "test.db")}` });
  try {
    await db.batch([
      "create table notes (id integer primary key, owner integer, body text unique)",
      "insert into notes values (1, 7, 'original')",
      "create table _pyre_sync (id integer primary key, database_epoch text, server_revision integer)",
      "insert into _pyre_sync values (1, 'epoch', 0)",
    ]);
    const sql = [
      { include: false, params: ["id", "body", "session_userId"], sql: "update notes set body = $body where id = $id and owner = $session_userId" },
      { include: true, params: ["id"], sql: "select json_object('id', id, 'body', body) as note from notes where id = $id" },
      { include: true, params: ["id"], sql: "select json_array(json_object('table_name', 'notes', 'headers', json_array('id', 'body'), 'rows', json_array(json_array(id, body)))) as _affectedRows from notes where id = $id" },
    ];
    const queries = { edit: {
      id: "edit", operation: "update", primary_db: "Main", sql, syncSql: sql,
      syncEffects: { sql: true, syncSql: true },
      generatedEdit: { writeStatement: 0 },
      session_args: ["userId"], optional_input_args: [], json_input_args: [],
      InputValidator: z.object({ id: z.number(), body: z.string() }).strict(),
      SessionValidator: z.object({ userId: z.number() }),
    } };
    const publish = mock(async (groups: any[], _sessions: any, _send: any, _origin: any, revision: any) => {
      expect((await db.execute("select body from notes")).rows[0].body).toBe("last");
      expect(groups).toEqual([{ table_name: "notes", headers: ["id", "body"], rows: [[1, "last"]] }]);
      return revision;
    });
    const execute = (operations: any, session = { userId: 7 }) => run(db, queries, operations, undefined, session, new Map(), publish, undefined, { mode: "sync", allocateSyncRevision: true });
    const success = await execute([
      { queryId: "edit", input: { id: 1, body: "first" } },
      { queryId: "edit", input: { id: 1, body: "last" } },
    ]);
    expect(success.kind).toBe("success");
    expect(success.response).toEqual([
      { index: 0, queryId: "edit", result: { note: [{ id: 1, body: "first" }] } },
      { index: 1, queryId: "edit", result: { note: [{ id: 1, body: "last" }] } },
    ]);
    expect(publish).not.toHaveBeenCalled();
    expect(await success.sync(() => {})).toEqual({ databaseEpoch: "epoch", serverRevision: 1 });
    await success.sync(() => {});
    expect(publish).toHaveBeenCalledTimes(1);

    const rejected = await execute([
      { queryId: "edit", input: { id: 1, body: "must roll back" } },
      { queryId: "edit", input: { id: 2, body: "missing" } },
      // This later successful write and revision allocation execute before the
      // application checks counts. Neither may survive the rejected middle edit.
      { queryId: "edit", input: { id: 1, body: "also must roll back" } },
    ]);
    expect(rejected.error).toMatchObject({ errorType: "TransactionFailed", operationIndex: 1 });
    await rejected.sync(() => {});
    expect((await db.execute("select body from notes")).rows).toEqual([{ body: "last" }]);
    expect((await db.execute("select server_revision from _pyre_sync")).rows).toEqual([{ server_revision: 1 }]);
    expect(publish).toHaveBeenCalledTimes(1);
    expect((await execute([{ queryId: "edit", input: { id: 1, body: "denied" } }], { userId: 9 })).kind).toBe("error");
    expect((await execute([{ queryId: "edit", input: { id: 1, body: 12 } }])).error?.errorType).toBe("InvalidInput");
    expect((await execute([{ queryId: "toString", input: {} }])).error?.errorType).toBe("UnknownQuery");
    expect((await execute([])).response).toEqual([]);

    await db.execute("insert into notes values (2, 7, 'taken')");
    const constraint = await execute([
      { queryId: "edit", input: { id: 1, body: "temporary" } },
      { queryId: "edit", input: { id: 2, body: "temporary" } },
    ]);
    // The adapter's batch SQL error does not identify its failing statement.
    expect(constraint.error).toMatchObject({ errorType: "TransactionFailed" });
    expect(constraint.error?.operationIndex).toBeUndefined();
    expect((await db.execute("select body from notes order by id")).rows).toEqual([{ body: "last" }, { body: "taken" }]);

    const broad = { ...queries.edit, sql: [{ include: false, params: [], sql: "update notes set owner = 8" }], syncSql: undefined };
    const overbroad = await run(db, { broad }, [{ queryId: "broad", input: { id: 1, body: "x" } }], undefined, { userId: 7 });
    expect(overbroad.kind).toBe("error");
    expect((await db.execute("select distinct owner from notes")).rows).toEqual([{ owner: 7 }]);
    const named = await run(db, { broad: { ...broad, generatedEdit: undefined } }, [{ queryId: "broad", input: { id: 1, body: "x" } }], undefined, { userId: 7 });
    expect(named.kind).toBe("success");
    expect((await db.execute("select distinct owner from notes")).rows).toEqual([{ owner: 8 }]);
    await db.execute("update notes set owner = 7");

    await db.execute("delete from _pyre_sync");
    const missingRevision = await execute([{ queryId: "edit", input: { id: 1, body: "must not commit" } }]);
    expect(missingRevision.kind).toBe("error");
    expect((await db.execute("select body from notes where id = 1")).rows[0].body).toBe("last");
  } finally {
    db.close();
    rmSync(directory, { recursive: true, force: true });
  }
});

test("revision allocation follows active compiler effects, not SQL literals", async () => {
  const directory = mkdtempSync(join(tmpdir(), "pyre-effects-"));
  const db = createClient({ url: `file:${join(directory, "test.db")}` });
  try {
    await db.batch([
      "create table _pyre_sync (id integer primary key, database_epoch text, server_revision integer)",
      "insert into _pyre_sync values (1, 'epoch', 0)",
      "create table notes (body text)",
    ]);
    const base = {
      id: "query", session_args: [], optional_input_args: [], json_input_args: [],
      InputValidator: z.object({}), SessionValidator: z.object({}),
    };
    const read = { ...base, sql: [{ include: true, params: [], sql: "select json_quote('_affectedRows') as literal" }], syncEffects: { sql: false, syncSql: false } };
    const write = {
      ...base,
      sql: [{ include: false, params: [], sql: "insert into notes values ('normal')" }],
      syncSql: [{ include: false, params: [], sql: "insert into notes values ('sync')" }],
      syncEffects: { sql: false, syncSql: true },
    };
    const revision = async () => Number((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision);
    for (const composed of [false, true]) {
      const execute = (query: typeof read | typeof write, mode: "sync" | "normal") => run(
        db, { query }, composed ? [{ queryId: "query", input: {} }] : "query", {}, {},
        undefined, undefined, undefined, { mode, allocateSyncRevision: true },
      );
      const before = await revision();
      expect((await execute(read, "sync")).kind).toBe("success");
      expect(await revision()).toBe(before);
      expect((await execute(write, "normal")).kind).toBe("success");
      expect(await revision()).toBe(before);
      expect((await execute(write, "sync")).kind).toBe("success");
      expect(await revision()).toBe(before + 1);
      // A sync request can fall back to the normal SQL variant.
      expect((await execute({ ...read, sql: write.syncSql, syncEffects: { sql: true, syncSql: false } }, "sync")).kind).toBe("success");
      expect(await revision()).toBe(before + 2);
    }
  } finally {
    db.close();
    rmSync(directory, { recursive: true, force: true });
  }
});

test("composed execution rejects mismatched namespaces before opening a transaction", async () => {
  const operation = {
    id: "query", sql: [], session_args: [], optional_input_args: [], json_input_args: [],
    InputValidator: z.object({}), SessionValidator: z.object({}),
  };
  const db = { transaction: mock(() => { throw new Error("must not execute"); }) };
  const result = await run(db as any, { a: { ...operation, primary_db: "A" }, b: { ...operation, primary_db: "B" } },
    [{ queryId: "a", input: {} }, { queryId: "b", input: {} }], undefined, {});
  expect(result.error).toMatchObject({ errorType: "InvalidInput", operationIndex: 1 });
  expect(db.transaction).not.toHaveBeenCalled();
  expect((await run(db as any, {}, [], undefined, {})).response).toEqual([]);
  expect(db.transaction).not.toHaveBeenCalled();
});

test("generated UUID creates reject noncanonical identity before execution", async () => {
  const db = { transaction: mock(() => { throw new Error('must not execute'); }) };
  const query = {
    id: 'create', sql: [], session_args: [], optional_input_args: [], json_input_args: [],
    generatedEdit: { writeStatement: 0, createId: 'id' },
    InputValidator: z.object({ id: z.string() }).strict(), SessionValidator: z.object({}),
  };
  for (const id of [undefined, '01900000-0000-4000-8000-000000000000', '019ABCDE-0000-7000-8000-000000000000', '01900000-0000-7000-0000-000000000000']) {
    const result = await run(db as any, { create: query }, 'create', { id }, {});
    expect(result.error?.errorType).toBe('InvalidInput');
  }
  expect(db.transaction).not.toHaveBeenCalled();
});

test("failed commit reports unknown outcome rather than a safe-to-retry rejection", async () => {
  const tx = {
    batch: async () => [],
    commit: async () => { throw new Error("connection lost"); },
    rollback: async () => {}, close() {}, closed: false,
  };
  const result = await run({ transaction: async () => tx } as any, { command: {
    id: "command", sql: [], session_args: [], optional_input_args: [], json_input_args: [],
    InputValidator: z.object({}), SessionValidator: z.object({}),
  } }, [{ queryId: "command", input: {} }], undefined, {});
  expect(result.error?.errorType).toBe("OutcomeUnknown");
});

test("transaction runner executes every step in exactly one ordered batch", async () => {
  const db = {
    batch: mock(async () => [
      {
        columns: ["updatedNotes"],
        rows: [{ updatedNotes: JSON.stringify({ id: 1, body: "updated" }) }],
      },
      {
        columns: ["missingNotes"],
        rows: [{ missingNotes: JSON.stringify([]) }],
      },
      {
        columns: ["createdNotes"],
        rows: [{ createdNotes: JSON.stringify({ id: 2, body: "created" }) }],
      },
    ]),
  };
  const sql = [
    {
      include: true,
      params: ["body", "session_userId"],
      sql: "update notes returning updatedNotes",
    },
    {
      include: true,
      params: ["session_userId"],
      sql: "delete from notes returning missingNotes",
    },
    {
      include: true,
      params: ["body", "session_userId"],
      sql: "insert into notes returning createdNotes",
    },
  ];
  const runner = toRunner(
    {
      session_args: ["userId"],
      optional_input_args: [],
      json_input_args: [],
      InputValidator: z.object({ body: z.string() }),
      SessionValidator: z.object({ userId: z.number() }),
      ReturnData: z.object({
        updatedNotes: z.array(z.object({ id: z.number(), body: z.string() })),
        missingNotes: z.array(z.object({ id: z.number(), body: z.string() })),
        createdNotes: z.array(z.object({ id: z.number(), body: z.string() })),
      }),
    },
    sql,
  );

  const result = await runner(db as any, { userId: 7 }, { body: "updated" });

  expect(db.batch).toHaveBeenCalledTimes(1);
  expect(db.batch).toHaveBeenCalledWith([
    {
      sql: "update notes returning updatedNotes",
      args: { body: "updated", session_userId: 7 },
    },
    {
      sql: "delete from notes returning missingNotes",
      args: { session_userId: 7 },
    },
    {
      sql: "insert into notes returning createdNotes",
      args: { body: "updated", session_userId: 7 },
    },
  ]);
  expect(result).toEqual({
    updatedNotes: [{ id: 1, body: "updated" }],
    missingNotes: [],
    createdNotes: [{ id: 2, body: "created" }],
  });
});

test("failed transaction batch rolls back before sync publication", async () => {
  const db = createClient({ url: "file::memory:" });
  const syncDeltas = mock(async () => ({ serverRevision: 1 }));

  try {
    await db.execute("create table notes (id integer primary key, body text unique not null)");
    await db.execute({
      sql: "insert into notes (body) values (?)",
      args: ["taken"],
    });

    await expect(run(
      db,
      {
        createNotes: {
          id: "createNotes",
          sql: [],
          syncSql: [
            {
              include: false,
              params: [],
              sql: "insert into notes (body) values ('first')",
            },
            {
              include: false,
              params: [],
              sql: "insert into notes (body) values ('taken')",
            },
          ],
          session_args: [],
          optional_input_args: [],
          json_input_args: [],
          InputValidator: z.object({}),
          SessionValidator: z.object({}),
        },
      },
      "createNotes",
      {},
      {},
      new Map([["client", { session: {} }]]),
      syncDeltas,
      undefined,
      { mode: "sync" },
    )).rejects.toThrow();

    const rows = await db.execute("select body from notes order by id");
    expect(rows.rows).toEqual([{ body: "taken" }]);
    expect(syncDeltas).not.toHaveBeenCalled();
  } finally {
    db.close();
  }
});

test("sync wraps mutation responses with server revision metadata", async () => {
  const db = {
    batch: mock(async () => [
      {
        columns: ["createdNote"],
        rows: [{ createdNote: JSON.stringify({ id: 1, body: "one" }) }],
      },
      {
        columns: ["_affectedRows"],
        rows: [
          {
            _affectedRows: JSON.stringify([
              { table_name: "notes", headers: ["id"], rows: [[1]] },
            ]),
          },
        ],
      },
    ]),
  };

  const result = await run(
    db as any,
    {
      createNote: {
        id: "createNote",
        sql: [
          { include: true, params: [], sql: "select createdNote" },
          { include: true, params: [], sql: "select _affectedRows" },
        ],
        session_args: [],
        optional_input_args: [],
        json_input_args: [],
        InputValidator: z.object({}),
        SessionValidator: z.object({}),
      },
    },
    "createNote",
    {},
    {},
    new Map(),
    async () => ({ serverRevision: 42 }),
  );

  await result.sync(() => {});

  expect(result.response).toEqual({
    serverRevision: 42,
    result: {
      createdNote: [{ id: 1, body: "one" }],
    },
  });
});

test("sync mode includes the mutation result", async () => {
  const db = {
    batch: mock(async () => [
      {
        columns: ["createdNote"],
        rows: [{ createdNote: JSON.stringify({ id: 1, body: "one" }) }],
      },
      {
        columns: ["_affectedRows"],
        rows: [
          {
            _affectedRows: JSON.stringify([
              { table_name: "notes", headers: ["id"], rows: [[1]] },
            ]),
          },
        ],
      },
    ]),
  };

  const result = await run(
    db as any,
    {
      createNote: {
        id: "createNote",
        sql: [{ include: true, params: [], sql: "select createdNote" }],
        syncSql: [
          { include: true, params: [], sql: "select createdNote" },
          { include: true, params: [], sql: "select _affectedRows" },
        ],
        session_args: [],
        optional_input_args: [],
        json_input_args: [],
        InputValidator: z.object({}),
        SessionValidator: z.object({}),
      },
    },
    "createNote",
    {},
    {},
    new Map(),
    async () => ({ serverRevision: 42, originMessage: { type: "delta" } }),
    undefined,
    { mode: "sync" },
  );

  await result.sync(() => {});

  expect(result.response).toEqual({
    serverRevision: 42,
    sync: { type: "delta" },
    result: {
      createdNote: [{ id: 1, body: "one" }],
    },
  });
  expect(db.batch).toHaveBeenCalledWith([
    { sql: "select createdNote", args: {} },
    { sql: "select _affectedRows", args: {} },
  ]);
});

test("SQL args serialize Date values as unix seconds", () => {
  const date = new Date("2026-07-11T16:36:52.000Z");

  expect(
    buildArgs(
      { startedAt: date, payload: { direct: date, nested: [date] } },
      { visibleAfter: date },
      ["visibleAfter"],
      [],
      ["payload"],
    ),
  ).toEqual({
    startedAt: 1783787812,
    payload: JSON.stringify({ direct: 1783787812, nested: [1783787812] }),
    session_visibleAfter: 1783787812,
  });
});

test("SQL args preserve nullable JSON null as SQL null", () => {
  expect(buildArgs({ payload: null }, {}, [], [], ["payload"])).toEqual({
    payload: null,
  });
});

test("SQL args bind logical tagged-union sessions to physical paths", () => {
  const date = new Date("2026-07-11T16:36:52.000Z");

  expect(
    buildArgs(
      undefined,
      {
        scope: { _type: "Workspace", id: 7, privateData: { hidden: true } },
        accountId: 3,
        visibleAfter: date,
        roles: ["admin", "editor"],
        preferences: { theme: "dark", refreshedAt: date },
        account__id: 11,
      },
      [
        "scope",
        "scope__id",
        "scope__accountId",
        "accountId",
        "visibleAfter",
        "roles",
        "preferences",
        "nullableField",
        "account__id",
      ],
    ),
  ).toEqual({
    session_scope: "Workspace",
    session_scope__id: 7,
    session_scope__accountId: null,
    session_accountId: 3,
    session_visibleAfter: 1783787812,
    session_roles: JSON.stringify(["admin", "editor"]),
    session_preferences: JSON.stringify({ theme: "dark", refreshedAt: 1783787812 }),
    session_nullableField: null,
    session_account__id: 11,
  });
});

test("SQL statements bind every declared parameter", () => {
  expect(toSqlStatements(
    [{ include: true, params: ["present", "omitted"], sql: "select $present, $omitted" }],
    { present: 1 },
  )).toEqual([{
    sql: "select $present, $omitted",
    args: { present: 1, omitted: null },
  }]);
});

test("nullable session args always receive SQL bindings", async () => {
  const db = {
    batch: mock(async () => []),
  };
  const query = {
    findUsers: {
      id: "findUsers",
      sql: [{
        include: true,
        params: ["session_isAdmin", "session_userId"],
        sql: "select 1 where $session_userId is not null or $session_isAdmin = 1",
      }],
      session_args: ["isAdmin", "userId"],
      optional_input_args: [],
      json_input_args: [],
      InputValidator: z.object({}),
      SessionValidator: z.object({
        userId: z.number().nullish(),
        isAdmin: z.boolean(),
      }),
    },
  };

  for (const [session, expectedUserId] of [
    [{ isAdmin: true, userId: 42 }, 42],
    [{ isAdmin: true, userId: null }, null],
    [{ isAdmin: true }, null],
  ] as const) {
    const result = await run(
      db as any,
      query,
      "findUsers",
      {},
      session,
      new Map(),
      async () => ({}),
    );

    expect(result.kind).not.toBe("error");
    const statement = db.batch.mock.calls.at(-1)?.[0][0];
    expect(statement.args).toEqual({
      session_isAdmin: true,
      session_userId: expectedUserId,
    });
    expect(Object.keys(statement.args)).toHaveLength(statement.sql.match(/\$session_/g)?.length ?? 0);
  }

  expect(db.batch).toHaveBeenCalledTimes(3);
});

test("missing non-nullable session args fail validation before SQL execution", async () => {
  const db = {
    batch: mock(async () => []),
  };

  const result = await run(
    db as any,
    {
      findUsers: {
        id: "findUsers",
        sql: [{ include: true, params: ["session_isAdmin"], sql: "select $session_isAdmin" }],
        session_args: ["isAdmin"],
        optional_input_args: [],
        json_input_args: [],
        InputValidator: z.object({}),
        SessionValidator: z.object({ isAdmin: z.boolean() }),
      },
    },
    "findUsers",
    {},
    {},
    new Map(),
    async () => ({}),
  );

  expect(result.kind).toBe("error");
  expect(result.error?.errorType).toBe("InvalidSession");
  expect(db.batch).not.toHaveBeenCalled();
});

test("sync does not fan out when no affected rows are returned", async () => {
  const db = {
    batch: mock(async () => [
      {
        columns: ["_affectedRows"],
        rows: [{ _affectedRows: JSON.stringify([]) }],
      },
    ]),
  };
  const syncDeltas = mock(async () => ({ serverRevision: 42 }));

  const result = await run(
    db as any,
    {
      createNote: {
        id: "createNote",
        sql: [{ include: true, params: [], sql: "select _affectedRows" }],
        session_args: [],
        optional_input_args: [],
        json_input_args: [],
        InputValidator: z.object({}),
        SessionValidator: z.object({}),
      },
    },
    "createNote",
    {},
    {},
    new Map([["s1", { session: {} }]]),
    syncDeltas,
  );

  const sendToSession = mock(() => {});
  const syncResult = await result.sync(sendToSession);

  expect(syncResult).toEqual({});
  expect(syncDeltas).not.toHaveBeenCalled();
  expect(sendToSession).not.toHaveBeenCalled();
});
