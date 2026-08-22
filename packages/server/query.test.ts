import { expect, mock, test } from "bun:test";
import type { SchemaMetadata } from "@pyre/core";
import { createClient } from "@libsql/client";
import { z } from "zod";
import { run, seed } from "./query";
import { toRunner } from "./runtime/runner";
import { buildArgs, toSqlStatements } from "./runtime/sql";

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

test("seed inserts nested rows through schema links", async () => {
  const executed: any[] = [];
  const db = {
    execute: mock(async (statement: any) => {
      executed.push(statement);
      if (statement === "begin" || statement === "commit") {
        return { rows: [] };
      }
      if (
        typeof statement === "string" &&
        statement.startsWith("pragma table_info")
      ) {
        return {
          rows: [
            { name: "id" },
            { name: "name" },
            { name: "authorId" },
            { name: "title" },
          ],
        };
      }
      if (statement.sql.includes('"users"')) {
        return { rows: [{ id: 10, name: statement.args.seed_0 }] };
      }
      if (statement.sql.includes('"posts"')) {
        const values = Object.values(statement.args);
        const authorId = values.find((value) => value === 10);
        const title = values.find(
          (value) => value === "First" || value === "Second",
        );
        return { rows: [{ id: title === "First" ? 20 : 21, authorId, title }] };
      }
      throw new Error("unexpected statement");
    }),
  };

  const result = await seed(db as any, userPostSchema(), {
    users: [
      {
        name: "Fred",
        posts: [{ title: "First" }, { title: "Second" }],
      },
    ],
  });

  expect(result).toEqual({
    kind: "success",
    response: {
      users: [
        {
          id: 10,
          name: "Fred",
          posts: [
            { id: 20, authorId: 10, title: "First" },
            { id: 21, authorId: 10, title: "Second" },
          ],
        },
      ],
    },
  });
  expect(executed[0]).toBe("begin");
  expect(executed.at(-1)).toBe("commit");
  const postInserts = executed.filter(
    (statement) =>
      typeof statement !== "string" && statement.sql.includes('"posts"'),
  );
  expect(Object.values(postInserts[0].args)).toContain(10);
  expect(Object.values(postInserts[1].args)).toContain(10);
});

test("seed batches sibling inserts when batch is supported", async () => {
  const executed: any[] = [];
  const batched: any[][] = [];
  const db = {
    execute: mock(async (statement: any) => {
      executed.push(statement);
      if (
        typeof statement === "string" &&
        statement.startsWith("pragma table_info")
      ) {
        return {
          rows: [
            { name: "id" },
            { name: "name" },
            { name: "authorId" },
            { name: "title" },
          ],
        };
      }
      throw new Error("unexpected execute");
    }),
    batch: mock(async (statements: any[]) => {
      batched.push(statements);
      return statements.map((statement) => {
        if (statement.sql.includes('"users"')) {
          return { rows: [{ id: 10, name: statement.args.seed_0 }] };
        }

        const values = Object.values(statement.args);
        const authorId = values.find((value) => value === 10);
        const title = values.find(
          (value) => value === "First" || value === "Second",
        );
        return { rows: [{ id: title === "First" ? 20 : 21, authorId, title }] };
      });
    }),
  };

  const result = await seed(db as any, userPostSchema(), {
    users: [
      {
        name: "Fred",
        posts: [{ title: "First" }, { title: "Second" }],
      },
    ],
  });

  expect(result).toEqual({
    kind: "success",
    response: {
      users: [
        {
          id: 10,
          name: "Fred",
          posts: [
            { id: 20, authorId: 10, title: "First" },
            { id: 21, authorId: 10, title: "Second" },
          ],
        },
      ],
    },
  });
  expect(
    executed.every(
      (statement) =>
        typeof statement === "string" &&
        statement.startsWith("pragma table_info"),
    ),
  ).toBe(true);
  expect(executed).not.toContain("begin");
  expect(executed).not.toContain("commit");
  expect(batched).toHaveLength(2);
  expect(batched[0]).toHaveLength(1);
  expect(batched[1]).toHaveLength(2);
});

test("seed rejects nested foreign key conflicts", async () => {
  const db = {
    execute: mock(async (statement: any) => {
      if (statement === "begin" || statement === "rollback") {
        return { rows: [] };
      }
      if (
        typeof statement === "string" &&
        statement.startsWith("pragma table_info")
      ) {
        return {
          rows: [
            { name: "id" },
            { name: "name" },
            { name: "authorId" },
            { name: "title" },
          ],
        };
      }
      return { rows: [{ id: 10, name: "Fred" }] };
    }),
  };

  const result = await seed(db as any, userPostSchema(), {
    users: [
      {
        name: "Fred",
        posts: [{ authorId: 999, title: "Wrong" }],
      },
    ],
  });

  expect(result.kind).toBe("error");
  expect(result.error?.errorType).toBe("InvalidInput");
  expect(result.error?.message).toContain("users[0].posts[0].authorId");
  expect(db.execute).toHaveBeenCalledWith("rollback");
});

test("seed rolls back when an insert fails", async () => {
  const db = {
    execute: mock(async (statement: any) => {
      if (statement === "begin" || statement === "rollback") {
        return { rows: [] };
      }
      if (
        typeof statement === "string" &&
        statement.startsWith("pragma table_info")
      ) {
        return {
          rows: [
            { name: "id" },
            { name: "name" },
            { name: "authorId" },
            { name: "title" },
          ],
        };
      }
      if (statement === "commit") {
        throw new Error("should not commit");
      }
      if (statement.sql.includes('"users"')) {
        return { rows: [{ id: 10, name: "Fred" }] };
      }
      throw new Error("post insert failed");
    }),
  };

  const result = await seed(db as any, userPostSchema(), {
    users: [{ name: "Fred", posts: [{ title: "First" }] }],
  });

  expect(result.kind).toBe("error");
  expect(result.error?.errorType).toBe("DatabaseError");
  expect(result.error?.message).toContain("post insert failed");
  expect(db.execute).toHaveBeenCalledWith("rollback");
});

test("seed serializes json columns and flattens constructed type columns", async () => {
  const inserts: any[] = [];
  const db = {
    execute: mock(async (statement: any) => {
      if (statement === "begin" || statement === "commit") {
        return { rows: [] };
      }
      if (
        typeof statement === "string" &&
        statement.startsWith("pragma table_info")
      ) {
        return {
          rows: [
            { name: "id" },
            { name: "state" },
            { name: "placement" },
            { name: "placement__x" },
            { name: "placement__y" },
            { name: "placement__scale" },
          ],
        };
      }
      inserts.push(statement);
      return {
        rows: [
          {
            id: 1,
            state: statement.args.seed_0,
            placement: statement.args.seed_1,
            placement__x: statement.args.seed_2,
            placement__y: statement.args.seed_3,
            placement__scale: statement.args.seed_4,
          },
        ],
      };
    }),
  };

  const result = await seed(db as any, jsonAndConstructedSchema(), {
    tokens: [
      {
        state: {
          groups: [{ _type: "GroupState", id: "party", members: ["a"] }],
          clocks: [],
        },
        placement: {
          _type: "MapEntityWorldPlacement",
          x: 10,
          y: 20,
          scale: 100,
        },
      },
    ],
  });

  expect(inserts[0].args.seed_0).toBe(
    JSON.stringify({
      groups: [{ _type: "GroupState", id: "party", members: ["a"] }],
      clocks: [],
    }),
  );
  expect(inserts[0].args.seed_1).toBe("MapEntityWorldPlacement");
  expect(inserts[0].args.seed_2).toBe(10);
  expect(inserts[0].args.seed_3).toBe(20);
  expect(inserts[0].args.seed_4).toBe(100);
  expect(result).toEqual({
    kind: "success",
    response: {
      tokens: [
        {
          id: 1,
          state: {
            groups: [{ _type: "GroupState", id: "party", members: ["a"] }],
            clocks: [],
          },
          placement: {
            _type: "MapEntityWorldPlacement",
            scale: 100,
            x: 10,
            y: 20,
          },
        },
      ],
    },
  });
});

test("seed rejects legacy constructed discriminators", async () => {
  const db = {
    execute: mock(async (statement: any) => {
      if (statement === "begin" || statement === "rollback") {
        return { rows: [] };
      }
      throw new Error("should not insert");
    }),
  };

  const result = await seed(db as any, jsonAndConstructedSchema(), {
    tokens: [
      {
        placement: {
          type: "MapEntityWorldPlacement",
          x: 10,
          y: 20,
          scale: 100,
        } as any,
      },
    ],
  });

  expect(result.kind).toBe("error");
  expect(result.error?.errorType).toBe("InvalidInput");
  expect(result.error?.message).toContain("use '_type'");
  expect(db.execute).toHaveBeenCalledWith("rollback");
});

test("seed validates columns with generated validators when provided", async () => {
  const db = {
    execute: mock(async (statement: any) => {
      if (statement === "begin" || statement === "rollback") {
        return { rows: [] };
      }
      throw new Error("should not insert");
    }),
  };

  const result = await seed(
    db as any,
    jsonAndConstructedSchema(),
    {
      tokens: [
        {
          placement: {
            _type: "MapEntityWorldPlacement",
            x: "bad",
            y: 20,
            scale: 100,
          } as any,
        },
      ],
    },
    {
      tokens: {
        placement: z.discriminatedUnion("_type", [
          z.object({
            _type: z.literal("MapEntityWorldPlacement"),
            x: z.number(),
            y: z.number(),
            scale: z.number(),
          }),
        ]),
      },
    },
  );

  expect(result.kind).toBe("error");
  expect(result.error?.errorType).toBe("InvalidInput");
  expect(result.error?.message).toContain("tokens[0].placement");
  expect(db.execute).toHaveBeenCalledWith("rollback");
});

test("seed uses transformed validator values", async () => {
  const executed: any[] = [];
  const db = {
    execute: mock(async (statement: any) => {
      executed.push(statement);
      if (statement === "begin" || statement === "commit") {
        return { rows: [] };
      }
      if (
        typeof statement === "string" &&
        statement.startsWith("pragma table_info")
      ) {
        return { rows: [{ name: "id" }, { name: "startedAt" }] };
      }
      return { rows: [{ id: 1, startedAt: statement.args.seed_0 }] };
    }),
  };

  const result = await seed(
    db as any,
    eventSchema(),
    { events: [{ startedAt: "1783787812" }] },
    {
      events: {
        startedAt: z
          .string()
          .transform((value) => new Date(Number(value) * 1000)),
      },
    },
  );

  expect(result.kind).toBe("success");
  expect(executed).toContainEqual({
    sql: 'insert into "events" ("startedAt") values ($seed_0) returning *',
    args: { seed_0: 1783787812 },
  });
});

test("seed serializes DateTime columns as unix seconds", async () => {
  const executed: any[] = [];
  const db = {
    execute: mock(async (statement: any) => {
      executed.push(statement);
      if (statement === "begin" || statement === "commit") {
        return { rows: [] };
      }
      if (
        typeof statement === "string" &&
        statement.startsWith("pragma table_info")
      ) {
        return { rows: [{ name: "id" }, { name: "startedAt" }] };
      }
      if (statement.sql.includes('"events"')) {
        return { rows: [{ id: 1, startedAt: statement.args.seed_0 }] };
      }
      throw new Error("unexpected statement");
    }),
  };

  const result = await seed(db as any, eventSchema(), {
    events: [{ startedAt: "2026-07-11T16:36:52.000Z" }],
  });

  expect(result.kind).toBe("success");
  expect(executed).toContainEqual({
    sql: 'insert into "events" ("startedAt") values ($seed_0) returning *',
    args: { seed_0: 1783787812 },
  });
});

test("seed accepts canonical DateTime forms and rejects noncanonical values", async () => {
  const accepted = [
    new Date("2026-07-11T16:36:52.999Z"),
    1783787812,
    "1783787812",
    "2026-07-11T16:36:52.999Z",
    "2026-07-11T18:36:52.999+02:00",
  ];

  for (const startedAt of accepted) {
    const db = {
      execute: mock(async (statement: any) => {
        if (statement === "begin" || statement === "commit")
          return { rows: [] };
        if (
          typeof statement === "string" &&
          statement.startsWith("pragma table_info")
        ) {
          return { rows: [{ name: "id" }, { name: "startedAt" }] };
        }
        return { rows: [{ id: 1, startedAt: statement.args.seed_0 }] };
      }),
    };
    const result = await seed(db as any, eventSchema(), {
      events: [{ startedAt }],
    });
    expect(result.kind).toBe("success");
  }

  for (const startedAt of [
    1783787812.5,
    "1783787812.5",
    "2026-07-11",
    "July 11, 2026",
    "2026-02-30T00:00:00Z",
  ]) {
    const db = { execute: mock(async () => ({ rows: [] })) };
    const result = await seed(db as any, eventSchema(), {
      events: [{ startedAt }],
    });
    expect(result.kind).toBe("error");
    expect(result.error?.errorType).toBe("InvalidInput");
  }
});

function userPostSchema(): SchemaMetadata {
  return {
    tables: {
      users: {
        name: "users",
        columns: [
          {
            name: "id",
            type: "Int",
            nullable: false,
            primary: true,
            unique: true,
            indexed: true,
          },
          {
            name: "name",
            type: "String",
            nullable: false,
            primary: false,
            unique: false,
            indexed: false,
          },
        ],
        links: {
          posts: {
            type: "one-to-many",
            from: "id",
            to: { table: "posts", column: "authorId" },
          },
        },
        indices: [],
      },
      posts: {
        name: "posts",
        columns: [
          {
            name: "id",
            type: "Int",
            nullable: false,
            primary: true,
            unique: true,
            indexed: true,
          },
          {
            name: "authorId",
            type: "Int",
            nullable: false,
            primary: false,
            unique: false,
            indexed: false,
          },
          {
            name: "title",
            type: "String",
            nullable: false,
            primary: false,
            unique: false,
            indexed: false,
          },
        ],
        links: {},
        indices: [],
      },
    },
    queryFieldToTable: {},
  };
}

function eventSchema(): SchemaMetadata {
  return {
    tables: {
      events: {
        name: "events",
        columns: [
          {
            name: "id",
            type: "Int",
            nullable: false,
            primary: true,
            unique: true,
            indexed: true,
          },
          {
            name: "startedAt",
            type: "DateTime",
            nullable: false,
            primary: false,
            unique: false,
            indexed: false,
          },
        ],
        links: {},
        indices: [],
      },
    },
    queryFieldToTable: {},
  };
}

function jsonAndConstructedSchema(): SchemaMetadata {
  return {
    tables: {
      tokens: {
        name: "tokens",
        columns: [
          {
            name: "id",
            type: "Int",
            nullable: false,
            primary: true,
            unique: true,
            indexed: true,
          },
          {
            name: "state",
            type: "Json<GameState>",
            nullable: false,
            primary: false,
            unique: false,
            indexed: false,
          },
          {
            name: "placement",
            type: "MapEntityPlacement",
            nullable: false,
            primary: false,
            unique: false,
            indexed: false,
          },
        ],
        links: {},
        indices: [],
      },
    },
    queryFieldToTable: {},
  };
}
