// Run in a fresh process so bun:test's WASM mocks cannot satisfy this integration check.
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createClient } from "@libsql/client";
import { z } from "zod";
import initWasm, * as wasm from "../wasm/pyre_wasm.js";
import { ensureDatabase, loadSchemaFromDatabase } from "../schema";
import { catchupReplacement, runWithSync } from "../query-sync";
import { runBatch, type BatchManifest } from "../query";
import { catchup } from "../sync";
import { databases } from "./compiled-batch/generated/databases";
import { compiledContract, manifestVersion } from "./compiled-batch/generated/manifest";
import { SessionValidator } from "./compiled-batch/generated/decode";
import { meta } from "./compiled-batch/generated/queries/metadata/entryCreate";
import { sql, syncSql } from "./compiled-batch/generated/queries/sql/entryCreate";

await initWasm({ module_or_path: readFileSync(new URL("../wasm/pyre_wasm_bg.wasm", import.meta.url)) });
const directory = mkdtempSync(join(tmpdir(), "pyre-real-wasm-"));
const db = createClient({ url: `file:${join(directory, "test.db")}` });
try {
  await ensureDatabase(db, "_default", databases._default.schemaSource);
  await loadSchemaFromDatabase("real", db);
  assert.equal(wasm.get_schema_compiled_contract(), compiledContract);
  const manifest: BatchManifest = { version: 1, manifestVersion, compiledContract, replacementContracts: { _default: compiledContract }, SessionValidator,
    queries: { [meta.id]: { ...meta, sql, syncSql } } };
  const authority = { databaseId: "real", namespace: "_default", manifest: manifestVersion, instance: "tab", authGeneration: 1 };
  const epoch = (await db.execute("select database_epoch from _pyre_sync")).rows[0].database_epoch as string;
  const request = { version: 1 as const, ...authority, databaseEpoch: epoch, requestId: "read", target: 1 };
  const session = { userId: 7, role: { _type: "Member" }, unrelated: "required" };
  const input = { id: "00000000-0000-4000-8000-000000000001", release: "00000000-0000-4000-8000-000000000002", enabled: true, count: 1, role: { _type: "Member" }, details: { _type: "Note", count: 2, enabled: false } };
  const accepted = await runBatch(db, manifest, authority, { version: 1, ...authority, databaseEpoch: epoch,
    requestId: "write", sequence: 1, operations: [{ operation: meta.id, input }] }, session);
  assert.equal(accepted.kind, "success");
  const replace = () => catchupReplacement(db, manifest, authority, request, session);
  const snapshot = await replace();
  assert.equal(snapshot.kind, "success", JSON.stringify(snapshot));
  if (snapshot.kind !== "success") throw Error("No replacement");
  assert.equal(snapshot.response.serverRevision, 1);
  assert.deepEqual(snapshot.response.tables.entries.rows[0], { ...input, enabled: 1, updatedAt: (snapshot.response.tables.entries.rows[0] as any).updatedAt });
  for (const corruption of [
    "update entries set role = 'Unknown'",
    "update entries set enabled = 8",
    "update entries set details = jsonb('{\"_type\":\"Unknown\"}')",
    "update entries set details = jsonb('{\"_type\":\"Note\",\"count\":2}')",
  ]) {
    await db.execute(corruption);
    assert.deepEqual(await replace(), { kind: "error", error: { errorType: "ReplacementUnavailable", message: "ReplacementUnavailable" } });
    await db.execute("update entries set role = 'Member', enabled = 1, details = jsonb('{\"_type\":\"Note\",\"count\":2,\"enabled\":false}')");
  }
  assert.equal((await replace()).kind, "success");
  assert.equal((await catchupReplacement(db, { ...manifest, replacementContracts: { _default: "wrong" } }, authority, request, session)).kind, "error");

  const linked = createClient({ url: `file:${join(directory, "linked.db")}` });
  try {
    const source = `session {
    userId Int
}
record Membership {
    id Int @id
    workspaceId Int
    userId Int
    @allow(query) { False }
    @allow(insert, update, delete) { True }
}
record Workspace {
    id Int @id
    memberships @link(Membership.workspaceId)
    @allow(query) { exists memberships { userId == Session.userId } }
    @allow(insert, update, delete) { True }
}`;
    await ensureDatabase(linked, "_default", source);
    await loadSchemaFromDatabase("linked", linked);
    const linkedManifest: BatchManifest = { version: 1, manifestVersion: "linked-manifest", replacementContracts: { _default: wasm.get_schema_compiled_contract() },
      SessionValidator: z.object({ userId: z.number().int() }), queries: {} };
    await linked.execute("insert into workspaces(id,updatedAt) values(1,0),(2,0)");
    await linked.execute("insert into memberships(id,workspaceId,userId,updatedAt) values(1,1,7,0),(2,2,8,0)");
    const linkedAuthority = { ...authority, databaseId: "linked", manifest: linkedManifest.manifestVersion };
    const linkedEpoch = (await linked.execute("select database_epoch from _pyre_sync")).rows[0].database_epoch as string;
    const linkedRequest = { ...request, ...linkedAuthority, databaseEpoch: linkedEpoch, target: 0 };
    const before = await catchupReplacement(linked, linkedManifest, linkedAuthority, linkedRequest, { userId: 7 });
    assert.equal(before.kind, "success", JSON.stringify(before));
    if (before.kind !== "success") throw Error("No linked replacement");
    assert.deepEqual(before.response.tables.workspaces.rows, [{ id: 1, updatedAt: 0 }]);
    assert.deepEqual(before.response.tables.memberships.rows, []);
    await assert.rejects(catchup(linked, { tables: {} }, { userId: 7 }, 1000, "linked"), /ReplacementRequired|replacement/i);
    const command = { id: "revoke", operation: "delete", primary_db: "_default", session_args: ["userId"],
      optional_input_args: [], json_input_args: [], InputValidator: z.object({}), SessionValidator: linkedManifest.SessionValidator,
      sql: [{ include: false, params: ["session_userId"], sql: "delete from memberships where userId = $session_userId" }] };
    const fence = { ...linkedAuthority, databaseEpoch: linkedEpoch, instance: "reader", authGeneration: 9 };
    const result = await runWithSync(linked, { revoke: command }, "revoke", {}, { userId: 7 }, new Map([
      ["legacy", { session: { userId: 7 } }], ["fenced", { session: { userId: 7 }, fence }],
    ]), "linked");
    const sent: any[] = [];
    await result.sync((id, message) => sent.push({ id, message }));
    assert.equal(sent.length, 2);
    assert.equal(sent.find(item => item.id === "legacy").message.type, "syncRequired");
    assert.deepEqual(sent.find(item => item.id === "fenced").message, { ...fence, type: "syncRequired", serverRevision: 1,
      reconciliation: { kind: "replaceRequired", atLeast: 1, invalidate: true, minimumSafeRevision: 1 } });
    const after = await catchupReplacement(linked, linkedManifest, linkedAuthority, { ...linkedRequest, target: 1 }, { userId: 7 });
    assert.equal(after.kind, "success", JSON.stringify(after));
    if (after.kind !== "success") throw Error("No revoked replacement");
    assert.deepEqual(after.response.tables.workspaces.rows, []);
    const transaction = db.transaction.bind(db);
    db.transaction = async mode => {
      const tx = await transaction(mode);
      const execute = tx.execute.bind(tx);
      tx.execute = async statement => {
        const result = await execute(statement);
        // Replace the same database cache entry while the original snapshot is in flight.
        await loadSchemaFromDatabase("real", linked);
        return result;
      };
      return tx;
    };
    assert.equal((await replace()).kind, "success");
    assert.deepEqual(await replace(), { kind: "error", error: { errorType: "InvalidRequest", message: "InvalidRequest" } });
  } finally { linked.close(); }

  for (const jsonType of ["Json<Int>", "Json"]) {
    for (const rhs of jsonType === "Json" ? ["Session.value"] : ["Session.value", "7", "null"]) {
      for (const operator of ["==", "!="]) {
        const id = `json-permission-${jsonType}-${rhs}-${operator}`;
        const permissions = createClient({ url: `file:${join(directory, `${id}.db`)}` });
        try {
          const source = `session {
    value ${jsonType}?
}
record Membership {
    id Int @id
    workspaceId Int
    value ${jsonType}?
    @allow(query) { value ${operator} ${rhs} }
    @allow(insert, update, delete) { False }
}
record Workspace {
    id Int @id
    memberships @link(Membership.workspaceId)
    @allow(query) { exists memberships { value ${operator} ${rhs} } }
    @allow(insert, update, delete) { False }
}`;
          await ensureDatabase(permissions, "_default", source);
          await loadSchemaFromDatabase(id, permissions);
          const manifest: BatchManifest = { version: 1, manifestVersion: id, replacementContracts: { _default: wasm.get_schema_compiled_contract() },
            SessionValidator: z.object({ value: z.number().int().nullable() }), queries: {} };
          await permissions.execute("insert into workspaces(id) values(1),(2),(3)");
          await permissions.execute("insert into memberships(id,workspaceId,value) values(1,1,jsonb('7')),(2,2,jsonb('8')),(3,3,NULL)");
          const binding = { ...authority, databaseId: id, manifest: id };
          const epoch = (await permissions.execute("select database_epoch from _pyre_sync")).rows[0].database_epoch as string;
          for (const value of [7, null]) {
            const result = await catchupReplacement(permissions, manifest, binding,
              { ...request, ...binding, databaseEpoch: epoch, target: 0 }, { value });
            assert.equal(result.kind, "success", JSON.stringify(result));
            if (result.kind !== "success") throw Error("No JSON permission replacement");
            const match = rhs === "null" || (rhs === "Session.value" && value === null) ? 3 : 1;
            const expected = operator === "==" ? [match] : [1, 2, 3].filter(id => id !== match);
            for (const table of ["memberships", "workspaces"]) {
              assert.deepEqual(result.response.tables[table].rows.map((row: any) => row.id).sort(), expected,
                `${jsonType}: ${operator} ${rhs}, session=${value}, ${table}`);
            }
          }
        } finally { permissions.close(); }
      }
    }
  }

  const strings = createClient({ url: `file:${join(directory, "strings.db")}` });
  try {
    await ensureDatabase(strings, "_default", `type Payload = Text { value Json<String> }
record Item {
    id Int @id
    label Json<String>
    raw Json
    optional Json<String?>
    payload Payload
    @public
}`);
    await loadSchemaFromDatabase("strings", strings);
    const manifest: BatchManifest = { version: 1, manifestVersion: "strings", replacementContracts: { _default: wasm.get_schema_compiled_contract() },
      SessionValidator: z.object({}), queries: {} };
    const values = ["hello", "7", "null", "true", "[1]", '{"x":1}', '"quoted"', "{invalid"];
    for (const [id, value] of values.entries()) {
      const encoded = JSON.stringify(value);
      await strings.execute({ sql: "insert into items(id,label,raw,optional,payload,payload__value) values(?,jsonb(?),jsonb(?),jsonb('null'),'Text',jsonb(?))",
        args: [id, encoded, encoded, encoded] });
    }
    const binding = { ...authority, databaseId: "strings", manifest: "strings" };
    const epoch = (await strings.execute("select database_epoch from _pyre_sync")).rows[0].database_epoch as string;
    const result = await catchupReplacement(strings, manifest, binding,
      { ...request, ...binding, databaseEpoch: epoch, target: 0 }, {});
    assert.equal(result.kind, "success", JSON.stringify(result));
    if (result.kind !== "success") throw Error("No scalar JSON replacement");
    assert.equal(result.response.complete, true);
    assert.equal(result.response.tables.items.rows.length, values.length);
    for (const row of result.response.tables.items.rows as any[]) {
      const value = values[row.id];
      assert.equal(row.label, value);
      assert.equal(row.raw, value);
      assert.equal(row.optional, null);
      assert.deepEqual(row.payload, { _type: "Text", value });
    }
  } finally { strings.close(); }
} finally { db.close(); rmSync(directory, { recursive: true, force: true }); }
