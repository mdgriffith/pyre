// @ts-nocheck
import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { namespace } from "@pyre/core/local-edits";
import { z } from "zod";

const defaultSyncSql = () => ({
  tables: [
    {
      table_name: "maps",
      primary_key: "id",
      permission_hash: "perm",
      sql: ["select 1"],
      headers: [
        "id",
        "name",
        "tiling",
        "tiling__tileRootKey",
        "tiling__tileWidth",
        "tiling__format",
        "updatedAt",
      ],
      json_columns: [],
    },
  ],
});

const defaultReshapeSyncTableGroups = () => ([
  {
    table_name: "maps",
    headers: ["id", "name", "tiling", "updatedAt"],
    rows: [
      [
        1,
        "World",
        {
          _type: "Tiling",
          tileRootKey: "tiles/root",
          tileWidth: 256,
          format: { _type: "Png" },
        },
        1700000000,
      ],
    ],
  },
]);

let getSyncSqlMock = defaultSyncSql;
let getSyncStatusSqlMock = () => "select 1";
let reshapeSyncTableGroupsMock = defaultReshapeSyncTableGroups;
let introspectionResult = { schema_source: "" };
let setSchemaCalls: unknown[] = [];
let migrationResult: any = { Ok: { sql: [], mark_success: "record migration" } };
let activeSchema: any;

mock.module("./wasm/pyre_wasm.js", () => ({
  sql_is_initialized: () => "select 1 as is_initialized",
  sql_introspect: () => "select introspection",
  get_sync_status_sql: () => getSyncStatusSqlMock(),
  get_sync_sql: (...args: unknown[]) => getSyncSqlMock(...args),
  calculate_sync_deltas: () => ({ groups: [] }),
  reshape_sync_table_groups: (groups: any) => reshapeSyncTableGroupsMock(groups),
  set_schema: (introspection: unknown) => { activeSchema = introspection; setSchemaCalls.push(introspection); },
  get_schema_compiled_contract: () => activeSchema?.compiledContract
    ?? ({ old: "replacement-old", new: "replacement-new" }[activeSchema?.schema_source]) ?? "contract-1",
  get_schema_manifest_contract: () => activeSchema?.manifestContract ?? activeSchema?.compiledContract ?? "contract-1",
  migrate_with_introspection: () => migrationResult,
  sql_introspect_uninitialized: () => "select uninitialized introspection",
  process_introspection: (introspection: unknown) => introspection,
}));

const { catchup, rotateDatabaseEpoch } = await import("./sync");
const { ensureDatabase, loadSchemaFromDatabase, getIntrospectionJson } = await import("./schema");
const { localEdits } = await import("./local-edits");
const { runBatch } = await import("./query");

beforeEach(async () => {
  const db = { execute: async (sql: string) => sql.includes("is_initialized")
    ? { rows: [{ is_initialized: 1 }] }
    : { rows: [{ result: JSON.stringify({ schema_source: "record Note {}" }) }] } };
  await loadSchemaFromDatabase(db as any);
  await loadSchemaFromDatabase("main", db as any);
  setSchemaCalls = [];
});

afterEach(() => {
  getSyncSqlMock = defaultSyncSql;
  getSyncStatusSqlMock = () => "select 1";
  reshapeSyncTableGroupsMock = defaultReshapeSyncTableGroups;
  introspectionResult = { schema_source: "" };
  setSchemaCalls = [];
  migrationResult = { Ok: { sql: [], mark_success: "record migration" } };
  activeSchema = undefined;
});

function initializationDatabase(initialized: boolean, introspection: any) {
  const batches: unknown[][] = [];
  let closed = false;
  const tx = {
    execute: mock(async (sql: string) => {
      if (sql.includes("is_initialized")) {
        return { rows: [{ is_initialized: initialized ? 1 : 0 }] };
      }
      return { rows: [{ result: JSON.stringify(introspection) }] };
    }),
    batch: mock(async (statements: unknown[]) => {
      batches.push(statements);
      return [];
    }),
    commit: mock(async () => { closed = true; }),
    rollback: mock(async () => { closed = true; }),
    close: mock(() => { closed = true; }),
    get closed() { return closed; },
  };
  return {
    db: {
      transaction: mock(async () => tx),
      execute: tx.execute,
    },
    tx,
    batches,
  };
}

function readDatabase(reads: any, schemaSource: string | null = "record Note {}") {
  let closed = false;
  const tx = {
    execute: mock(async (statement: any) => {
      const sql = typeof statement === "string" ? statement : statement.sql;
      if (sql.includes("_pyre_migrations")) return { rows: [{ schema: schemaSource }] };
      return reads.execute(statement);
    }),
    batch: reads.batch,
    rollback: mock(async () => { closed = true; }),
    close: mock(() => { closed = true; }),
    get closed() { return closed; },
  };
  return {
    execute: mock(async () => { throw new Error("Catchup must read inside its transaction"); }),
    batch: mock(async () => { throw new Error("Catchup must batch inside its transaction"); }),
    transaction: mock(async () => tx),
    tx,
  };
}

test("ensureDatabase creates a database in one write transaction", async () => {
  migrationResult = {
    Ok: {
      sql: ["create table notes (id integer primary key)"],
      mark_success: "record migration",
    },
  };
  const database = initializationDatabase(false, {
    tables: [],
    migration_state: { NoMigrationTable: null },
    schema_source: "",
    links: [],
  });

  const outcome = await ensureDatabase(database.db as any, "Campaign", "record Note {}");

  expect(outcome).toBe("created");
  expect(database.db.transaction).toHaveBeenCalledWith("write");
  expect(database.batches).toEqual([[
    "create table notes (id integer primary key)",
    "record migration",
  ]]);
  expect(database.tx.commit).toHaveBeenCalledTimes(1);
});

test("ensureDatabase reuses an unchanged database", async () => {
  const database = initializationDatabase(true, {
    tables: [{ name: "notes" }],
    migration_state: { MigrationTable: { migrations: [] } },
    schema_source: "record Note {}",
    links: [],
  });

  const outcome = await ensureDatabase(database.db as any, "Campaign", "record Note {}");

  expect(outcome).toBe("up-to-date");
  expect(database.batches).toEqual([]);
  expect(database.tx.rollback).toHaveBeenCalledTimes(1);
});

test("schema reads recognize bigint initialization flags", async () => {
  const introspection = { tables: [{ name: "notes" }], schema_source: "record Note {}" };
  const queries: string[] = [];
  const db = { execute: mock(async (sql: string) => {
    queries.push(sql);
    if (sql.includes("is_initialized")) return { rows: [{ is_initialized: 1n }] };
    return { rows: [{ result: JSON.stringify(introspection) }] };
  }) };

  await loadSchemaFromDatabase(db as any);
  expect(queries).toEqual(["select 1 as is_initialized", "select introspection"]);
  queries.length = 0;
  expect(await getIntrospectionJson(db as any)).toEqual(introspection);
  expect(queries).toEqual(["select 1 as is_initialized", "select introspection"]);
});

test("failed ensure refresh removes prior exact-client schema evidence", async () => {
  const database = initializationDatabase(true, {
    tables: [{ name: "notes" }], schema_source: "record Note {}", compiledContract: "contract-1",
  });
  const scope = namespace<any>("Main", "manifest-1");
  const manifest = { version: 1 as const, manifestVersion: scope.manifest, compiledContract: "contract-1",
    replacementContracts: { Main: "contract-1" }, queries: {}, SessionValidator: z.object({}) };
  const bind = () => localEdits.bind({ database: database.db as any, databaseId: "main",
    namespace: scope, manifest, session: {} });

  await ensureDatabase(database.db as any, "Campaign", "record Note {}");
  expect(bind).not.toThrow();
  migrationResult = { Err: ["invalid"] };
  await expect(ensureDatabase(database.db as any, "Campaign", "changed")).rejects.toThrow("Schema migration failed");
  expect(bind).toThrow("InvalidRequest");
});

test("ensureDatabase rejects unmanaged tables", async () => {
  const database = initializationDatabase(false, {
    tables: [{ name: "legacy" }],
    migration_state: { NoMigrationTable: null },
    schema_source: "",
    links: [],
  });

  await expect(
    ensureDatabase(database.db as any, "Campaign", "record Note {}"),
  ).rejects.toThrow("not managed by Pyre");
  expect(database.tx.rollback).toHaveBeenCalledTimes(1);
});

test("schema manifest evidence is exact-client, rejects stale contracts, and clears on failed refresh", async () => {
  const firstSchema: any = { schema_source: "first", compiledContract: "replacement-1", manifestContract: "contract-1" };
  const secondSchema: any = { schema_source: "second", compiledContract: "replacement-2", manifestContract: "contract-2" };
  let failFirst = false;
  const client = (schema: () => any, fails: () => boolean) => ({
    execute: mock(async (sql: string) => {
      if (fails()) throw new Error("refresh failed");
      if (sql.includes("is_initialized")) return { rows: [{ is_initialized: 1 }] };
      return { rows: [{ result: JSON.stringify(schema()) }] };
    }),
  });
  const first = client(() => firstSchema, () => failFirst);
  const second = client(() => secondSchema, () => false);
  const unproven = client(() => firstSchema, () => false);
  const scope = namespace<any>("Main", "manifest-1");
  const matching = { version: 1 as const, manifestVersion: scope.manifest, compiledContract: "contract-1",
    replacementContracts: { Main: "replacement-1" }, queries: {}, SessionValidator: z.object({}) };
  const bind = (database: any, manifest: any = matching) => localEdits.bind({
    database, databaseId: "main", namespace: scope, manifest, session: {},
  });

  await loadSchemaFromDatabase(first as any);
  expect(() => bind(first)).not.toThrow();
  expect(() => bind(unproven)).toThrow("InvalidRequest");
  expect(() => bind(first, { ...matching, compiledContract: "application-contract",
    replacementContracts: { ...matching.replacementContracts, Other: "replacement-other" } })).not.toThrow();
  expect(() => bind(first, { ...matching, compiledContract: "stale" })).toThrow("InvalidRequest");
  expect(() => bind(first, { ...matching, replacementContracts: { Main: "stale" } })).toThrow("InvalidRequest");
  expect(() => bind(first, { ...matching, compiledContract: undefined })).toThrow("InvalidRequest");

  await loadSchemaFromDatabase(second as any);
  expect(() => bind(second)).toThrow("InvalidRequest");
  expect(() => bind(first)).not.toThrow();

  failFirst = true;
  await expect(loadSchemaFromDatabase(first as any)).rejects.toThrow("refresh failed");
  expect(() => bind(first)).toThrow("InvalidRequest");
});

test("migration evidence supersedes an overlapping schema load", async () => {
  const oldSchema = { tables: [{ name: "notes" }], schema_source: "old", compiledContract: "replacement-old", manifestContract: "manifest-old" };
  const newSchema = { tables: [{ name: "notes" }], schema_source: "new", compiledContract: "replacement-new", manifestContract: "manifest-new" };
  let migrated = false;
  let releaseMigration!: () => void;
  let markMigrationStarted!: () => void;
  const migrationStarted = new Promise<void>(resolve => { markMigrationStarted = resolve; });
  const migrationGate = new Promise<void>(resolve => { releaseMigration = resolve; });
  let closed = false;
  const tx = {
    execute: mock(async (sql: string) => sql.includes("is_initialized")
      ? { rows: [{ is_initialized: 1 }] }
      : { rows: [{ result: JSON.stringify(migrated ? newSchema : oldSchema) }] }),
    batch: mock(async () => { markMigrationStarted(); await migrationGate; migrated = true; }),
    commit: mock(async () => { closed = true; }), rollback: mock(async () => { closed = true; }),
    close: mock(() => { closed = true; }), get closed() { return closed; },
  };
  const db = {
    transaction: mock(async () => tx),
    execute: mock(async (sql: string) => sql.includes("is_initialized")
      ? { rows: [{ is_initialized: 1 }] }
      : { rows: [{ result: JSON.stringify(oldSchema) }] }),
  };
  migrationResult = { Ok: { sql: ["alter table notes add column title text"], mark_success: "record migration" } };
  const scope = namespace<any>("Main", "manifest-1");
  const bind = (compiledContract: string, replacementContract: string) => localEdits.bind({
    database: db as any, databaseId: "main", namespace: scope,
    manifest: { version: 1, manifestVersion: scope.manifest, compiledContract,
      replacementContracts: { Main: replacementContract }, queries: {}, SessionValidator: z.object({}) },
    session: {},
  });

  const migration = ensureDatabase(db as any, "Main", "new");
  await migrationStarted;
  await loadSchemaFromDatabase(db as any);
  expect(() => bind("manifest-old", "replacement-old")).toThrow("InvalidRequest");
  releaseMigration();
  await migration;

  expect(() => bind("manifest-new", "replacement-new")).not.toThrow();
  expect(() => bind("manifest-old", "replacement-old")).toThrow("InvalidRequest");
});

test("queued batch revalidates schema evidence after acquiring its write transaction", async () => {
  const authority = { databaseId: "schema-race", namespace: "Main", manifest: "manifest-1", instance: "tab-1", authGeneration: 0 };
  const request = { version: 1, ...authority, databaseEpoch: "epoch-1", requestId: "request-1", sequence: 1,
    operations: [{ operation: "write", input: {} }] };
  const query = { id: "write", operation: "transaction", primary_db: "Main", sql: [
    { include: false, params: [], sql: "update notes set body = 'captured'" },
  ], session_args: [], optional_input_args: [], json_input_args: [], InputValidator: z.object({}), SessionValidator: z.object({}) };
  const oldManifest = { version: 1, manifestVersion: authority.manifest, compiledContract: "manifest-old",
    replacementContracts: { Main: "replacement-old" }, queries: { write: query }, SessionValidator: z.object({}) };

  let releaseFirst: () => void;
  let markFirstStarted: () => void;
  const firstStarted = new Promise<void>(resolve => { markFirstStarted = resolve; });
  const firstGate = new Promise<void>(resolve => { releaseFirst = resolve; });
  const firstTx = {
    execute: mock(async (statement: any) => {
      const sql = typeof statement === "string" ? statement : statement.sql;
      if (sql === "pragma database_list") {
        markFirstStarted();
        await firstGate;
        return { rows: [{ name: "main", file: "/tmp/schema-race.db" }], columns: ["name", "file"], rowsAffected: 0 };
      }
      if (sql.includes("select database_epoch")) return { rows: [{ database_epoch: "epoch-1" }], columns: ["database_epoch"], rowsAffected: 0 };
      if (sql.includes("_pyre_migrations")) return { rows: [{ schema: "old" }] };
      if (sql.includes("update _pyre_sync")) return { rows: [{ database_epoch: "epoch-1", server_revision: 1 }], columns: ["database_epoch", "server_revision"], rowsAffected: 1 };
      return { rows: [], columns: [], rowsAffected: 1 };
    }),
    commit: mock(async () => {}), rollback: mock(async () => {}), close: mock(() => {}),
  };
  const integerMode = { rows: [{ _pyre_integer_mode: 1 }] };
  const firstDb = { execute: mock(async (sql: string) => sql.includes("_pyre_integer_mode")
    ? integerMode : sql.includes("is_initialized") ? { rows: [{ is_initialized: 1 }] }
      : { rows: [{ result: JSON.stringify({ schema_source: "old", compiledContract: "replacement-old", manifestContract: "manifest-old" }) }] }),
    transaction: mock(async () => firstTx) };

  let schema = { schema_source: "old", compiledContract: "replacement-old", manifestContract: "manifest-old" };
  const secondExecute = mock(async (sql: string) => sql.includes("_pyre_integer_mode")
    ? integerMode
    : sql.includes("is_initialized")
      ? { rows: [{ is_initialized: 1 }] }
      : { rows: [{ result: JSON.stringify(schema) }] });
  const secondTx = { execute: mock(async (statement: any) => {
    const sql = typeof statement === "string" ? statement : statement.sql;
    if (sql === "pragma database_list") return { rows: [{ name: "main", file: "/tmp/schema-race.db" }] };
    if (sql.includes("_pyre_migrations")) return { rows: [{ schema: schema.schema_source }] };
    throw new Error("captured SQL must not execute");
  }),
    commit: mock(async () => {}), rollback: mock(async () => {}), close: mock(() => {}) };
  const secondDb = { execute: secondExecute, transaction: mock(async () => secondTx) };

  await loadSchemaFromDatabase(firstDb as any);
  await loadSchemaFromDatabase(secondDb as any);
  const first = runBatch(firstDb as any, oldManifest as any, authority, request as any, {});
  await firstStarted;
  const queued = runBatch(secondDb as any, oldManifest as any, authority, request as any, {});
  schema = { schema_source: "new", compiledContract: "replacement-new", manifestContract: "manifest-new" };
  releaseFirst!();

  expect((await first).kind).toBe("success");
  expect(await queued).toEqual({ kind: "error", error: { errorType: "InvalidRequest", message: "InvalidRequest" } });
  expect(secondDb.transaction).toHaveBeenCalledWith("write");
  expect(secondTx.execute.mock.calls.map(([statement]) => typeof statement === "string" ? statement : statement.sql))
    .toEqual(['SELECT schema FROM "main"._pyre_migrations WHERE finished_at IS NOT NULL AND error IS NULL ORDER BY id DESC LIMIT 1']);
  expect(secondTx.rollback).toHaveBeenCalledTimes(1);
});

test("ensureDatabase refreshes existing database-id schema registrations", async () => {
  const introspection = {
    tables: [{ name: "notes" }], schema_source: "record Note {}", compiledContract: "contract-1",
  };
  const database = initializationDatabase(true, introspection);
  await loadSchemaFromDatabase("main", database.db as any);
  await ensureDatabase(database.db as any, "Main", introspection.schema_source);
  getSyncSqlMock = () => ({ tables: [] });

  await catchup(readDatabase({ execute: mock(async () => ({ rows: [{ database_epoch: "epoch" }] })), batch: mock(async () => []) }) as any,
    { tables: {} }, {}, 1000, "main");

  expect(setSchemaCalls.at(-1)).toEqual(introspection);
});

test("catchup activates the schema loaded for its databaseId", async () => {
  getSyncSqlMock = () => ({ tables: [] });
  const mainIntrospection = { schema_source: "main schema" };
  const campaignIntrospection = { schema_source: "campaign schema" };
  const schemaDb = {
    execute: mock(async (sql: string) => {
      if (sql.includes("is_initialized")) {
        return { rows: [{ is_initialized: 1 }] };
      }

      return { rows: [{ result: JSON.stringify(introspectionResult) }] };
    }),
  };
  const db = {
    execute: mock(async () => ({ rows: [{ database_epoch: "test-epoch" }] })),
    batch: mock(async () => ([])),
  };

  introspectionResult = mainIntrospection;
  await loadSchemaFromDatabase("main", schemaDb as any);
  introspectionResult = campaignIntrospection;
  await loadSchemaFromDatabase("campaign", schemaDb as any);

  await catchup(readDatabase(db, mainIntrospection.schema_source) as any, { tables: {} }, {}, 1000, "main");

  expect(setSchemaCalls.at(-1)).toEqual(mainIntrospection);
});

test("catchup reshapes flattened custom types before returning sync rows", async () => {
  const db = {
    execute: mock(async () => ({ rows: [{ table_name: "maps", needs_sync: 1, database_epoch: "test-epoch" }] })),
    batch: mock(async () => ([
      {
        columns: [
          "id",
          "name",
          "tiling",
          "tiling__tileRootKey",
          "tiling__tileWidth",
          "tiling__format",
          "updatedAt",
        ],
        rows: [
          {
            id: 1,
            name: "World",
            tiling: "Tiling",
            tiling__tileRootKey: "tiles/root",
            tiling__tileWidth: 256,
            tiling__format: "Png",
            updatedAt: 1700000000n,
          },
        ],
      },
    ])),
  };

  const result = await catchup(readDatabase(db) as any, { tables: {} }, {}, 1000);

  expect(result).toEqual({
    databaseEpoch: "test-epoch",
    tables: {
      maps: {
        rows: [
          {
            id: 1,
            name: "World",
            tiling: {
              _type: "Tiling",
              tileRootKey: "tiles/root",
              tileWidth: 256,
              format: { _type: "Png" },
            },
            updatedAt: 1700000000,
          },
        ],
        permission_hash: "perm",
        last_seen_updated_at: 1700000000,
        last_seen_primary_key: 1,
      },
    },
    has_more: false,
  });
});

test("catchup stamps response with databaseId when provided", async () => {
  getSyncSqlMock = () => ({ tables: [] });
  const schemaDb = {
    execute: mock(async (sql: string) => {
      if (sql.includes("is_initialized")) {
        return { rows: [{ is_initialized: 1 }] };
      }

      return { rows: [{ result: JSON.stringify({ schema_source: "campaign schema" }) }] };
    }),
  };
  const db = {
    execute: mock(async () => ({ rows: [{ database_epoch: "test-epoch" }] })),
    batch: mock(async () => ([])),
  };

  await loadSchemaFromDatabase("campaign:123", schemaDb as any);
  const result = await catchup(readDatabase(db, "campaign schema") as any, { tables: {} }, {}, 1000, "campaign:123");

  expect(result.databaseId).toBe("campaign:123");
});

test("catchup reuses server revision from status query without a second execute", async () => {
  getSyncSqlMock = () => ({ tables: [] });
  const db = {
    execute: mock(async () => ({ rows: [{ server_revision: 7, database_epoch: "test-epoch" }] })),
    batch: mock(async () => ([])),
  };

  const database = readDatabase(db);
  const result = await catchup(database as any, { tables: {} }, {}, 1000);

  expect(result.serverRevision).toBe(7);
  expect(database.transaction).toHaveBeenCalledWith("read");
  expect(database.tx.execute).toHaveBeenCalledTimes(2);
  expect(database.tx.rollback).toHaveBeenCalledTimes(1);
  expect(database.tx.close).toHaveBeenCalledTimes(1);
  expect(database.execute).not.toHaveBeenCalled();
  expect(db.execute).toHaveBeenCalledTimes(1);
  expect(db.batch).toHaveBeenCalledTimes(0);
});

test("catchup returns an explicit replacement without querying table rows on epoch mismatch", async () => {
  let syncSqlCalls = 0;
  getSyncSqlMock = () => {
    syncSqlCalls += 1;
    return defaultSyncSql();
  };
  const db = {
    execute: mock(async () => ({ rows: [{ server_revision: 7, database_epoch: "current-epoch" }] })),
    batch: mock(async () => ([])),
  };

  const result = await catchup(readDatabase(db) as any, { tables: {} }, {}, 1000, "main", "stale-epoch");

  expect(result).toEqual({
    type: "reset",
    databaseId: "main",
    databaseEpoch: "current-epoch",
    operation: "replace",
    scope: "database",
    reason: "database_epoch_changed",
  });
  expect(syncSqlCalls).toBe(0);
  expect(db.batch).toHaveBeenCalledTimes(0);
});

test("rotateDatabaseEpoch replaces the epoch and resets revision", async () => {
  const db = {
    execute: mock(async (sql: string) => {
      expect(sql).toContain("server_revision = 0");
      return { rows: [{ database_epoch: "rotated-epoch" }] };
    }),
  };

  expect(await rotateDatabaseEpoch(db as any)).toBe("rotated-epoch");
});

test("catchup normalizes bigint row values before reshaping", async () => {
  const db = {
    execute: mock(async () => ({ rows: [{ table_name: "maps", needs_sync: 1, database_epoch: "test-epoch" }] })),
    batch: mock(async () => ([
      {
        columns: [
          "id",
          "name",
          "tiling",
          "tiling__tileRootKey",
          "tiling__tileWidth",
          "tiling__format",
          "updatedAt",
        ],
        rows: [
          {
            id: 1n,
            name: "World",
            tiling: "Tiling",
            tiling__tileRootKey: "tiles/root",
            tiling__tileWidth: 256n,
            tiling__format: "Png",
            updatedAt: 1700000000,
          },
        ],
      },
    ])),
  };

  const result = await catchup(readDatabase(db) as any, { tables: {} }, {}, 1000);

  expect(result.tables.maps.rows[0]).toEqual({
    id: 1,
    name: "World",
    tiling: {
      _type: "Tiling",
      tileRootKey: "tiles/root",
      tileWidth: 256,
      format: { _type: "Png" },
    },
    updatedAt: 1700000000,
  });
  expect(result.tables.maps.last_seen_updated_at).toBe(1700000000);
});

test("catchup unwraps double-encoded json objects for json columns", async () => {
  getSyncSqlMock = () => ({
    tables: [
      {
        table_name: "gameEntities",
        primary_key: "id",
        permission_hash: "perm",
        sql: ["select 1"],
        headers: ["id", "attrs", "updatedAt"],
        json_columns: ["attrs"],
      },
    ],
  });
  reshapeSyncTableGroupsMock = (groups: any) => groups;

  const db = {
    execute: mock(async () => ({ rows: [{ table_name: "gameEntities", needs_sync: 1, database_epoch: "test-epoch" }] })),
    batch: mock(async () => ([
      {
        columns: ["id", "attrs", "updatedAt"],
        rows: [
          {
            id: 1,
            attrs: '"{\\"position\\":{\\"x\\":11,\\"y\\":14}}"',
            updatedAt: 1700000000,
          },
        ],
      },
    ])),
  };

  const result = await catchup(readDatabase(db) as any, { tables: {} }, {}, 1000);

  expect(result.tables.gameEntities.rows[0]).toEqual({
    id: 1,
    attrs: {
      position: {
        x: 11,
        y: 14,
      },
    },
    updatedAt: 1700000000,
  });
});

test("catchup expands aggregate sync row payloads", async () => {
  getSyncSqlMock = () => ({
    tables: [
      {
        table_name: "gameEntities",
        primary_key: "id",
        permission_hash: "perm",
        sql: ["select aggregate rows"],
        headers: ["id", "attrs", "updatedAt"],
        json_columns: ["attrs"],
      },
    ],
  });
  reshapeSyncTableGroupsMock = (groups: any) => groups;

  const db = {
    execute: mock(async () => ({ rows: [{ table_name: "gameEntities", needs_sync: 1, database_epoch: "test-epoch" }] })),
    batch: mock(async () => ([
      {
        columns: ["_pyre_rows"],
        rows: [
          {
            _pyre_rows: JSON.stringify([
              [1, { position: { x: 11, y: 14 } }, 1700000000],
            ]),
          },
        ],
      },
    ])),
  };

  const result = await catchup(readDatabase(db) as any, { tables: {} }, {}, 1000);

  expect(result.tables.gameEntities.rows[0]).toEqual({
    id: 1,
    attrs: { position: { x: 11, y: 14 } },
    updatedAt: 1700000000,
  });
  expect(result.tables.gameEntities.last_seen_updated_at).toBe(1700000000);
});

test("catchup executes status and table sync SQL with bound params", async () => {
  getSyncStatusSqlMock = () => ({ sql: "select ? as status", params: ["tenant' OR 1=1 --"] });
  getSyncSqlMock = () => ({
    tables: [
      {
        table_name: "maps",
        primary_key: "id",
        permission_hash: "perm",
        sql: ["select ? as id, ? as name, ? as updatedAt"],
        params: [[1, "World", 1700000000]],
        headers: ["id", "name", "updatedAt"],
        json_columns: [],
      },
    ],
  });
  reshapeSyncTableGroupsMock = (groups: any) => groups;
  const db = {
    execute: mock(async () => ({ rows: [{ table_name: "maps", needs_sync: 1, database_epoch: "test-epoch" }] })),
    batch: mock(async () => ([
      {
        columns: ["id", "name", "updatedAt"],
        rows: [{ id: 1, name: "World", updatedAt: 1700000000 }],
      },
    ])),
  };

  await catchup(readDatabase(db) as any, { tables: {} }, {}, 1000);

  expect(db.execute).toHaveBeenCalledWith({ sql: "select ? as status", args: ["tenant' OR 1=1 --"] });
  expect(db.batch).toHaveBeenCalledWith([
    { sql: "select ? as id, ? as name, ? as updatedAt", args: [1, "World", 1700000000] },
  ]);
});

test("catchup caps pageSize before requesting sync SQL and slicing rows", async () => {
  let requestedPageSize = 0;
  getSyncSqlMock = (_statusRows?: unknown, _cursor?: unknown, _session?: unknown, pageSize?: number) => {
    requestedPageSize = pageSize ?? 0;
    return defaultSyncSql();
  };
  reshapeSyncTableGroupsMock = (groups: any) => groups;
  const rows = Array.from({ length: 5001 }, (_, index) => ({
    id: index + 1,
    name: `Map ${index + 1}`,
    tiling: null,
    tiling__tileRootKey: null,
    tiling__tileWidth: null,
    tiling__format: null,
    updatedAt: index + 1,
  }));
  const db = {
    execute: mock(async () => ({ rows: [{ table_name: "maps", needs_sync: 1, database_epoch: "test-epoch" }] })),
    batch: mock(async () => ([
      {
        columns: ["id", "name", "tiling", "tiling__tileRootKey", "tiling__tileWidth", "tiling__format", "updatedAt"],
        rows,
      },
    ])),
  };

  const result = await catchup(readDatabase(db) as any, { tables: {} }, {}, 999999);

  expect(requestedPageSize).toBe(5000);
  expect(result.tables.maps.rows).toHaveLength(5000);
  expect(result.tables.maps.last_seen_primary_key).toBe(5000);
  expect(result.has_more).toBe(true);
});

test("catchup rejects oversized sync cursors before wasm work", async () => {
  const tables: Record<string, { last_seen_updated_at: number | null; permission_hash: string }> = {};
  for (let index = 0; index < 513; index += 1) {
    tables[`table_${index}`] = { last_seen_updated_at: null, permission_hash: "perm" };
  }
  const db = {
    execute: mock(async () => ({ rows: [{ database_epoch: "test-epoch" }] })),
    batch: mock(async () => ([])),
  };

  await expect(catchup(db as any, { tables }, {}, 1000)).rejects.toThrow("max is 512");
  expect(db.execute).not.toHaveBeenCalled();
});

test("catchup rejects oversized sync cursor permission hashes", async () => {
  const db = {
    execute: mock(async () => ({ rows: [{ database_epoch: "test-epoch" }] })),
    batch: mock(async () => ([])),
  };

  await expect(catchup(db as any, {
    tables: {
      maps: { last_seen_updated_at: null, permission_hash: "x".repeat(257) },
    },
  }, {}, 1000)).rejects.toThrow("permission_hash");
  expect(db.execute).not.toHaveBeenCalled();
});

test("catchup rejects non-integer sync cursor timestamps before wasm work", async () => {
  for (const last_seen_updated_at of [1.5, "1700000000", Number.NaN, Number.POSITIVE_INFINITY, Number.MAX_SAFE_INTEGER + 1]) {
    const db = {
      execute: mock(async () => ({ rows: [{ database_epoch: "test-epoch" }] })),
      batch: mock(async () => ([])),
    };

    await expect(catchup(db as any, {
      tables: {
        maps: { last_seen_updated_at, permission_hash: "perm" },
      },
    } as any, {}, 1000)).rejects.toThrow("safe integer or null");
    expect(db.execute).not.toHaveBeenCalled();
  }
});

test("catchup rejects non-integer database timestamps", async () => {
  for (const updatedAt of [1.5, "1700000000", new Date("2023-11-14T22:13:20Z"), Number.NaN, BigInt(Number.MAX_SAFE_INTEGER) + 1n]) {
    const db = {
      execute: mock(async () => ({ rows: [{ table_name: "maps", needs_sync: 1, database_epoch: "test-epoch" }] })),
      batch: mock(async () => ([
        {
          columns: ["id", "name", "tiling", "tiling__tileRootKey", "tiling__tileWidth", "tiling__format", "updatedAt"],
          rows: [{
            id: 1,
            name: "World",
            tiling: null,
            tiling__tileRootKey: null,
            tiling__tileWidth: null,
            tiling__format: null,
            updatedAt,
          }],
        },
      ])),
    };

    await expect(catchup(readDatabase(db) as any, { tables: {} }, {}, 1000)).rejects.toThrow(
      "database updatedAt must be a safe integer number or bigint",
    );
  }
});

test("catchup rejects missing or changed persisted authority before status or row reads", async () => {
  for (const source of [null, "", "new"]) {
    const reads = { execute: mock(async () => { throw new Error("status must not execute"); }), batch: mock(async () => []) };
    const db = readDatabase(reads, source);
    getSyncStatusSqlMock = mock(() => "select status");

    await expect(catchup(db as any, { tables: {} }, {})).rejects.toThrow(
      source === "new" ? "Schema contract mismatch" : "Missing persisted schema authority",
    );

    expect(db.transaction).toHaveBeenCalledWith("read");
    expect(db.tx.execute.mock.calls).toEqual([
      ['SELECT schema FROM "main"._pyre_migrations WHERE finished_at IS NOT NULL AND error IS NULL ORDER BY id DESC LIMIT 1'],
    ]);
    expect(getSyncStatusSqlMock).not.toHaveBeenCalled();
    expect(reads.execute).not.toHaveBeenCalled();
    expect(reads.batch).not.toHaveBeenCalled();
    expect(db.execute).not.toHaveBeenCalled();
    expect(db.tx.rollback).toHaveBeenCalledTimes(1);
    expect(db.tx.close).toHaveBeenCalledTimes(1);
  }
});

test("catchup requires cached schema source even when WASM has a compiled contract", async () => {
  const schemaDb = initializationDatabase(true, { schema_source: "" });
  await loadSchemaFromDatabase(schemaDb.db as any);
  const db = readDatabase({ execute: mock(async () => ({})), batch: mock(async () => []) });

  await expect(catchup(db as any, { tables: {} }, {})).rejects.toThrow("Missing replacement schema");
  expect(db.transaction).not.toHaveBeenCalled();
});

test("catchup restores its captured schema across transaction, status and batch awaits", async () => {
  const captured = { schema_source: "old", compiledContract: "replacement-old", tables: [{ name: "maps" }] };
  const schemaDb = initializationDatabase(true, captured);
  await loadSchemaFromDatabase("catchup-race", schemaDb.db as any);
  const other = { schema_source: "new", compiledContract: "replacement-new" };
  getSyncStatusSqlMock = () => {
    expect(activeSchema).toEqual(captured);
    return "select status";
  };
  getSyncSqlMock = () => {
    expect(activeSchema).toEqual(captured);
    return defaultSyncSql();
  };
  reshapeSyncTableGroupsMock = groups => {
    expect(activeSchema).toEqual(captured);
    return groups;
  };
  const db = readDatabase({
    execute: mock(async () => {
      activeSchema = other;
      return { rows: [{ database_epoch: "epoch", server_revision: 7 }] };
    }),
    batch: mock(async () => {
      activeSchema = other;
      return [{ columns: ["id", "updatedAt"], rows: [{ id: 1, updatedAt: 42 }] }];
    }),
  }, "old");
  db.transaction.mockImplementation(async () => {
    await loadSchemaFromDatabase("catchup-race", initializationDatabase(true, other).db as any);
    return db.tx;
  });

  const result = await catchup(db as any, { tables: {} }, {}, 1000, "catchup-race");
  expect(result.serverRevision).toBe(7);
  expect(result.tables.maps.rows[0]).toMatchObject({ id: 1, updatedAt: 42 });
  expect(db.transaction).toHaveBeenCalledTimes(1);
  expect(db.tx.execute).toHaveBeenCalledTimes(2);
  expect(db.tx.batch).toHaveBeenCalledWith(["select 1"]);
  expect(db.execute).not.toHaveBeenCalled();
  expect(db.batch).not.toHaveBeenCalled();
  expect(db.tx.rollback).toHaveBeenCalledTimes(1);
  expect(db.tx.close).toHaveBeenCalledTimes(1);
});

test("catchup preserves replacement-required errors when read transaction cleanup fails", async () => {
  getSyncSqlMock = () => "Error: replacement required";
  const db = readDatabase({ execute: mock(async () => ({ rows: [{ database_epoch: "epoch" }] })), batch: mock(async () => []) });
  db.tx.rollback.mockImplementation(async () => { throw new Error("rollback failed"); });
  db.tx.close.mockImplementation(() => { throw new Error("close failed"); });

  await expect(catchup(db as any, { tables: {} }, {})).rejects.toThrow("Error: replacement required");
  expect(db.tx.batch).not.toHaveBeenCalled();
  expect(db.tx.rollback).toHaveBeenCalledTimes(1);
  expect(db.tx.close).toHaveBeenCalledTimes(1);
});
