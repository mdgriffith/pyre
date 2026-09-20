// A separate process keeps bun:test module mocks out of this real libsql/WASM check.
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createClient, type TransactionMode } from "@libsql/client";
import { namespace, scopedEdit } from "@pyre/core/local-edits";
import initWasm, { get_schema_compiled_contract } from "../wasm/pyre_wasm.js";
import { bindSchemaManifest, captureReplacementSchema, ensureDatabase, loadSchemaFromDatabase } from "../schema";
import { run, runBatch, type BatchManifest } from "../query";
import { catchupReplacement, runWithSync } from "../query-sync";
import { toRunner } from "../runtime/runner";
import { catchup } from "../sync";
import { localEdits } from "../local-edits";
import { databases } from "./compiled-batch/generated/databases";
import { compiledContract, manifestVersion, replacementContracts } from "./compiled-batch/generated/manifest";
import { SessionValidator } from "./compiled-batch/generated/decode";
import { meta } from "./compiled-batch/generated/queries/metadata/entryCreate";
import { sql, syncSql } from "./compiled-batch/generated/queries/sql/entryCreate";
import { meta as readMeta } from "./compiled-batch/generated/queries/metadata/entriesForContext";
import { sql as readSql } from "./compiled-batch/generated/queries/sql/entriesForContext";

await initWasm({ module_or_path: readFileSync(new URL("../wasm/pyre_wasm_bg.wasm", import.meta.url)) });
const source = databases._default.schemaSource;
const revoked = source.replace("@public", "@allow(query, insert, update, delete) { False }");
assert.notEqual(revoked, source);
const queries = { [meta.id]: { ...meta, sql, syncSql }, [readMeta.id]: { ...readMeta, sql: readSql } };
const manifest: BatchManifest = { version: 1, manifestVersion, compiledContract, replacementContracts, SessionValidator, queries };
const details = { _type: "Note", count: 2, enabled: false };
const session = { userId: 7, role: { _type: "Member" }, context: details, unrelated: "required" };
const directory = mkdtempSync(join(tmpdir(), "pyre-schema-authority-"));
try {
  for (const scenario of ["revoked", "missing", "null", "invalid", "comments", "failed", "unfinished", "migration-first", "write-first"]) {
    const url = `file:${join(directory, `${scenario}.db`)}`;
    const a = createClient({ url });
    const b = createClient({ url });
    try {
      await ensureDatabase(a, "_default", source);
      await loadSchemaFromDatabase(scenario, a);
      const authority = { databaseId: scenario, namespace: "_default", manifest: manifestVersion, instance: "tab", authGeneration: 1 };
      const epoch = (await a.execute("select database_epoch from _pyre_sync")).rows[0].database_epoch as string;
      let sequence = 0;
      const input = () => ({ id: `01890f6c-7b80-7000-8000-${String(++sequence).padStart(12, "0")}`,
        release: "00000000-0000-4000-8000-000000000002", enabled: true, count: 1, role: { _type: "Member" }, details });
      const sent: unknown[] = [];
      const publish = (...message: unknown[]) => { sent.push(message); };
      const recipients = new Map([["reader", { session, fence: { ...authority, databaseEpoch: epoch } }]]);
      const scope = namespace("_default", manifestVersion);
      // Keep the same public binding across B's migration, just like a long-lived host.
      const bound = localEdits.bind({ database: a, databaseId: scenario, namespace: scope,
        manifest, session, connectedSessions: recipients, sendToSession: publish });
      const batch = (empty = false) => runBatch(a, manifest, authority, { version: 1, ...authority, databaseEpoch: epoch,
        requestId: `write-${sequence}`, sequence: sequence + 1,
        operations: empty ? [] : [{ operation: meta.id, input: input() }] }, session, publish);
      const runner = toRunner(meta, sql);
      const readRunner = toRunner(readMeta, readSql);
      const operations = {
        batch: () => batch(),
        bound: () => {
          const { id: _id, ...fields } = input();
          return bound.submit(scopedEdit(scope, {
            id: meta.id, parseInput: value => meta.InputValidator.parse(value), decodeResult: value => value,
          }, fields));
        },
        emptyBatch: () => batch(true),
        write: () => run(a, queries, meta.id, input(), session),
        read: () => run(a, queries, readMeta.id, {}, session),
        sync: () => runWithSync(a, queries, meta.id, input(), session, recipients, scenario, undefined, publish, manifestVersion),
        runnerWrite: () => runner(a, session, input()),
        runnerRead: () => readRunner(a, session, {}),
        replacement: () => catchupReplacement(a, manifest, authority, { version: 1, ...authority, databaseEpoch: epoch,
          requestId: "replacement", target: 0 }, session),
        legacy: () => catchup(a, { tables: {} }, session, 1000, scenario),
      };
      const state = async () => ({
        rows: (await a.execute("select * from entries order by id")).rows,
        sync: (await a.execute("select * from _pyre_sync")).rows,
      });
      const check = async (allowed: boolean) => {
        for (const [name, operation] of Object.entries(operations)) {
          const label = `${scenario}: ${name}`;
          const before = await state();
          const publications = sent.length;
          if (allowed) {
            const result = await operation();
            if (result && typeof result === "object" && "kind" in result) assert.equal(result.kind, name === "bound" ? "confirmed" : "success", `${label}: ${JSON.stringify(result)}`);
            if (name === "read" || name === "runnerRead") {
              const data = name === "read" ? (result as any).response : result;
              assert.ok(data.entry.length > 0, label);
            }
            if (name === "replacement") assert.ok((result as any).response.tables.entries.rows.length > 0, label);
            if (name === "legacy") assert.ok((result as any).tables.entries.rows.length > 0, label);
            const after = await state();
            if (["batch", "bound", "write", "sync", "runnerWrite"].includes(name)) {
              assert.equal(after.rows.length, before.rows.length + 1, label);
              assert.equal(after.sync[0].server_revision, Number(before.sync[0].server_revision) +
                (name === "batch" || name === "bound" || name === "sync" ? 1 : 0), label);
            } else {
              assert.deepEqual(after, before, label);
              assert.equal(sent.length, publications, label);
            }
          } else {
            if (name === "bound") {
              assert.deepEqual(await operation(), { kind: "rejected", code: "InvalidRequest" }, label);
            } else if (["batch", "emptyBatch", "replacement"].includes(name)) {
              assert.deepEqual(await operation(), { kind: "error", error: { errorType: "InvalidRequest", message: "InvalidRequest" } }, label);
            } else {
              await assert.rejects(operation, label);
            }
            assert.deepEqual(await state(), before, `${label}: no row or revision changes`);
            assert.equal(sent.length, publications, `${label}: no publication`);
          }
        }
      };

      // Prove the exact generated inputs/SQL and both catchup paths work before damaging authority.
      await check(true);
      const beforeMigration = await state();
      const migrate = () => ensureDatabase(b, "_default", revoked);
      if (scenario === "migration-first" || scenario === "write-first") {
        const transaction = a.transaction.bind(a);
        const events: string[] = [];
        a.transaction = async (mode?: TransactionMode) => {
          assert.equal(mode, "write");
          a.transaction = transaction;
          if (scenario === "migration-first") { await migrate(); events.push("migration"); }
          const tx = await transaction(mode);
          events.push("acquire");
          if (scenario === "write-first") {
            const commit = tx.commit.bind(tx);
            tx.commit = async () => {
              await commit();
              events.push("commit");
              // Never await a competing writer while holding SQLite's write lock.
              await migrate();
              events.push("migration");
            };
          }
          return tx;
        };
        const publications = sent.length;
        const result = await batch();
        if (scenario === "migration-first") {
          assert.equal(result.kind, "error");
          assert.deepEqual(events, ["migration", "acquire"]);
          assert.deepEqual(await state(), beforeMigration);
          assert.equal(sent.length, publications);
        } else {
          assert.equal(result.kind, "success", JSON.stringify(result));
          assert.deepEqual(events, ["acquire", "commit", "migration"]);
          const after = await state();
          assert.equal(after.rows.length, beforeMigration.rows.length + 1);
          assert.equal(after.sync[0].server_revision, Number(beforeMigration.sync[0].server_revision) + 1);
          assert.equal(sent.length, publications + 1);
        }
      } else if (scenario === "revoked") {
        await migrate();
      } else if (scenario === "comments") {
        await ensureDatabase(b, "_default", `${source}\n// Permission-neutral documentation change\n`);
      } else if (scenario === "missing") {
        await b.execute("delete from _pyre_migrations");
      } else {
        await b.execute({ sql: "insert into _pyre_migrations(name, sql, schema, finished_at, error) values(?, '', ?, ?, ?)",
          args: [scenario, scenario === "null" ? null : scenario === "invalid" ? "not a Pyre schema {" : revoked,
            scenario === "unfinished" ? null : 1, scenario === "failed" ? "migration failed" : null] });
      }
      // B never acquires A's database ID. Neither A's evidence nor its cached source is refreshed.
      bindSchemaManifest(a, "_default", manifest);
      assert.equal(captureReplacementSchema(scenario, replacementContracts._default).schemaSource, source);
      assert.equal((await state()).sync[0].database_epoch, epoch);
      if (scenario !== "write-first") assert.deepEqual(await state(), beforeMigration);
      await check(["comments", "failed", "unfinished"].includes(scenario));
    } finally { a.close(); b.close(); }
  }

  // New mutation SQL may be valid while A's database-id cache still grants old read permissions.
  const databaseId = "new-execution-stale-publisher";
  const url = `file:${join(directory, `${databaseId}.db`)}`;
  const a = createClient({ url });
  const b = createClient({ url });
  try {
    await ensureDatabase(a, "_default", source);
    await loadSchemaFromDatabase(databaseId, a);
    const epoch = (await a.execute("select database_epoch from _pyre_sync")).rows[0].database_epoch;
    const recipients = new Map(["origin", "legacy-1", "legacy-2"].map(id => [id, { session }]));
    const sent: { id: string; message: any }[] = [];
    const send = (id: string, message: any) => { sent.push({ id, message }); };
    const input = { id: "01890f6c-7b80-7000-8000-000000000100", release: "private-release", enabled: true,
      count: 1, role: { _type: "Member" }, details };
    const baseline = await runWithSync(a, queries, meta.id, input, session, recipients, databaseId, "origin", send);
    assert.equal(baseline.kind, "success");
    assert.deepEqual(sent.map(item => item.id).sort(), ["legacy-1", "legacy-2"]);
    for (const message of [...sent.map(item => item.message), (baseline.response as any).sync]) {
      assert.equal(message.type, "delta");
      assert.ok(JSON.stringify(message.data).includes(input.id), "permissive cache really emits affected rows");
    }

    const restricted = source.replace("@public", "@allow(query) { False }\n    @allow(insert, update, delete) { True }");
    await ensureDatabase(b, "_default", restricted);
    const newContract = get_schema_compiled_contract();
    assert.notEqual(newContract, replacementContracts._default);
    assert.equal(captureReplacementSchema(databaseId, replacementContracts._default).schemaSource, source);
    // Insert permission is unchanged, so the generated insert/RETURNING SQL is still valid.
    // Its trusted schema contract must describe B's new permission source, not A's cached source.
    const newQuery = { ...queries[meta.id], schemaContracts: { _default: newContract } };
    const newInput = { ...input, id: "01890f6c-7b80-7000-8000-000000000101" };
    sent.length = 0;
    const errors: unknown[][] = [];
    const logError = console.error;
    let result;
    try {
      console.error = (...args: unknown[]) => { errors.push(args); };
      result = await runWithSync(a, { [meta.id]: newQuery }, meta.id, newInput, session,
        recipients, databaseId, "origin", send);
    } finally { console.error = logError; }
    assert.equal(result.kind, "success");
    assert.equal((result.response as any).result.entry[0].id, newInput.id);
    assert.equal((await a.execute("select count(*) as n from entries")).rows[0].n, 2);
    assert.deepEqual((await a.execute("select database_epoch, server_revision from _pyre_sync")).rows,
      [{ database_epoch: epoch, server_revision: 2 }]);
    const hint = { type: "syncRequired", databaseId, databaseEpoch: epoch, serverRevision: 2 };
    assert.deepEqual(sent, ["legacy-1", "legacy-2"].map(id => ({ id, message: hint })));
    assert.deepEqual((result.response as any).sync, hint);
    assert.deepEqual((await result.sync(send)).originMessage, hint);
    assert.equal(sent.length, 2, "publication is not repeated by sync()");
    assert.equal(errors.length, 1);
    assert.match(String(errors[0][1]), /Replacement contract mismatch/);
    assert.equal(captureReplacementSchema(databaseId, replacementContracts._default).schemaSource, source);
  } finally { a.close(); b.close(); }
} finally { rmSync(directory, { recursive: true, force: true }); }
