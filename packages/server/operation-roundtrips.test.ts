import { expect, test } from "bun:test";
import { createClient } from "@libsql/client";
import { z } from "zod";
import { run } from "./query";

// Exercise the real HTTP driver against a minimal Hrana v2 response fixture.
// SQLite execution/rollback is covered separately in query.test.ts. This test
// measures actual fetch boundaries, not calls to a mocked Transaction.batch.
function transport(options: { reject?: boolean; loseCommit?: boolean } = {}) {
  const requests: any[] = [];
  const sqlCache = new Map<number, string>();
  const result = (columns: string[] = [], rows: any[][] = []) => ({
    cols: columns.map(name => ({ name, decltype: null })), rows,
    affected_row_count: 0, last_insert_rowid: null,
  });
  const integer = (value: number) => ({ type: "integer", value: String(value) });
  const fetch = async (input: any) => {
    const request = new Request(input);
    // Negotiate the supported JSON protocol; exclude connection setup from
    // transaction request counts.
    if (request.method === "GET") return new Response(null, { status: 404 });
    const body = await request.json() as any;
    requests.push(body);
    let closed = false;
    const results = body.requests.map((entry: any) => {
      if (entry.type === "store_sql") {
        sqlCache.set(entry.sql_id, entry.sql);
        return { type: "ok", response: { type: "store_sql" } };
      }
      if (entry.type === "close") {
        closed = true;
        return { type: "ok", response: { type: "close" } };
      }
      if (entry.type === "batch") {
        const step_results = entry.batch.steps.map((step: any) => {
          const sql = (step.stmt.sql ?? sqlCache.get(step.stmt.sql_id)) as string;
          if (sql === "select changes() as count") {
            return result(["count"], [[integer(options.reject ? 0 : 1)]]);
          }
          if (sql.includes("update _pyre_sync")) {
            return result(["database_epoch", "server_revision"], [[{ type: "text", value: "epoch" }, integer(1)]]);
          }
          return result();
        });
        return { type: "ok", response: { type: "batch", result: { step_results, step_errors: [] } } };
      }
      if (entry.type === "execute") {
        if (entry.stmt.sql === "COMMIT" && options.loseCommit) throw new Error("Commit response lost");
        return { type: "ok", response: { type: "execute", result: result() } };
      }
      throw new Error(`Unexpected protocol request: ${entry.type}`);
    });
    return Response.json({ baton: closed ? null : "transaction", base_url: null, results });
  };
  return { requests, fetch: fetch as typeof globalThis.fetch };
}

const edit = {
  id: "edit", primary_db: "Main",
  sql: [{ include: false, params: ["id"], sql: "update notes set body = 'edited' where id = $id" }],
  generatedEdit: { writeStatement: 0 },
  syncEffects: { sql: true, syncSql: true },
  session_args: [], optional_input_args: [], json_input_args: [],
  InputValidator: z.object({ id: z.number() }), SessionValidator: z.object({}),
};

for (const size of [1, 10, 100]) {
  test(`${size} composed edits use one HTTP execution request and one commit`, async () => {
    const wire = transport();
    const db = createClient({ url: "http://pyre.invalid", fetch: wire.fetch });
    try {
      const result = await run(db, { edit }, Array.from({ length: size }, (_, id) => ({ queryId: "edit", input: { id } })), undefined, {},
        undefined, undefined, undefined, { allocateSyncRevision: true });
      expect(result.kind).toBe("success");
      expect(result.response).toHaveLength(size);
      expect(wire.requests).toHaveLength(2);
      const steps = wire.requests[0].requests.find((entry: any) => entry.type === "batch").batch.steps;
      expect(steps[0].stmt.sql).toBe("BEGIN IMMEDIATE");
      expect(steps).toHaveLength(1 + size * 2 + 1);
      expect(wire.requests[1].requests.map((entry: any) => entry.stmt?.sql ?? entry.type)).toEqual(["COMMIT", "close"]);
    } finally { db.close(); }
  });
}

test("failed cardinality uses a rollback request instead of commit", async () => {
  const wire = transport({ reject: true });
  const db = createClient({ url: "http://pyre.invalid", fetch: wire.fetch });
  try {
    const result = await run(db, { edit }, [{ queryId: "edit", input: { id: 99 } }], undefined, {});
    expect(result.error).toMatchObject({ errorType: "TransactionFailed", operationIndex: 0 });
    expect(wire.requests).toHaveLength(2);
    expect(wire.requests[1].requests[0].stmt.sql).toBe("ROLLBACK");
  } finally { db.close(); }
});

test("lost HTTP commit response reports unknown without replaying execution", async () => {
  const wire = transport({ loseCommit: true });
  const db = createClient({ url: "http://pyre.invalid", fetch: wire.fetch });
  try {
    const result = await run(db, { edit }, [{ queryId: "edit", input: { id: 1 } }], undefined, {});
    expect(result.error?.errorType).toBe("OutcomeUnknown");
    expect(wire.requests).toHaveLength(2);
    expect(wire.requests[1].requests[0].stmt.sql).toBe("COMMIT");
  } finally { db.close(); }
});
