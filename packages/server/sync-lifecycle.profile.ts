import { createClient, type Client, type InStatement, type InValue } from "@libsql/client";

declare const Bun: { env: Record<string, string | undefined> };

type PhaseTotals = Map<string, number>;

interface ProfileConfig {
  mode: "local" | "turso";
  url: string;
  authToken?: string;
  rows: number;
  pageSize: number;
  iterations: number;
  sessions: number;
  mimicRttMs: number;
  mimicBandwidthMbps: number;
}

interface CatchupCursor {
  lastSeenUpdatedAt: number | null;
  permissionHash: string;
}

const TABLE_NAME = "pyre_sync_profile_notes";
const PERMISSION_HASH = "public";

function readConfig(): ProfileConfig {
  const tursoUrl = Bun.env.TURSO_DATABASE_URL;
  const mode = tursoUrl ? "turso" : "local";
  const url = tursoUrl ?? Bun.env.SYNC_PROFILE_LOCAL_URL ?? "file::memory:";
  const authToken = Bun.env.TURSO_AUTH_TOKEN;

  if (mode === "turso" && Bun.env.SYNC_PROFILE_ALLOW_REMOTE_WRITES !== "1") {
    throw new Error(
      "Refusing to write to Turso without SYNC_PROFILE_ALLOW_REMOTE_WRITES=1",
    );
  }

  return {
    mode,
    url,
    authToken,
    rows: Number(Bun.env.SYNC_PROFILE_ROWS ?? 1_000),
    pageSize: Number(Bun.env.SYNC_PROFILE_PAGE_SIZE ?? 1_000),
    iterations: Number(Bun.env.SYNC_PROFILE_ITERATIONS ?? 10),
    sessions: Number(Bun.env.SYNC_PROFILE_SESSIONS ?? 25),
    mimicRttMs: Number(Bun.env.SYNC_PROFILE_MIMIC_RTT_MS ?? 20),
    mimicBandwidthMbps: Number(Bun.env.SYNC_PROFILE_MIMIC_BANDWIDTH_MBPS ?? 25),
  };
}

function nowMs(): number {
  return performance.now();
}

async function time<T>(totals: PhaseTotals, phase: string, fn: () => Promise<T> | T): Promise<T> {
  const startedAt = nowMs();
  try {
    return await fn();
  } finally {
    totals.set(phase, (totals.get(phase) ?? 0) + nowMs() - startedAt);
  }
}

function add(totals: PhaseTotals, phase: string, elapsedMs: number): void {
  totals.set(phase, (totals.get(phase) ?? 0) + elapsedMs);
}

async function setupDatabase(db: Client, config: ProfileConfig): Promise<void> {
  await db.batch([
    `drop table if exists ${TABLE_NAME}`,
    `create table ${TABLE_NAME} (
      id integer primary key,
      ownerId integer not null,
      body text not null,
      attrs text not null,
      updatedAt integer not null
    )`,
    `create table if not exists _pyre_sync (
      id integer primary key check (id = 1),
      database_epoch text not null,
      server_revision integer not null
    )`,
    "insert into _pyre_sync (id, database_epoch, server_revision) values (1, lower(hex(randomblob(16))), 0) on conflict(id) do nothing",
  ]);

  const chunkSize = 100;
  for (let start = 1; start <= config.rows; start += chunkSize) {
    const statements: InStatement[] = [];
    const end = Math.min(config.rows, start + chunkSize - 1);
    for (let id = start; id <= end; id += 1) {
      statements.push({
        sql: `insert into ${TABLE_NAME} (id, ownerId, body, attrs, updatedAt) values (?, ?, ?, ?, ?)`,
        args: [id, 1, `note-${id}`, JSON.stringify({ index: id, tag: "bench" }), id],
      });
    }
    await db.batch(statements);
  }
}

async function profileCatchup(db: Client, config: ProfileConfig): Promise<PhaseTotals> {
  const totals: PhaseTotals = new Map();
  const cursor: CatchupCursor = { lastSeenUpdatedAt: null, permissionHash: "" };

  for (let iteration = 0; iteration < config.iterations; iteration += 1) {
    let statusSql = "";
    await time(totals, "catchup.status_sql_build", () => {
      const lastSeen = cursor.lastSeenUpdatedAt ?? "null";
      statusSql = `select '${TABLE_NAME}' as table_name, 0 as sync_layer, '${PERMISSION_HASH}' as permission_hash, ${lastSeen} as last_seen_updated_at, max(updatedAt) as max_updated_at from ${TABLE_NAME}`;
    });

    const statusResult = await time(totals, "catchup.status_query", () => db.execute(statusSql));

    let needsSync = false;
    await time(totals, "catchup.status_parse", () => {
      const row = statusResult.rows[0];
      const maxUpdatedAt = Number(row?.max_updated_at ?? 0);
      needsSync = cursor.permissionHash !== PERMISSION_HASH || cursor.lastSeenUpdatedAt == null || maxUpdatedAt > cursor.lastSeenUpdatedAt;
    });

    let dataSql = "";
    await time(totals, "catchup.data_sql_build", () => {
      if (!needsSync) {
        dataSql = "";
        return;
      }
      const lastSeenWhere = cursor.permissionHash === PERMISSION_HASH && cursor.lastSeenUpdatedAt != null
        ? `where updatedAt > ${cursor.lastSeenUpdatedAt}`
        : "";
      dataSql = `select id, ownerId, body, attrs, updatedAt from ${TABLE_NAME} ${lastSeenWhere} order by updatedAt asc limit ${config.pageSize + 1}`;
    });

    const dataResult = await time(totals, "catchup.data_query", async () => {
      if (!dataSql) {
        return { rows: [] as any[] };
      }
      return await db.execute(dataSql);
    });

    let rows: Array<Record<string, unknown>> = [];
    await time(totals, "catchup.materialize_rows", () => {
      rows = dataResult.rows.map((row) => ({
        id: Number(row.id),
        ownerId: Number(row.ownerId),
        body: row.body,
        attrs: JSON.parse(String(row.attrs)),
        updatedAt: Number(row.updatedAt),
      }));
    });

    let response: unknown;
    await time(totals, "catchup.shape_response", () => {
      const finalRows = rows.slice(0, config.pageSize);
      cursor.lastSeenUpdatedAt = finalRows.length > 0
        ? Number(finalRows[finalRows.length - 1].updatedAt)
        : cursor.lastSeenUpdatedAt;
      cursor.permissionHash = PERMISSION_HASH;
      response = {
        tables: {
          [TABLE_NAME]: {
            rows: finalRows,
            permission_hash: PERMISSION_HASH,
            last_seen_updated_at: cursor.lastSeenUpdatedAt,
          },
        },
        has_more: rows.length > config.pageSize,
      };
    });

    await time(totals, "catchup.revision_read", () => db.execute("select server_revision from _pyre_sync where id = 1"));
    await time(totals, "catchup.serialize_response", () => JSON.stringify(response));

    if (cursor.lastSeenUpdatedAt != null && cursor.lastSeenUpdatedAt >= config.rows) {
      cursor.lastSeenUpdatedAt = null;
      cursor.permissionHash = "";
    }
  }

  return totals;
}

async function profileMutationLifecycle(db: Client, config: ProfileConfig): Promise<PhaseTotals> {
  const totals: PhaseTotals = new Map();

  for (let iteration = 0; iteration < config.iterations; iteration += 1) {
    const id = config.rows + iteration + 1;
    let mutationStatement: { sql: string; args: InValue[] };
    await time(totals, "mutation.sql_build", () => {
      mutationStatement = {
        sql: `insert into ${TABLE_NAME} (id, ownerId, body, attrs, updatedAt) values (?, ?, ?, ?, ?) returning id, ownerId, body, attrs, updatedAt`,
        args: [id, 1, `mutation-${id}`, JSON.stringify({ index: id, tag: "mutation" }), id],
      };
    });

    const mutationResult = await time(totals, "mutation.execute", () => db.execute(mutationStatement));

    let affectedRows: Array<Record<string, unknown>> = [];
    await time(totals, "mutation.affected_rows_parse", () => {
      affectedRows = mutationResult.rows.map((row) => ({
        id: Number(row.id),
        ownerId: Number(row.ownerId),
        body: row.body,
        attrs: JSON.parse(String(row.attrs)),
        updatedAt: Number(row.updatedAt),
      }));
    });

    let deltaMessages: unknown[] = [];
    await time(totals, "delta.filter_and_shape", () => {
      deltaMessages = Array.from({ length: config.sessions }, (_, sessionIndex) => ({
        sessionId: `session-${sessionIndex + 1}`,
        message: {
          type: "delta",
          data: [
            {
              table_name: TABLE_NAME,
              headers: ["id", "ownerId", "body", "attrs", "updatedAt"],
              rows: affectedRows.map((row) => [row.id, row.ownerId, row.body, row.attrs, row.updatedAt]),
            },
          ],
        },
      }));
    });

    const revisionResult = await time(totals, "delta.revision_allocate", () => db.execute("update _pyre_sync set server_revision = server_revision + 1 where id = 1 returning server_revision"));
    const serverRevision = Number(revisionResult.rows[0]?.server_revision ?? 0);

    await time(totals, "delta.stamp_messages", () => {
      deltaMessages = deltaMessages.map((entry: any) => ({
        ...entry,
        message: { ...entry.message, serverRevision },
      }));
    });

    await time(totals, "delta.serialize_messages", () => JSON.stringify(deltaMessages));
  }

  return totals;
}

async function profileLiveSyncFanout(db: Client, config: ProfileConfig): Promise<PhaseTotals> {
  const totals: PhaseTotals = new Map();
  const fanoutSizes = [1, 10, 100, 1000];
  const affectedRows = [[1, 1, "mutation-1", { index: 1, tag: "mutation" }, 1]];

  for (const sessionCount of fanoutSizes) {
    for (let iteration = 0; iteration < config.iterations; iteration += 1) {
      const originSessionId = "session-1";
      let recipients: string[] = [];
      await time(totals, `fanout.${sessionCount}.skip_origin`, () => {
        recipients = Array.from({ length: sessionCount }, (_, index) => `session-${index + 1}`)
          .filter((sessionId) => sessionId !== originSessionId);
      });

      let message: unknown;
      await time(totals, `fanout.${sessionCount}.shape_once`, () => {
        message = {
          type: "delta",
          serverRevision: iteration + 1,
          data: [
            {
              table_name: TABLE_NAME,
              headers: ["id", "ownerId", "body", "attrs", "updatedAt"],
              rows: affectedRows,
            },
          ],
        };
      });

      await time(totals, `fanout.${sessionCount}.send_object_refs`, () => {
        const sent: unknown[] = [];
        for (const sessionId of recipients) {
          sent.push([sessionId, message]);
        }
      });

      await time(totals, `fanout.${sessionCount}.stringify_per_client`, () => {
        const sent: string[] = [];
        for (const sessionId of recipients) {
          sent.push(`${sessionId}:${JSON.stringify(message)}`);
        }
      });

      await time(totals, `fanout.${sessionCount}.stringify_once`, () => {
        const serialized = JSON.stringify(message);
        const sent: string[] = [];
        for (const sessionId of recipients) {
          sent.push(`${sessionId}:${serialized}`);
        }
      });
    }
  }

  await time(totals, "revision.current_update_returning", async () => {
    for (let iteration = 0; iteration < config.iterations; iteration += 1) {
      await db.execute("update _pyre_sync set server_revision = server_revision + 1 where id = 1 returning server_revision");
    }
  });
  await time(totals, "revision.legacy_lazy_ensure", async () => {
    for (let iteration = 0; iteration < config.iterations; iteration += 1) {
      await db.execute("create table if not exists _pyre_sync (id integer primary key check (id = 1), database_epoch text not null, server_revision integer not null)");
      await db.execute("insert into _pyre_sync (id, database_epoch, server_revision) values (1, lower(hex(randomblob(16))), 0) on conflict(id) do nothing");
      await db.execute("update _pyre_sync set server_revision = server_revision + 1 where id = 1 returning server_revision");
    }
  });

  return totals;
}

async function profileMutationModeComparison(db: Client, config: ProfileConfig): Promise<PhaseTotals> {
  const totals: PhaseTotals = new Map();

  for (let iteration = 0; iteration < config.iterations; iteration += 1) {
    const updatedAt = config.rows + 10_000 + iteration;
    const body = `mode-${iteration}`;

    await time(totals, "current_sync.update_returning", () => db.execute({
      sql: `update ${TABLE_NAME} set body = ?, attrs = ?, updatedAt = ? where id = 1 returning *`,
      args: [body, JSON.stringify({ index: 1, tag: "current-sync" }), updatedAt],
    }));
    const currentSyncAffected = await time(totals, "current_sync.affected_rows_sql", () => db.execute(affectedRowsSql()));
    const currentSyncResult = await time(totals, "current_sync.typed_result_sql", () => db.execute(typedResultSql()));
    await time(totals, "current_sync.format_response", () => {
      const sync = parseAffectedRows(currentSyncAffected.rows[0]?._affectedRows);
      const result = parseTypedResult(currentSyncResult.rows[0]?.note);
      JSON.stringify({ result: { note: result }, serverRevision: iteration + 1, sync });
    });

    await time(totals, "sync_only.update_returning", () => db.execute({
      sql: `update ${TABLE_NAME} set body = ?, attrs = ?, updatedAt = ? where id = 1 returning *`,
      args: [body, JSON.stringify({ index: 1, tag: "sync-only" }), updatedAt],
    }));
    const syncOnlyAffected = await time(totals, "sync_only.affected_rows_sql", () => db.execute(affectedRowsSql()));
    await time(totals, "sync_only.format_response", () => {
      const sync = parseAffectedRows(syncOnlyAffected.rows[0]?._affectedRows);
      JSON.stringify({ serverRevision: iteration + 1, sync });
    });

    await time(totals, "current_normal.update_returning", () => db.execute({
      sql: `update ${TABLE_NAME} set body = ?, attrs = ?, updatedAt = ? where id = 1 returning *`,
      args: [body, JSON.stringify({ index: 1, tag: "current-normal" }), updatedAt],
    }));
    const currentNormalAffected = await time(totals, "current_normal.affected_rows_sql", () => db.execute(affectedRowsSql()));
    const currentNormalResult = await time(totals, "current_normal.typed_result_sql", () => db.execute(typedResultSql()));
    await time(totals, "current_normal.format_response", () => {
      parseAffectedRows(currentNormalAffected.rows[0]?._affectedRows);
      const result = parseTypedResult(currentNormalResult.rows[0]?.note);
      JSON.stringify({ result: { note: result } });
    });

    await time(totals, "normal_only.update", () => db.execute({
      sql: `update ${TABLE_NAME} set body = ?, attrs = ?, updatedAt = ? where id = 1`,
      args: [body, JSON.stringify({ index: 1, tag: "normal-only" }), updatedAt],
    }));
    const normalOnlyResult = await time(totals, "normal_only.typed_result_sql", () => db.execute(typedResultSql()));
    await time(totals, "normal_only.format_response", () => {
      const result = parseTypedResult(normalOnlyResult.rows[0]?.note);
      JSON.stringify({ result: { note: result } });
    });
  }

  return totals;
}

function typedResultSql(): string {
  return `select coalesce(json_group_array(json_object('id', id, 'ownerId', ownerId, 'body', body, 'attrs', json(attrs), 'updatedAt', updatedAt)), json('[]')) as note from ${TABLE_NAME} where id = 1`;
}

function affectedRowsSql(): string {
  return `select json_group_array(json(affected_row)) as _affectedRows from (select json_object('table_name', '${TABLE_NAME}', 'headers', json_array('id', 'ownerId', 'body', 'attrs', 'updatedAt'), 'rows', json_group_array(json_array(id, ownerId, body, json(attrs), updatedAt))) as affected_row from ${TABLE_NAME} where id = 1)`;
}

function parseAffectedRows(raw: unknown): unknown {
  return typeof raw === "string" ? JSON.parse(raw) : raw;
}

function parseTypedResult(raw: unknown): unknown {
  return typeof raw === "string" ? JSON.parse(raw) : raw;
}

async function profileDataQueryAnatomy(db: Client, config: ProfileConfig): Promise<PhaseTotals> {
  const totals: PhaseTotals = new Map();
  const iterations = config.iterations;
  const limit = config.pageSize + 1;

  for (let iteration = 0; iteration < iterations; iteration += 1) {
    await time(totals, "sqlite.execute_select_1", () => db.execute("select 1"));
    await time(totals, "sqlite.count_matching_rows", () => db.execute(`select count(*) as count from ${TABLE_NAME}`));
    await time(totals, "sqlite.page_ids_only", () => db.execute(`select id from ${TABLE_NAME} order by updatedAt asc limit ${limit}`));
    await time(totals, "sqlite.page_scalar_columns", () => db.execute(`select id, ownerId, updatedAt from ${TABLE_NAME} order by updatedAt asc limit ${limit}`));
    await time(totals, "sqlite.page_full_raw", () => db.execute(`select id, ownerId, body, attrs, updatedAt from ${TABLE_NAME} order by updatedAt asc limit ${limit}`));
    await time(totals, "sqlite.page_full_json_column", () => db.execute(`select id, ownerId, body, json(attrs) as attrs, updatedAt from ${TABLE_NAME} order by updatedAt asc limit ${limit}`));
    await time(totals, "sqlite.page_json_object_rows", () => db.execute(`select json_object('id', id, 'ownerId', ownerId, 'body', body, 'attrs', json(attrs), 'updatedAt', updatedAt) as row from ${TABLE_NAME} order by updatedAt asc limit ${limit}`));
    await time(totals, "sqlite.page_json_array_rows", () => db.execute(`select json_array(id, ownerId, body, json(attrs), updatedAt) as row from ${TABLE_NAME} order by updatedAt asc limit ${limit}`));
    await time(totals, "sqlite.aggregate_json_objects", () => db.execute(`select json_group_array(json_object('id', id, 'ownerId', ownerId, 'body', body, 'attrs', json(attrs), 'updatedAt', updatedAt)) as rows_json from (select id, ownerId, body, attrs, updatedAt from ${TABLE_NAME} order by updatedAt asc limit ${limit})`));
    await time(totals, "sqlite.aggregate_affected_rows_shape", () => db.execute(`select json_group_array(json(affected_row)) as _affectedRows from (select json_object('table_name', '${TABLE_NAME}', 'headers', json_array('id', 'ownerId', 'body', 'attrs', 'updatedAt'), 'rows', json_group_array(json_array(id, ownerId, body, json(attrs), updatedAt))) as affected_row from (select id, ownerId, body, attrs, updatedAt from ${TABLE_NAME} order by updatedAt asc limit ${limit}))`));
    await time(totals, "sqlite.page_full_no_order", () => db.execute(`select id, ownerId, body, attrs, updatedAt from ${TABLE_NAME} limit ${limit}`));
  }

  return totals;
}

interface CatchupComparisonResult {
  rowTotals: PhaseTotals;
  aggregateTotals: PhaseTotals;
  rowDbPayloadBytes: number;
  aggregateDbPayloadBytes: number;
}

async function profileCatchupApproaches(db: Client, config: ProfileConfig): Promise<CatchupComparisonResult> {
  const rowTotals: PhaseTotals = new Map();
  const aggregateTotals: PhaseTotals = new Map();
  const limit = config.pageSize + 1;
  let rowDbPayloadBytes = 0;
  let aggregateDbPayloadBytes = 0;

  for (let iteration = 0; iteration < config.iterations; iteration += 1) {
    const rowResult = await time(rowTotals, "row.execute", () => db.execute(`select id, ownerId, body, json(attrs) as attrs, updatedAt from ${TABLE_NAME} order by updatedAt asc limit ${limit}`));
    rowDbPayloadBytes += new TextEncoder().encode(JSON.stringify(rowResult.rows)).byteLength;
    let rowResponse: unknown;
    await time(rowTotals, "row.materialize_and_shape", () => {
      const rows = rowResult.rows.slice(0, config.pageSize).map((row) => ({
        id: Number(row.id),
        ownerId: Number(row.ownerId),
        body: row.body,
        attrs: JSON.parse(String(row.attrs)),
        updatedAt: Number(row.updatedAt),
      }));
      rowResponse = {
        rows,
        has_more: rowResult.rows.length > config.pageSize,
      };
    });
    await time(rowTotals, "row.serialize_response", () => JSON.stringify(rowResponse));

    const aggregateResult = await time(aggregateTotals, "aggregate.execute", () => db.execute(`select json_group_array(json_object('id', id, 'ownerId', ownerId, 'body', body, 'attrs', json(attrs), 'updatedAt', updatedAt)) as rows_json from (select id, ownerId, body, attrs, updatedAt from ${TABLE_NAME} order by updatedAt asc limit ${limit})`));
    const aggregateRaw = aggregateResult.rows[0]?.rows_json;
    aggregateDbPayloadBytes += new TextEncoder().encode(typeof aggregateRaw === "string" ? aggregateRaw : "").byteLength;
    let aggregateResponse: unknown;
    await time(aggregateTotals, "aggregate.parse_and_shape", () => {
      const rows = typeof aggregateRaw === "string" && aggregateRaw.length > 0
        ? JSON.parse(aggregateRaw)
        : [];
      aggregateResponse = {
        rows: rows.slice(0, config.pageSize),
        has_more: rows.length > config.pageSize,
      };
    });
    await time(aggregateTotals, "aggregate.serialize_response", () => JSON.stringify(aggregateResponse));
  }

  return {
    rowTotals,
    aggregateTotals,
    rowDbPayloadBytes: rowDbPayloadBytes / config.iterations,
    aggregateDbPayloadBytes: aggregateDbPayloadBytes / config.iterations,
  };
}

function printTotals(title: string, totals: PhaseTotals, iterations: number): void {
  const totalMs = Array.from(totals.values()).reduce((sum, value) => sum + value, 0);
  console.log(`\n${title}`);
  console.log("phase, total_ms, avg_ms, percent");
  for (const [phase, elapsedMs] of Array.from(totals.entries()).sort((a, b) => b[1] - a[1])) {
    const percent = totalMs > 0 ? (elapsedMs / totalMs) * 100 : 0;
    console.log(`${phase}, ${elapsedMs.toFixed(3)}, ${(elapsedMs / iterations).toFixed(3)}, ${percent.toFixed(1)}%`);
  }
}

function sumTotals(totals: PhaseTotals): number {
  return Array.from(totals.values()).reduce((sum, value) => sum + value, 0);
}

function estimatedTransferMs(bytes: number, bandwidthMbps: number): number {
  return (bytes * 8) / (bandwidthMbps * 1_000_000) * 1_000;
}

function printCatchupApproachComparison(result: CatchupComparisonResult, config: ProfileConfig): void {
  const rowLocalMs = sumTotals(result.rowTotals) / config.iterations;
  const aggregateLocalMs = sumTotals(result.aggregateTotals) / config.iterations;
  const rowTransferMs = estimatedTransferMs(result.rowDbPayloadBytes, config.mimicBandwidthMbps);
  const aggregateTransferMs = estimatedTransferMs(result.aggregateDbPayloadBytes, config.mimicBandwidthMbps);

  console.log("\ncatch-up approach comparison");
  console.log("approach, local_avg_ms, avg_db_payload_bytes, mimic_remote_ms");
  console.log(`row-materialized, ${rowLocalMs.toFixed(3)}, ${result.rowDbPayloadBytes.toFixed(0)}, ${(rowLocalMs + config.mimicRttMs + rowTransferMs).toFixed(3)}`);
  console.log(`sqlite-aggregate-json, ${aggregateLocalMs.toFixed(3)}, ${result.aggregateDbPayloadBytes.toFixed(0)}, ${(aggregateLocalMs + config.mimicRttMs + aggregateTransferMs).toFixed(3)}`);
  console.log(`mimic settings: rtt_ms=${config.mimicRttMs} bandwidth_mbps=${config.mimicBandwidthMbps}`);
}

const config = readConfig();
const db = createClient({ url: config.url, authToken: config.authToken });

console.log(`sync lifecycle profile: ${config.mode}`);
console.log(`url: ${config.mode === "turso" ? config.url : config.url}`);
console.log(`rows=${config.rows} pageSize=${config.pageSize} iterations=${config.iterations} sessions=${config.sessions}`);

const setupStartedAt = nowMs();
await setupDatabase(db, config);
console.log(`setup_ms=${(nowMs() - setupStartedAt).toFixed(3)}`);

printTotals("catch-up", await profileCatchup(db, config), config.iterations);
printTotals("data-query anatomy", await profileDataQueryAnatomy(db, config), config.iterations);
const catchupComparison = await profileCatchupApproaches(db, config);
printTotals("catch-up row-materialized approach", catchupComparison.rowTotals, config.iterations);
printTotals("catch-up sqlite-aggregate-json approach", catchupComparison.aggregateTotals, config.iterations);
printCatchupApproachComparison(catchupComparison, config);
printTotals("mutation-to-delta", await profileMutationLifecycle(db, config), config.iterations);
printTotals("live-sync fanout", await profileLiveSyncFanout(db, config), config.iterations);
printTotals("mutation mode comparison", await profileMutationModeComparison(db, config), config.iterations);

db.close();
