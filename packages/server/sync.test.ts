// @ts-nocheck
import { afterEach, expect, mock, test } from "bun:test";

const defaultSyncSql = () => ({
  tables: [
    {
      table_name: "maps",
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

mock.module("./wasm/pyre_wasm.js", () => ({
  sql_is_initialized: () => "select 1 as is_initialized",
  sql_introspect: () => "select introspection",
  get_sync_status_sql: () => getSyncStatusSqlMock(),
  get_sync_sql: (...args: unknown[]) => getSyncSqlMock(...args),
  calculate_sync_deltas: () => ({ groups: [] }),
  reshape_sync_table_groups: (groups: any) => reshapeSyncTableGroupsMock(groups),
  set_schema: (introspection: unknown) => setSchemaCalls.push(introspection),
}));

const { catchup } = await import("./sync");
const { loadSchemaFromDatabase } = await import("./schema");

afterEach(() => {
  getSyncSqlMock = defaultSyncSql;
  getSyncStatusSqlMock = () => "select 1";
  reshapeSyncTableGroupsMock = defaultReshapeSyncTableGroups;
  introspectionResult = { schema_source: "" };
  setSchemaCalls = [];
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
    execute: mock(async () => ({ rows: [] })),
    batch: mock(async () => ([])),
  };

  introspectionResult = mainIntrospection;
  await loadSchemaFromDatabase("main", schemaDb as any);
  introspectionResult = campaignIntrospection;
  await loadSchemaFromDatabase("campaign", schemaDb as any);

  await catchup(db as any, { tables: {} }, {}, 1000, "main");

  expect(setSchemaCalls.at(-1)).toEqual(mainIntrospection);
});

test("catchup reshapes flattened custom types before returning sync rows", async () => {
  const db = {
    execute: mock(async () => ({ rows: [{ table_name: "maps", needs_sync: 1 }] })),
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

  const result = await catchup(db as any, { tables: {} }, {}, 1000);

  expect(result).toEqual({
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
    execute: mock(async () => ({ rows: [] })),
    batch: mock(async () => ([])),
  };

  await loadSchemaFromDatabase("campaign:123", schemaDb as any);
  const result = await catchup(db as any, { tables: {} }, {}, 1000, "campaign:123");

  expect(result.databaseId).toBe("campaign:123");
});

test("catchup reuses server revision from status query without a second execute", async () => {
  getSyncSqlMock = () => ({ tables: [] });
  const db = {
    execute: mock(async () => ({ rows: [{ server_revision: 7 }] })),
    batch: mock(async () => ([])),
  };

  const result = await catchup(db as any, { tables: {} }, {}, 1000);

  expect(result.serverRevision).toBe(7);
  expect(db.execute).toHaveBeenCalledTimes(1);
  expect(db.batch).toHaveBeenCalledTimes(0);
});

test("catchup normalizes bigint row values before reshaping", async () => {
  const db = {
    execute: mock(async () => ({ rows: [{ table_name: "maps", needs_sync: 1 }] })),
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

  const result = await catchup(db as any, { tables: {} }, {}, 1000);

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
        permission_hash: "perm",
        sql: ["select 1"],
        headers: ["id", "attrs", "updatedAt"],
        json_columns: ["attrs"],
      },
    ],
  });
  reshapeSyncTableGroupsMock = (groups: any) => groups;

  const db = {
    execute: mock(async () => ({ rows: [{ table_name: "gameEntities", needs_sync: 1 }] })),
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

  const result = await catchup(db as any, { tables: {} }, {}, 1000);

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
        permission_hash: "perm",
        sql: ["select aggregate rows"],
        headers: ["id", "attrs", "updatedAt"],
        json_columns: ["attrs"],
      },
    ],
  });
  reshapeSyncTableGroupsMock = (groups: any) => groups;

  const db = {
    execute: mock(async () => ({ rows: [{ table_name: "gameEntities", needs_sync: 1 }] })),
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

  const result = await catchup(db as any, { tables: {} }, {}, 1000);

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
    execute: mock(async () => ({ rows: [{ table_name: "maps", needs_sync: 1 }] })),
    batch: mock(async () => ([
      {
        columns: ["id", "name", "updatedAt"],
        rows: [{ id: 1, name: "World", updatedAt: 1700000000 }],
      },
    ])),
  };

  await catchup(db as any, { tables: {} }, {}, 1000);

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
    execute: mock(async () => ({ rows: [{ table_name: "maps", needs_sync: 1 }] })),
    batch: mock(async () => ([
      {
        columns: ["id", "name", "tiling", "tiling__tileRootKey", "tiling__tileWidth", "tiling__format", "updatedAt"],
        rows,
      },
    ])),
  };

  const result = await catchup(db as any, { tables: {} }, {}, 999999);

  expect(requestedPageSize).toBe(5000);
  expect(result.tables.maps.rows).toHaveLength(5000);
  expect(result.has_more).toBe(true);
});

test("catchup rejects oversized sync cursors before wasm work", async () => {
  const tables: Record<string, { last_seen_updated_at: number | null; permission_hash: string }> = {};
  for (let index = 0; index < 513; index += 1) {
    tables[`table_${index}`] = { last_seen_updated_at: null, permission_hash: "perm" };
  }
  const db = {
    execute: mock(async () => ({ rows: [] })),
    batch: mock(async () => ([])),
  };

  await expect(catchup(db as any, { tables }, {}, 1000)).rejects.toThrow("max is 512");
  expect(db.execute).not.toHaveBeenCalled();
});

test("catchup rejects oversized sync cursor permission hashes", async () => {
  const db = {
    execute: mock(async () => ({ rows: [] })),
    batch: mock(async () => ([])),
  };

  await expect(catchup(db as any, {
    tables: {
      maps: { last_seen_updated_at: null, permission_hash: "x".repeat(257) },
    },
  }, {}, 1000)).rejects.toThrow("permission_hash");
  expect(db.execute).not.toHaveBeenCalled();
});
