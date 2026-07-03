import { expect, mock, test } from "bun:test";
import type { SchemaMetadata } from "@pyre/core";
import { z } from "zod";
import { run, seed } from "./query";

test("sync wraps mutation responses with server revision metadata", async () => {
  const db = {
    batch: mock(async () => ([
      {
        columns: ["createdNote"],
        rows: [{ createdNote: JSON.stringify({ id: 1, body: "one" }) }],
      },
      {
        columns: ["_affectedRows"],
        rows: [{ _affectedRows: JSON.stringify([{ table_name: "notes", headers: ["id"], rows: [[1]] }]) }],
      },
    ])),
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

test("sync mode omits normal mutation result", async () => {
  const db = {
    batch: mock(async () => ([
      {
        columns: ["_affectedRows"],
        rows: [{ _affectedRows: JSON.stringify([{ table_name: "notes", headers: ["id"], rows: [[1]] }]) }],
      },
    ])),
  };

  const result = await run(
    db as any,
    {
      createNote: {
        id: "createNote",
        sql: [
          { include: true, params: [], sql: "select createdNote" },
        ],
        syncSql: [
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
  });
  expect(db.batch).toHaveBeenCalledWith([{ sql: "select _affectedRows", args: {} }]);
});

test("seed inserts nested rows through schema links", async () => {
  const executed: any[] = [];
  const db = {
    execute: mock(async (statement: any) => {
      executed.push(statement);
      if (statement === "begin" || statement === "commit") {
        return { rows: [] };
      }
      if (typeof statement === "string" && statement.startsWith("pragma table_info")) {
        return { rows: [{ name: "id" }, { name: "name" }, { name: "authorId" }, { name: "title" }] };
      }
      if (statement.sql.includes('"users"')) {
        return { rows: [{ id: 10, name: statement.args.seed_0 }] };
      }
      if (statement.sql.includes('"posts"')) {
        const values = Object.values(statement.args);
        const authorId = values.find((value) => value === 10);
        const title = values.find((value) => value === "First" || value === "Second");
        return { rows: [{ id: title === "First" ? 20 : 21, authorId, title }] };
      }
      throw new Error("unexpected statement");
    }),
  };

  const result = await seed(db as any, userPostSchema(), {
    users: [
      {
        name: "Fred",
        posts: [
          { title: "First" },
          { title: "Second" },
        ],
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
  const postInserts = executed.filter((statement) => typeof statement !== "string" && statement.sql.includes('"posts"'));
  expect(Object.values(postInserts[0].args)).toContain(10);
  expect(Object.values(postInserts[1].args)).toContain(10);
});

test("seed batches sibling inserts when batch is supported", async () => {
  const executed: any[] = [];
  const batched: any[][] = [];
  const db = {
    execute: mock(async (statement: any) => {
      executed.push(statement);
      if (typeof statement === "string" && statement.startsWith("pragma table_info")) {
        return { rows: [{ name: "id" }, { name: "name" }, { name: "authorId" }, { name: "title" }] };
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
        const title = values.find((value) => value === "First" || value === "Second");
        return { rows: [{ id: title === "First" ? 20 : 21, authorId, title }] };
      });
    }),
  };

  const result = await seed(db as any, userPostSchema(), {
    users: [
      {
        name: "Fred",
        posts: [
          { title: "First" },
          { title: "Second" },
        ],
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
  expect(executed.every((statement) => typeof statement === "string" && statement.startsWith("pragma table_info"))).toBe(true);
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
      if (typeof statement === "string" && statement.startsWith("pragma table_info")) {
        return { rows: [{ name: "id" }, { name: "name" }, { name: "authorId" }, { name: "title" }] };
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
      if (typeof statement === "string" && statement.startsWith("pragma table_info")) {
        return { rows: [{ name: "id" }, { name: "name" }, { name: "authorId" }, { name: "title" }] };
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
      if (typeof statement === "string" && statement.startsWith("pragma table_info")) {
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
        state: { groups: [{ _type: "GroupState", id: "party", members: ["a"] }], clocks: [] },
        placement: { _type: "MapEntityWorldPlacement", x: 10, y: 20, scale: 100 },
      },
    ],
  });

  expect(inserts[0].args.seed_0).toBe(JSON.stringify({ groups: [{ _type: "GroupState", id: "party", members: ["a"] }], clocks: [] }));
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
          state: { groups: [{ _type: "GroupState", id: "party", members: ["a"] }], clocks: [] },
          placement: { _type: "MapEntityWorldPlacement", scale: 100, x: 10, y: 20 },
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
    tokens: [{ placement: { type: "MapEntityWorldPlacement", x: 10, y: 20, scale: 100 } as any }],
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
    { tokens: [{ placement: { _type: "MapEntityWorldPlacement", x: "bad", y: 20, scale: 100 } as any }] },
    {
      tokens: {
        placement: z.discriminatedUnion("_type", [
          z.object({ _type: z.literal("MapEntityWorldPlacement"), x: z.number(), y: z.number(), scale: z.number() }),
        ]),
      },
    },
  );

  expect(result.kind).toBe("error");
  expect(result.error?.errorType).toBe("InvalidInput");
  expect(result.error?.message).toContain("tokens[0].placement");
  expect(db.execute).toHaveBeenCalledWith("rollback");
});

function userPostSchema(): SchemaMetadata {
  return {
    tables: {
      users: {
        name: "users",
        columns: [
          { name: "id", type: "Int", nullable: false, primary: true, unique: true, indexed: true },
          { name: "name", type: "String", nullable: false, primary: false, unique: false, indexed: false },
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
          { name: "id", type: "Int", nullable: false, primary: true, unique: true, indexed: true },
          { name: "authorId", type: "Int", nullable: false, primary: false, unique: false, indexed: false },
          { name: "title", type: "String", nullable: false, primary: false, unique: false, indexed: false },
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
          { name: "id", type: "Int", nullable: false, primary: true, unique: true, indexed: true },
          { name: "state", type: "Json<GameState>", nullable: false, primary: false, unique: false, indexed: false },
          { name: "placement", type: "MapEntityPlacement", nullable: false, primary: false, unique: false, indexed: false },
        ],
        links: {},
        indices: [],
      },
    },
    queryFieldToTable: {},
  };
}
