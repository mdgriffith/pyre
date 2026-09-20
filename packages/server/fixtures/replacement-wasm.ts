// Run in a fresh process so bun:test's WASM mocks cannot satisfy this integration check.
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { createClient } from "@libsql/client";
import { z } from "zod";
import initWasm, * as wasm from "../wasm/pyre_wasm.js";
import { ensureDatabase, loadSchemaFromDatabase } from "../schema";
import { catchupReplacement, runWithSync } from "../query-sync";
import { runBatch, type BatchManifest } from "../query";
import { catchup } from "../sync";
import { databases } from "./compiled-batch/generated/databases";
import { compiledContract, manifestVersion, replacementContracts } from "./compiled-batch/generated/manifest";
import { SessionValidator } from "./compiled-batch/generated/decode";
import { meta } from "./compiled-batch/generated/queries/metadata/entryCreate";
import { sql, syncSql } from "./compiled-batch/generated/queries/sql/entryCreate";

await initWasm({ module_or_path: readFileSync(new URL("../wasm/pyre_wasm_bg.wasm", import.meta.url)) });
const directory = mkdtempSync(new URL("../../../target/pyre-real-wasm-", import.meta.url));
const db = createClient({ url: `file:${join(directory, "test.db")}` });
try {
  await ensureDatabase(db, "_default", databases._default.schemaSource);
  await loadSchemaFromDatabase("real", db);
  assert.equal(wasm.get_schema_compiled_contract(), replacementContracts._default);
  const manifest: BatchManifest = { version: 1, manifestVersion, compiledContract, replacementContracts, SessionValidator,
    queries: { [meta.id]: { ...meta, sql, syncSql } } };
  const authority = { databaseId: "real", namespace: "_default", manifest: manifestVersion, instance: "tab", authGeneration: 1 };
  const epoch = (await db.execute("select database_epoch from _pyre_sync")).rows[0].database_epoch as string;
  const request = { version: 1 as const, ...authority, databaseEpoch: epoch, requestId: "read", target: 1 };
  const session = { userId: 7, role: { _type: "Member" }, unrelated: "required" };
  const input = { id: "01890f6c-7b80-7000-8000-000000000001", release: "00000000-0000-4000-8000-000000000002", enabled: true, count: 1, role: { _type: "Member" }, details: { _type: "Note", count: 2, enabled: false } };
  const accepted = await runBatch(db, manifest, authority, { version: 1, ...authority, databaseEpoch: epoch,
    requestId: "write", sequence: 1, operations: [{ operation: meta.id, input }] }, session);
  assert.equal(accepted.kind, "success");
  const replace = () => catchupReplacement(db, manifest, authority, request, session);
  const snapshot = await replace();
  assert.equal(snapshot.kind, "success", JSON.stringify(snapshot));
  if (snapshot.kind !== "success") throw Error("No replacement");
  assert.equal(snapshot.response.serverRevision, 1);
  assert.deepEqual(snapshot.response.tables.entries.rows[0], { ...input, updatedAt: (snapshot.response.tables.entries.rows[0] as any).updatedAt });
  await db.execute(`update entries set updatedAt = '1700000000', details = jsonb('{"_type":"Bundle","when":"2023-11-14T22:13:20Z","role":"Member","children":[{"_type":"Empty"}],"byName":{"first":{"_type":"Empty"}},"note":null}')`);
  const canonical = await replace();
  assert.equal(canonical.kind, "success", JSON.stringify(canonical));
  if (canonical.kind !== "success") throw Error("No canonical replacement");
  assert.equal((canonical.response.tables.entries.rows[0] as any).updatedAt, 1700000000);
  assert.deepEqual((canonical.response.tables.entries.rows[0] as any).details, {
    _type: "Bundle", when: 1700000000, role: { _type: "Member" }, children: [{ _type: "Empty" }],
    byName: { first: { _type: "Empty" } }, note: null,
  });
  for (const corruption of [
    "update entries set role = 'Unknown'",
    "update entries set enabled = 8",
    "update entries set details = jsonb('{\"_type\":\"Unknown\"}')",
    "update entries set details = jsonb('{\"_type\":\"Note\",\"count\":2}')",
    "update entries set details = jsonb('{\"_type\":\"Note\",\"count\":2,\"enabled\":1}')",
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
    id Id.Uuid @id
    workspaceId Id.Uuid
    userId Int
    @allow(query) { False }
    @allow(insert, update, delete) { True }
}
record Workspace {
    id Id.Uuid @id
    memberships @link(Membership.workspaceId)
    @allow(query) { exists memberships { userId == Session.userId } }
    @allow(insert, update, delete) { True }
}`;
    await ensureDatabase(linked, "_default", source);
    await loadSchemaFromDatabase("linked", linked);
    const linkedManifest: BatchManifest = { version: 1, manifestVersion: "linked-manifest", replacementContracts: { _default: wasm.get_schema_compiled_contract() },
      SessionValidator: z.object({ userId: z.number().int() }), queries: {} };
    const visibleWorkspaceId = "01890f6c-7b80-7000-8000-000000000010";
    const hiddenWorkspaceId = "01890f6c-7b80-7000-8000-000000000011";
    await linked.batch([
      { sql: "insert into workspaces(id,updatedAt) values(?,0),(?,0)", args: [visibleWorkspaceId, hiddenWorkspaceId] },
      { sql: "insert into memberships(id,workspaceId,userId,updatedAt) values(?,?,7,0),(?,?,8,0)", args: [
        visibleWorkspaceId, visibleWorkspaceId, hiddenWorkspaceId, hiddenWorkspaceId,
      ] },
    ]);
    const linkedAuthority = { ...authority, databaseId: "linked", manifest: linkedManifest.manifestVersion };
    const linkedEpoch = (await linked.execute("select database_epoch from _pyre_sync")).rows[0].database_epoch as string;
    const linkedRequest = { ...request, ...linkedAuthority, databaseEpoch: linkedEpoch, target: 0 };
    const before = await catchupReplacement(linked, linkedManifest, linkedAuthority, linkedRequest, { userId: 7 });
    assert.equal(before.kind, "success", JSON.stringify(before));
    if (before.kind !== "success") throw Error("No linked replacement");
    assert.deepEqual(before.response.tables.workspaces.rows, [{ id: visibleWorkspaceId, updatedAt: 0 }]);
    assert.deepEqual(before.response.tables.memberships.rows, []);
    await assert.rejects(catchup(linked, { tables: {} }, { userId: 7 }, 1000, "linked"), /ReplacementRequired|replacement/i);
    const command = { id: "revoke", operation: "delete", primary_db: "_default", attached_dbs: [],
      schemaContracts: linkedManifest.replacementContracts, session_args: ["userId"],
      optional_input_args: [], json_input_args: [], InputValidator: z.object({}), SessionValidator: linkedManifest.SessionValidator,
      sql: [{ include: false, params: ["session_userId"], sql: "delete from memberships where userId = $session_userId" }] };
    const fence = { ...linkedAuthority, databaseEpoch: linkedEpoch, instance: "reader", authGeneration: 9 };
    const sent: any[] = [];
    const result = await runWithSync(linked, { revoke: command }, "revoke", {}, { userId: 7 }, new Map([
      ["legacy", { session: { userId: 7 } }], ["fenced", { session: { userId: 7 }, fence }],
    ]), "linked", undefined, (id, message) => sent.push({ id, message }), linkedManifest.manifestVersion);
    await result.sync(() => {});
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

  for (const timing of ["beforeTransaction", "beforeSnapshot", "afterSnapshot"]) {
    const id = `migration-${timing}`;
    const url = `file:${join(directory, `${id}.db`)}`;
    const reader = createClient({ url });
    const writer = createClient({ url });
    try {
      const source = (allow: string) => `record Note {
    id Id.Uuid @id
    body String
    @allow(query) { ${allow} }
    @allow(insert, update, delete) { True }
}`;
      await reader.execute("pragma journal_mode = WAL");
      await ensureDatabase(reader, "_default", source("True"));
      await loadSchemaFromDatabase(id, reader);
      const originalManifest: BatchManifest = { version: 1, manifestVersion: id,
        replacementContracts: { _default: wasm.get_schema_compiled_contract() }, SessionValidator: z.object({}), queries: {} };
      const binding = { ...authority, databaseId: id, manifest: id };
      const epoch = (await reader.execute("select database_epoch from _pyre_sync")).rows[0].database_epoch as string;
      const originalRequest = { ...request, ...binding, databaseEpoch: epoch, target: 0 };
      await reader.execute("insert into notes(id,body) values('01890f6c-7b80-7000-8000-000000000030','original')");
      let migrated = false;
      let currentManifest = originalManifest;
      const migrate = async () => {
        assert.equal(migrated, false);
        migrated = true;
        await ensureDatabase(writer, "_default", source("False"));
        await writer.execute("insert into notes(id,body) values('01890f6c-7b80-7000-8000-000000000031','post-revocation secret')");
        await loadSchemaFromDatabase(id, writer);
        currentManifest = { ...originalManifest, manifestVersion: `${id}:new`,
          replacementContracts: { _default: wasm.get_schema_compiled_contract() } };
      };
      const transaction = reader.transaction.bind(reader);
      reader.transaction = async mode => {
        if (!migrated && timing === "beforeTransaction") await migrate();
        const tx = await transaction(mode);
        const execute = tx.execute.bind(tx);
        tx.execute = async statement => {
          const snapshotRead = typeof statement === "string" && statement.startsWith("select database_epoch");
          if (!migrated && snapshotRead && timing === "beforeSnapshot") await migrate();
          const result = await execute(statement);
          if (!migrated && snapshotRead && timing === "afterSnapshot") await migrate();
          return result;
        };
        return tx;
      };
      const result = await catchupReplacement(reader, originalManifest, binding, originalRequest, {});
      assert.equal(migrated, true, timing);
      if (timing === "afterSnapshot") {
        assert.equal(result.kind, "success", JSON.stringify(result));
        if (result.kind !== "success") throw Error("No pinned replacement");
        assert.deepEqual(result.response.tables.notes.rows.map((row: any) => row.body), ["original"]);
      } else {
        assert.deepEqual(result, { kind: "error", error: { errorType: "InvalidRequest", message: "InvalidRequest" } }, timing);
      }
      assert.equal((await reader.execute("select database_epoch from _pyre_sync")).rows[0].database_epoch, epoch,
        "permission-only migrations need not rotate the epoch");
      const currentBinding = { ...binding, manifest: currentManifest.manifestVersion };
      const current = await catchupReplacement(reader, currentManifest, currentBinding,
        { ...originalRequest, ...currentBinding }, {});
      assert.equal(current.kind, "success", JSON.stringify(current));
      if (current.kind !== "success") throw Error("No post-migration replacement");
      assert.deepEqual(current.response.tables.notes.rows, []);
      assert.equal((await writer.execute("select count(*) as count from notes")).rows[0].count, 2);
    } finally { reader.close(); writer.close(); }
  }

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
    id Id.Uuid @id
    workspaceId Id.Uuid
    value ${jsonType}?
    @allow(query) { value ${operator} ${rhs} }
    @allow(insert, update, delete) { False }
}
record Workspace {
    id Id.Uuid @id
    memberships @link(Membership.workspaceId)
    @allow(query) { exists memberships { value ${operator} ${rhs} } }
    @allow(insert, update, delete) { False }
}`;
          await ensureDatabase(permissions, "_default", source);
          await loadSchemaFromDatabase(id, permissions);
          const manifest: BatchManifest = { version: 1, manifestVersion: id, replacementContracts: { _default: wasm.get_schema_compiled_contract() },
            SessionValidator: z.object({ value: z.number().int().nullable() }), queries: {} };
          const ids = [
            "01890f6c-7b80-7000-8000-000000000020",
            "01890f6c-7b80-7000-8000-000000000021",
            "01890f6c-7b80-7000-8000-000000000022",
          ];
          await permissions.batch([
            { sql: "insert into workspaces(id) values(?),(?),(?)", args: ids },
            { sql: "insert into memberships(id,workspaceId,value) values(?,?,jsonb('7')),(?,?,jsonb('8')),(?,?,NULL)", args: [
              ids[0], ids[0], ids[1], ids[1], ids[2], ids[2],
            ] },
          ]);
          const binding = { ...authority, databaseId: id, manifest: id };
          const epoch = (await permissions.execute("select database_epoch from _pyre_sync")).rows[0].database_epoch as string;
          for (const value of [7, null]) {
            const result = await catchupReplacement(permissions, manifest, binding,
              { ...request, ...binding, databaseEpoch: epoch, target: 0 }, { value });
            assert.equal(result.kind, "success", JSON.stringify(result));
            if (result.kind !== "success") throw Error("No JSON permission replacement");
            const matchIndex = rhs === "null" || (rhs === "Session.value" && value === null) ? 2 : 0;
            const expected = operator === "==" ? [ids[matchIndex]] : ids.filter((_, index) => index !== matchIndex);
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
    id Id.Uuid @id
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
    const valuesById = new Map<string, string>();
    for (const [index, value] of values.entries()) {
      const id = `01890f6c-7b80-7000-8000-${String(index).padStart(12, "0")}`;
      valuesById.set(id, value);
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
      const value = valuesById.get(row.id);
      assert.equal(row.label, value);
      assert.equal(row.raw, value);
      assert.equal(row.optional, null);
      assert.deepEqual(row.payload, { _type: "Text", value });
    }
  } finally { strings.close(); }
} finally { db.close(); rmSync(directory, { recursive: true, force: true }); }
