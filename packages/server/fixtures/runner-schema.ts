// Isolate real WASM from the module mocks used by other server tests.
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createClient, type TransactionMode } from "@libsql/client";
import { z } from "zod";
import initWasm from "../wasm/pyre_wasm.js";
import { ensureDatabase, loadSchemaFromDatabase } from "../schema";
import { catchup } from "../sync";
import { toRunner } from "../runtime/runner";
import { databases } from "./compiled-batch/generated/databases";
import { meta } from "./compiled-batch/generated/queries/metadata/entryCreate";
import { sql } from "./compiled-batch/generated/queries/sql/entryCreate";

await initWasm({ module_or_path: readFileSync(new URL("../wasm/pyre_wasm_bg.wasm", import.meta.url)) });
const directory = mkdtempSync(join(tmpdir(), "pyre-runner-schema-"));
const db = createClient({ url: `file:${join(directory, "test.db")}` });
try {
  const source = databases._default.schemaSource;
  await ensureDatabase(db, "_default", source);
  const session = { userId: 7, role: { _type: "Member" }, unrelated: "required" };
  const input = { id: "01890f6c-7b80-7000-8000-000000000001", release: "release", enabled: true, count: 1,
    role: { _type: "Member" }, details: { _type: "Note", count: 2, enabled: false } };
  const create = toRunner<any, any>(meta, sql);
  const result = await create(db, session, input);
  assert.equal(result.entry[0].id, input.id);
  assert.equal(result.entry[0].enabled, true);
  assert.ok(result.entry[0].updatedAt instanceof Date);

  const invalidReturn = toRunner({ ...meta, ReturnData: z.never() }, sql);
  await assert.rejects(invalidReturn(db, session, { ...input, id: "01890f6c-7b80-7000-8000-000000000002" }), /return data/);
  assert.equal((await db.execute("select count(*) as n from entries")).rows[0].n, 1);

  const readMeta = { ...meta, operation: "query" as const, generatedEdit: undefined,
    InputValidator: z.object({}), ReturnData: z.object({ entry: z.array(z.object({ id: z.string() })) }) };
  const read = toRunner<any, any>(readMeta, [{ include: true, params: [],
    sql: "select json_group_array(json_object('id', id)) as entry from entries" }]);
  assert.deepEqual(await read(db, session, {}), { entry: [{ id: input.id }] });
  await loadSchemaFromDatabase("memory-guard", db);
  const memory = createClient({ url: "file::memory:" });
  try {
    await memory.execute("create table marker(value text)");
    await memory.execute("insert into marker values ('preserved')");
    const transaction = memory.transaction.bind(memory);
    let transactions = 0;
    memory.transaction = (mode?: TransactionMode) => { transactions++; return transaction(mode); };
    await assert.rejects(read(memory, session, {}), /in-memory transaction/);
    await assert.rejects(catchup(memory, { tables: {} }, session, 100, "memory-guard"), /in-memory transaction/);
    assert.equal(transactions, 0, "reject before the adapter detaches its connection");
    assert.deepEqual((await memory.execute("select value from marker")).rows, [{ value: "preserved" }]);
  } finally { memory.close(); }
  for (const schemaContracts of [undefined, {}, { Other: "wrong" }, { _default: "" }]) {
    await assert.rejects(toRunner({ ...readMeta, schemaContracts } as any, [])(db, session, {}), /schema contracts/);
  }
  await assert.rejects(toRunner({ ...readMeta, attached_dbs: ["Auth"] }, [])(db, session, {}), /schema contracts/);

  // Permissions change without changing the physical tables or compiled SQL.
  const revoked = source.replace("@public", "@allow(query, insert, update, delete) { False }");
  assert.notEqual(revoked, source);
  await ensureDatabase(db, "_default", revoked);
  await assert.rejects(read(db, session, {}));
  await assert.rejects(create(db, session, { ...input, id: "01890f6c-7b80-7000-8000-000000000003" }));
  assert.equal((await db.execute("select count(*) as n from entries")).rows[0].n, 1);
} finally {
  db.close();
  rmSync(directory, { recursive: true, force: true });
}
