import { afterEach, expect, mock, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { SchemaMetadata } from "@pyre/core";
import { createClient } from "@libsql/client";
import { z } from "zod";
import { run, runBatch, seed, type BatchManifest, type BatchRequest, type QueryMetadata } from "./query";
import { toRunner } from "./runtime/runner";
import { buildArgs, toSqlStatements } from "./runtime/sql";
import { meta as compiledCreate } from "./fixtures/compiled-batch/generated/queries/metadata/entryCreate";
import { sql as compiledCreateSql } from "./fixtures/compiled-batch/generated/queries/sql/entryCreate";
import { CoercedDate, Role, SessionValidator as compiledSession } from "./fixtures/compiled-batch/generated/decode";
import { manifestVersion as compiledFingerprint } from "./fixtures/compiled-batch/generated/manifest";
import { meta as compiledContextQuery } from "./fixtures/compiled-batch/generated/queries/metadata/entriesForContext";
import { sql as compiledContextSql } from "./fixtures/compiled-batch/generated/queries/sql/entriesForContext";

const batchAuthority = { databaseId: "tenant-1", namespace: "Main", manifest: "m1", instance: "tab-1", authGeneration: 2 };
const batchRequest = (operations: BatchRequest["operations"]): BatchRequest => ({
  version: 1, ...batchAuthority, databaseEpoch: "e1", requestId: "request-1", sequence: 1, operations,
});

function editManifest(): BatchManifest {
  const common = {
    primary_db: "Main", session_args: ["userId"], optional_input_args: ["body"], json_input_args: [],
    SessionValidator: z.object({ userId: z.number() }),
  };
  const update: QueryMetadata = {
    ...common, id: "update", operation: "update", InputValidator: z.object({ id: z.number(), body: z.string().optional() }),
    ReturnData: z.object({ notes: z.array(z.object({ id: z.number(), body: z.string() })) }),
    generatedEdit: { kind: "update", writeStatementIndices: [0], writableInputs: ["body"] },
    sql: [
      { include: true, params: ["id", "body", "session_userId"], sql: "update notes set body = $body where id = $id and owner = $session_userId returning id as _pyreEditId" },
      { include: true, params: [], sql: "select '[]' as notes" },
    ],
  };
  return { version: 1, manifestVersion: "m1", SessionValidator: common.SessionValidator, queries: {
    update,
    create: { ...common, id: "create", operation: "insert", InputValidator: z.object({ body: z.string() }),
      generatedEdit: { kind: "create", writeStatementIndices: [0], writableInputs: ["body"] },
      sql: [{ include: true, params: ["body", "session_userId"], sql: "insert into notes(body, owner) values ($body, $session_userId) returning id as _pyreEditId" }],
    },
    delete: { ...common, id: "delete", operation: "delete", InputValidator: z.object({ id: z.number() }),
      generatedEdit: { kind: "delete", writeStatementIndices: [0], writableInputs: [] },
      sql: [{ include: true, params: ["id", "session_userId"], sql: "delete from notes where id = $id and owner = $session_userId returning id as _pyreEditId" }],
    },
    named: { ...common, id: "named", operation: "transaction", InputValidator: z.object({}), ReturnData: z.object({ notes: z.array(z.object({ id: z.number(), body: z.string() })) }),
      sql: [{ include: true, params: [], sql: "select json_group_array(json_object('id',id,'body',body)) as notes from notes" }],
    },
  } };
}

const batchDirectories: string[] = [];
afterEach(() => { for (const path of batchDirectories.splice(0)) rmSync(path, { recursive: true, force: true }); });

async function batchDatabase() {
  // libsql transaction() detaches its connection; :memory: would be lost on the next client call.
  const directory = mkdtempSync(join(tmpdir(), "pyre-batch-"));
  batchDirectories.push(directory);
  const db = createClient({ url: `file:${join(directory, "test.db")}` });
  await db.execute("create table notes(id integer primary key, body text unique not null, owner integer not null)");
  await db.execute("create table audit(note integer)");
  await db.execute("create trigger log_update after update on notes begin insert into audit values(new.id); insert into audit values(new.id); end");
  await db.execute("insert into notes values (1,'first',7), (2,'second',8)");
  await db.execute("create table _pyre_sync(id integer primary key, database_epoch text not null, server_revision integer not null)");
  await db.execute("insert into _pyre_sync values (1,'e1',0)");
  return db;
}

test("memory batches reject before detachment, including otherwise-valid writes and stale epochs", async () => {
  for (const url of ["file::memory:", ":memory:", "file::memory:?cache=private", "file::memory:?cache=shared"]) {
    const db = createClient({ url });
    try {
      await db.execute("create table notes(id integer primary key, body text, owner integer)");
      await db.execute("insert into notes values(1,'original',7)");
      await db.execute("create table _pyre_sync(id integer primary key, database_epoch text, server_revision integer)");
      await db.execute("insert into _pyre_sync values(1,'e1',0)");
      const transaction = mock(db.transaction.bind(db));
      db.transaction = transaction;
      const publish = mock(() => {});
      const request = batchRequest([{ operation: "update", input: { id: 1, body: "new" } }]);
      for (const databaseEpoch of ["e1", "stale"]) {
        expect(await runBatch(db, editManifest(), batchAuthority, { ...request, databaseEpoch }, { userId: 7 }, publish))
          .toEqual({ kind: "error", error: { errorType: "TransactionFailed", message: "TransactionFailed" } });
        expect((await db.execute("select * from notes")).rows).toEqual([{ id: 1, body: "original", owner: 7 }]);
        expect((await db.execute("select * from _pyre_sync")).rows).toEqual([{ id: 1, database_epoch: "e1", server_revision: 0 }]);
      }
      expect(await runBatch(db, editManifest(), batchAuthority, { ...request, namespace: "Other" }, { userId: 7 }, publish))
        .toMatchObject({ kind: "error", error: { errorType: "InvalidRequest" } });
      const execute = mock(db.execute.bind(db));
      db.execute = execute;
      expect(await runBatch(db, editManifest(), batchAuthority, batchRequest([]), { userId: 7 }, publish))
        .toEqual({ kind: "success", response: { ...batchAuthority, databaseEpoch: "e1", requestId: "request-1", status: "confirmed", results: [] } });
      expect(execute).not.toHaveBeenCalled();
      expect(transaction).not.toHaveBeenCalled();
      expect(publish).not.toHaveBeenCalled();
      // The same public client remains usable for both reads and writes after rejection/success.
      await db.execute("update notes set body = 'still connected' where id = 1");
      expect((await db.execute("select body from notes")).rows[0].body).toBe("still connected");
    } finally { db.close(); }
  }
});

test("local batch storage guard fails closed when the main filename cannot be established", async () => {
  for (const rows of [[], [{ name: "temp", file: "/tmp/temporary" }], [{ name: "main", file: null }]]) {
    const db = { protocol: "file", execute: mock(async () => ({ rows })), transaction: mock(() => { throw Error("must not detach"); }) };
    expect(await runBatch(db as any, editManifest(), batchAuthority, batchRequest([{ operation: "create", input: { body: "new" } }]), { userId: 7 }))
      .toEqual({ kind: "error", error: { errorType: "TransactionFailed", message: "TransactionFailed" } });
    expect(db.execute).toHaveBeenCalledWith("pragma database_list");
    expect(db.transaction).not.toHaveBeenCalled();
  }
});

test("file-backed databases with memory journals still commit batches", async () => {
  const db = await batchDatabase();
  try {
    await db.execute("pragma journal_mode = memory");
    expect(await runBatch(db, editManifest(), batchAuthority, batchRequest([{ operation: "update", input: { id: 1, body: "committed" } }]), { userId: 7 }))
      .toMatchObject({ kind: "success", response: { status: "accepted", commitRevision: 1 } });
    expect((await db.execute("select body from notes where id = 1")).rows[0].body).toBe("committed");
  } finally { db.close(); }
});

test("libsql local RETURNING count defect requires immediate direct changes(), excluding triggers", async () => {
  const db = await batchDatabase();
  try {
    const tx = await db.transaction("write");
    const updated = await tx.execute("update notes set body = body where id = 1 returning id");
    expect(updated.rowsAffected).toBe(0);
    expect((await tx.execute("select changes() as n")).rows[0].n).toBe(1);
    expect(updated.rows).toEqual([{ id: 1 }]);
    expect((await tx.execute("select count(*) as n from audit")).rows[0].n).toBe(2);
    expect((await tx.execute("update notes set body = body where id = 999 returning id")).rowsAffected).toBe(0);
    await tx.execute("update notes set body = body returning id");
    expect((await tx.execute("select changes() as n")).rows[0].n).toBe(2);
    await tx.execute("insert into notes values(3,'third',7) returning id");
    expect((await tx.execute("select changes() as n")).rows[0].n).toBe(1);
    await tx.execute("delete from notes where id = 3 returning id");
    expect((await tx.execute("select changes() as n")).rows[0].n).toBe(1);
    expect((await tx.execute("update notes set body = body where id = 1")).rowsAffected).toBe(1);
    await tx.rollback();
  } finally { db.close(); }
});

test("compiled batch preserves repeated IDs and named results; hidden reads do not deny direct writes", async () => {
  const db = await batchDatabase();
  try {
    const result = await runBatch(db, editManifest(), batchAuthority, batchRequest([
      { operation: "update", input: { id: 1, body: "first" } },
      { operation: "update", input: { id: 1, body: "final" } },
      { operation: "named", input: {} },
      { operation: "create", input: { body: "third" } },
      { operation: "delete", input: { id: 1 } },
    ]), { userId: 7 });
    expect(result.kind).toBe("success");
    if (result.kind !== "success") return;
    expect(result.response.results).toEqual([
      { index: 0, operation: "update", value: { id: 1 } },
      { index: 1, operation: "update", value: { id: 1 } },
      { index: 2, operation: "named", value: { notes: [{ id: 1, body: "final" }, { id: 2, body: "second" }] } },
      { index: 3, operation: "create", value: { id: 3 } },
      { index: 4, operation: "delete", value: { id: 1 } },
    ]);
    expect(result.response.commitRevision).toBe(1);
    expect(result.response.reconciliation).toEqual({ kind: "replaceRequired", atLeast: 1, invalidate: true, minimumSafeRevision: 1 });
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(1);
  } finally { db.close(); }
});

test("generated missing, forbidden, many-target and SQL failures roll back the entire batch and revision", async () => {
  for (const failure of ["missing", "forbidden", "many", "constraint", "codec"]) {
    const db = await batchDatabase();
    const manifest = editManifest();
    if (failure === "many") manifest.queries.update.sql[0].sql = "update notes set body = body returning id";
    if (failure === "codec") manifest.queries.named.ReturnData = z.never();
    const publish = mock(() => {});
    try {
      const result = await runBatch(db, manifest, batchAuthority, batchRequest([
        { operation: "create", input: { body: "prefix" } },
        failure === "codec" ? { operation: "named", input: {} } : {
          operation: "update", input: { id: failure === "missing" ? 99 : failure === "forbidden" ? 2 : 1, body: failure === "constraint" ? "second" : "new" },
        },
      ]), { userId: 7 }, publish);
      expect(result).toEqual({ kind: "error", error: {
        errorType: failure === "constraint" ? "TransactionFailed" : failure === "codec" ? "InvalidRequest" : "TargetNotWritable",
        message: failure === "constraint" ? "TransactionFailed" : failure === "codec" ? "InvalidRequest" : "TargetNotWritable", index: 1,
      } });
      expect((await db.execute("select body from notes order by id")).rows).toEqual([{ body: "first" }, { body: "second" }]);
      expect((await db.execute("select * from audit")).rows).toEqual([]);
      expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(0);
      expect(publish).not.toHaveBeenCalled();
    } finally { db.close(); }
  }
});

test("batch validates every member, authority, session, strip-mode protected fields and limits before I/O", async () => {
  const db = { transaction: mock(() => { throw Error("must not execute"); }) };
  const manifest = editManifest();
  const valid = batchRequest([{ operation: "update", input: { id: 1, body: "new" } }]);
  const cases: [BatchRequest, Record<string, unknown>][] = [
    [batchRequest([{ operation: "update", input: { id: 1, body: "new", owner: 9 } }]), { userId: 7 }],
    [batchRequest([{ operation: "update", input: { id: 1 } }]), { userId: 7 }],
    [batchRequest([{ operation: "toString", input: {} }]), { userId: 7 }],
    [valid, {}],
    [{ ...valid, databaseId: "other" }, { userId: 7 }],
    [{ ...valid, namespace: "Other" }, { userId: 7 }],
    [{ ...valid, manifest: "old" }, { userId: 7 }],
    [{ ...valid, instance: "old" }, { userId: 7 }],
    [batchRequest(Array(101).fill(valid.operations[0])), { userId: 7 }],
    [batchRequest([{ operation: "create", input: { body: "x".repeat(1024 * 1024) } }]), { userId: 7 }],
  ];
  for (const [request, session] of cases) expect((await runBatch(db as any, manifest, batchAuthority, request, session)).kind).toBe("error");
  manifest.queries.update.primary_db = "Other";
  expect((await runBatch(db as any, manifest, batchAuthority, valid, { userId: 7 })).kind).toBe("error");
  manifest.queries.update.primary_db = "Main";
  manifest.queries.update.operation = "query";
  expect((await runBatch(db as any, manifest, batchAuthority, valid, { userId: 7 })).kind).toBe("error");
  manifest.queries.update.operation = "update";
  manifest.queries.update.attached_dbs = ["Other"];
  expect((await runBatch(db as any, manifest, batchAuthority, valid, { userId: 7 })).kind).toBe("error");
  expect(db.transaction).not.toHaveBeenCalled();
});

test("batch capture is synchronous and queued in invocation order; publication failure retains commit", async () => {
  const db = await batchDatabase();
  try {
    const manifest = editManifest();
    const operations = [{ operation: "update", input: { id: 1, body: "captured" } }];
    const session = { userId: 7 };
    const first = runBatch(db, manifest, batchAuthority, batchRequest(operations), session, async committed => {
      expect(committed.response.commitRevision).toBe(1);
      throw Error("notification unavailable");
    });
    operations[0].input.body = "mutated";
    operations.push({ operation: "delete", input: { id: 1, body: "" } });
    session.userId = 8;
    const second = runBatch(db, manifest, batchAuthority, batchRequest([{ operation: "named", input: {} }]), { userId: 7 });
    const [a, b] = await Promise.all([first, second]);
    expect(a.kind).toBe("success");
    expect(b.kind).toBe("success");
    if (b.kind === "success") {
      expect(b.response.commitRevision).toBe(2);
      expect((b.response.results[0].value as any).notes[0].body).toBe("captured");
    }
  } finally { db.close(); }
});

test("empty batch has no transaction, revision or publication", async () => {
  const db = { transaction: mock(() => { throw Error("must not execute"); }) };
  const publish = mock(() => {});
  const result = await runBatch(db as any, editManifest(), batchAuthority, batchRequest([]), { userId: 7 }, publish);
  expect(result.kind).toBe("success");
  if (result.kind === "success") { expect(result.response.results).toEqual([]); expect(result.response.commitRevision).toBeUndefined(); }
  expect(db.transaction).not.toHaveBeenCalled();
  expect(publish).not.toHaveBeenCalled();
});

test("batch captures nested JSON and rejects nested stripped fields", async () => {
  const db = await batchDatabase();
  try {
    const manifest = editManifest();
    manifest.queries.create.InputValidator = z.object({ body: z.object({ title: z.string() }) });
    manifest.queries.create.json_input_args = ["body"];
    const body = { title: "captured" };
    const pending = runBatch(db, manifest, batchAuthority, batchRequest([{ operation: "create", input: { body } }]), { userId: 7 });
    body.title = "mutated";
    expect((await pending).kind).toBe("success");
    expect((await db.execute("select body from notes where id = 3")).rows[0].body).toBe('{"title":"captured"}');
    const invalid = await runBatch(db, manifest, batchAuthority, batchRequest([{ operation: "create", input: { body: { title: "bad", managed: true } } }]), { userId: 7 });
    expect(invalid.kind).toBe("error");
  } finally { db.close(); }
});

test("attached connections and compiler attachment metadata reject before writes", async () => {
  const db = await batchDatabase();
  try {
    await db.execute("attach database ':memory:' as other");
    const result = await runBatch(db, editManifest(), batchAuthority, batchRequest([{ operation: "create", input: { body: "blocked" } }]), { userId: 7 });
    expect(result).toMatchObject({ kind: "error", error: { errorType: "InvalidRequest" } });
    expect((await db.execute("select count(*) as n from notes")).rows[0].n).toBe(2);
    const manifest = editManifest();
    manifest.queries.create.attached_dbs = ["other"];
    expect(await runBatch(db, manifest, batchAuthority, batchRequest([{ operation: "create", input: { body: "blocked" } }]), { userId: 7 }))
      .toMatchObject({ kind: "error", error: { errorType: "InvalidRequest", index: 0 } });
  } finally { db.close(); }
});

test("revision allocation failure rolls writes back and stale epoch rejects", async () => {
  const db = await batchDatabase();
  try {
    await db.execute("create trigger no_revision before update on _pyre_sync begin select raise(abort, 'private revision failure'); end");
    const request = batchRequest([{ operation: "create", input: { body: "prefix" } }]);
    const result = await runBatch(db, editManifest(), batchAuthority, request, { userId: 7 });
    expect(result).toEqual({ kind: "error", error: { errorType: "TransactionFailed", message: "TransactionFailed" } });
    expect((await db.execute("select count(*) as n from notes")).rows[0].n).toBe(2);
    await db.execute("drop trigger no_revision");
    await db.execute("update _pyre_sync set database_epoch = 'e2'");
    expect(await runBatch(db, editManifest(), batchAuthority, request, { userId: 7 }))
      .toMatchObject({ kind: "error", error: { errorType: "InvalidRequest" } });
  } finally { db.close(); }
});

test("named zero-row writes keep declared semantics and allocate a revision without subscribers", async () => {
  const db = await batchDatabase();
  try {
    const manifest = editManifest();
    manifest.queries.named.sql = [{ include: false, params: [], sql: "delete from notes where id = 99" }];
    manifest.queries.named.ReturnData = z.object({});
    const result = await runBatch(db, manifest, batchAuthority, batchRequest([{ operation: "named", input: {} }]), { userId: 7 });
    expect(result).toMatchObject({ kind: "success", response: { status: "accepted", commitRevision: 1, results: [{ index: 0, operation: "named", value: {} }] } });
  } finally { db.close(); }
});

test("lost commit response is unknown, never a definitive rollback", async () => {
  const db = await batchDatabase();
  try {
    const transaction = db.transaction.bind(db);
    db.transaction = async mode => {
      const tx = await transaction(mode);
      const commit = tx.commit.bind(tx);
      tx.commit = async () => { await commit(); throw Error("connection lost after commit"); };
      return tx;
    };
    const publish = mock(() => {});
    const result = await runBatch(db, editManifest(), batchAuthority, batchRequest([{ operation: "create", input: { body: "committed" } }]), { userId: 7 }, publish);
    expect(result).toEqual({ kind: "unknown", error: { errorType: "OutcomeUnknown", message: "OutcomeUnknown" } });
    expect((await db.execute("select server_revision from _pyre_sync")).rows[0].server_revision).toBe(1);
    expect((await db.execute("select body from notes where id = 3")).rows[0].body).toBe("committed");
    expect(publish).not.toHaveBeenCalled();
  } finally { db.close(); }
});

test("batch accepts the exact operation and serialized UTF-8 payload limits", async () => {
  const db = await batchDatabase();
  try {
    const manifest = editManifest();
    const many = await runBatch(db, manifest, batchAuthority, batchRequest(Array.from({ length: 100 }, () => ({ operation: "named", input: {} }))), { userId: 7 });
    expect(many.kind).toBe("success");
    if (many.kind === "success") expect(many.response.results).toHaveLength(100);
    const request = batchRequest([{ operation: "create", input: { body: "" } }]);
    const overhead = new TextEncoder().encode(JSON.stringify(request)).byteLength;
    (request.operations[0].input as { body: string }).body = "x".repeat(1024 * 1024 - overhead);
    expect(new TextEncoder().encode(JSON.stringify(request)).byteLength).toBe(1024 * 1024);
    expect((await runBatch(db, manifest, batchAuthority, request, { userId: 7 })).kind).toBe("success");
    (request.operations[0].input as { body: string }).body += "x";
    expect(await runBatch(db, manifest, batchAuthority, request, { userId: 7 }))
      .toMatchObject({ kind: "error", error: { errorType: "InvalidRequest" } });
  } finally { db.close(); }
});

test("batch codecs preserve explicit null, structured inputs and session discriminators", async () => {
  const db = await batchDatabase();
  try {
    const manifest = editManifest();
    const query = manifest.queries.named;
    query.InputValidator = z.object({ value: z.string().nullable(), date: z.date(), payload: z.object({ _type: z.literal("Nested"), flag: z.boolean() }) });
    query.SessionValidator = z.object({ role: z.object({ _type: z.literal("Member") }) });
    manifest.SessionValidator = query.SessionValidator;
    query.session_args = ["role"];
    query.json_input_args = ["payload"];
    query.ReturnData = z.object({ items: z.array(z.object({ value: z.string().nullable(), seconds: z.number(), payload: z.object({ _type: z.literal("Nested"), flag: z.boolean() }), role: z.literal("Member") })) });
    query.sql = [{ include: true, params: ["value", "date", "payload", "session_role"], sql: "select json_object('value', $value, 'seconds', $date, 'payload', json($payload), 'role', $session_role) as items" }];
    const request = batchRequest([{ operation: "named", input: { value: null, date: new Date("2026-01-01T00:00:00Z"), payload: { _type: "Nested", flag: true } } }]);
    expect(await runBatch(db, manifest, batchAuthority, request, { role: { _type: "Member" } })).toMatchObject({
      kind: "success", response: { results: [{ index: 0, operation: "named", value: { items: [{ value: null, seconds: 1767225600, payload: { _type: "Nested", flag: true }, role: "Member" }] } }] },
    });
    expect((await runBatch(db, manifest, batchAuthority, request, { role: { _type: "Member", admin: true } })).kind).toBe("success");
  } finally { db.close(); }
});

test("batch wire request requires every fence and rejects unknown envelope/member fields and bad types", async () => {
  const db = { transaction: mock(() => { throw Error("must not execute"); }) };
  const valid = batchRequest([{ operation: "create", input: { body: "one" } }]);
  const malformed: unknown[] = [
    null, [], { ...valid, extra: true }, { ...valid, version: 2 }, { ...valid, version: "1" },
    { ...valid, sequence: 0 }, { ...valid, sequence: -1 }, { ...valid, sequence: 1.5 },
    { ...valid, sequence: Number.MAX_SAFE_INTEGER + 1 }, { ...valid, authGeneration: -1 },
    { ...valid, authGeneration: "2" }, { ...valid, authGeneration: 3 }, { ...valid, instance: "other" },
    { ...valid, requestId: "" }, { ...valid, databaseEpoch: 5 }, { ...valid, operations: {} },
    { ...valid, operations: [{ operation: "create", input: {}, sql: "drop table notes" }] },
    { ...valid, operations: [{ operation: "create" }] },
    { ...valid, operations: [{ operation: 1, input: {} }] },
  ];
  for (const field of Object.keys(valid)) {
    const missing = { ...valid } as Record<string, unknown>;
    delete missing[field];
    malformed.push(missing);
  }
  for (const request of malformed) {
    expect(await runBatch(db as any, editManifest(), batchAuthority, request as BatchRequest, { userId: 7 }))
      .toEqual({ kind: "error", error: { errorType: "InvalidRequest", message: "InvalidRequest" } });
  }
  expect(await runBatch(db as any, editManifest(), batchAuthority, batchRequest([{ operation: "private-unknown-id", input: {} }]), { userId: 7 }))
    .toEqual({ kind: "error", error: { errorType: "InvalidRequest", message: "InvalidRequest", index: 0 } });
  expect(await runBatch(db as any, { ...editManifest(), manifestVersion: "different-content" }, batchAuthority, valid, { userId: 7 }))
    .toEqual({ kind: "error", error: { errorType: "InvalidRequest", message: "InvalidRequest" } });
  expect(db.transaction).not.toHaveBeenCalled();
});

test("generated edits require exactly one nominated included statement", async () => {
  const db = { transaction: mock(() => { throw Error("must not execute"); }) };
  for (const indices of [[], [0, 1], [-1], [0.5], [99], [0, 0]]) {
    const manifest = editManifest();
    manifest.queries.create.generatedEdit!.writeStatementIndices = indices;
    expect(await runBatch(db as any, manifest, batchAuthority, batchRequest([{ operation: "create", input: { body: "one" } }]), { userId: 7 }))
      .toEqual({ kind: "error", error: { errorType: "InvalidRequest", message: "InvalidRequest", index: 0 } });
  }
  const manifest = editManifest();
  manifest.queries.create.sql[0].include = false;
  expect(await runBatch(db as any, manifest, batchAuthority, batchRequest([{ operation: "create", input: { body: "one" } }]), { userId: 7 }))
    .toMatchObject({ kind: "error", error: { errorType: "InvalidRequest", index: 0 } });
  expect(db.transaction).not.toHaveBeenCalled();
});

const compiledInput = {
  id: "uuid-is-a-string-codec", release: "release", enabled: true, count: 1,
  role: { _type: "Member" }, details: { _type: "Note", count: 2, enabled: true },
};

test("actual generated metadata executes release identifier and full unused session with enum objects", async () => {
  const db = await batchDatabase();
  try {
    await db.execute("create table entries(id text primary key, release text, enabled integer, count integer, role text, details blob, updatedAt integer)");
    const manifest: BatchManifest = { version: 1, manifestVersion: compiledFingerprint, SessionValidator: compiledSession, queries: {
      [compiledCreate.id]: { ...compiledCreate, sql: compiledCreateSql },
    } };
    const authority = { ...batchAuthority, manifest: compiledFingerprint, namespace: compiledCreate.primary_db };
    const request = { ...batchRequest([{ operation: compiledCreate.id, input: compiledInput }]), manifest: compiledFingerprint, namespace: authority.namespace };
    const session = { userId: 7, role: { _type: "Member" }, unrelated: "required even when unused" };
    expect(compiledCreate.session_args).toEqual([]);
    expect(await runBatch(db, manifest, authority, request, session)).toEqual({ kind: "success", response: {
      ...authority, databaseEpoch: "e1", requestId: "request-1", status: "accepted", commitRevision: 1,
      results: [{ index: 0, operation: compiledCreate.id, value: { id: compiledInput.id } }],
      reconciliation: { kind: "replaceRequired", atLeast: 1, invalidate: true, minimumSafeRevision: 1 },
    } });
    expect((await db.execute("select release, enabled, count, role, json(details) as details from entries")).rows).toEqual([
      { release: "release", enabled: 1, count: 1, role: "Member", details: '{"_type":"Note","count":2,"enabled":true}' },
    ]);
    expect(compiledContextQuery.json_session_args).toEqual(["context"]);
    const contextResult = await run(db, { [compiledContextQuery.id]: { ...compiledContextQuery, sql: compiledContextSql } }, compiledContextQuery.id, {}, {
      ...session, context: { _type: "Note", count: 2, enabled: true, hostile__count: 999 },
    });
    expect(contextResult.response).toEqual({ entry: [{ id: compiledInput.id }] });
    const empty = { ...request, operations: [] };
    expect(await runBatch(db, manifest, authority, empty, session)).toEqual({ kind: "success", response: {
      ...authority, databaseEpoch: "e1", requestId: "request-1", status: "confirmed", results: [],
    } });
    expect((await runBatch(db, manifest, authority, empty, { ...session, applicationClaim: "ignored" })).kind).toBe("success");
    for (const invalidSession of [{ userId: 7, role: "Member" }, { ...session, role: { _type: "Unknown", admin: true } }]) {
      expect(await runBatch(db, manifest, authority, empty, invalidSession))
        .toEqual({ kind: "error", error: { errorType: "InvalidSession", message: "InvalidSession" } });
    }
    for (const input of [
      { ...compiledInput, managed: true }, { ...compiledInput, role: { _type: "Member", admin: true } },
      { ...compiledInput, role: "Unknown" }, { ...compiledInput, details: { ...compiledInput.details, private: true } },
    ]) {
      expect(await runBatch(db, manifest, authority, { ...request, operations: [{ operation: compiledCreate.id, input }] }, session))
        .toEqual({ kind: "error", error: { errorType: "InvalidRequest", message: "InvalidRequest", index: 0 } });
    }
  } finally { db.close(); }
});

test("generator parity: top-level Int rejects fractions like Rust", () => {
  expect(compiledCreate.InputValidator.safeParse({ ...compiledInput, count: 1.5 }).success).toBe(false);
});

test("named wire results retain Rust-compatible enum objects and timestamps after codec validation", async () => {
  const db = await batchDatabase();
  try {
    const manifest = editManifest();
    manifest.queries.named.sql = [{ include: true, params: [], sql: "select json_object('role', json_object('_type', 'Member'), 'createdAt', 1) as items" }];
    manifest.queries.named.ReturnData = z.object({ items: z.array(z.object({ role: Role, createdAt: CoercedDate })) });
    const result = await runBatch(db, manifest, batchAuthority, batchRequest([{ operation: "named", input: {} }]), { userId: 7 });
    expect(result).toMatchObject({ kind: "success", response: {
      results: [{ index: 0, operation: "named", value: { items: [{ role: { _type: "Member" }, createdAt: 1 }] } }],
    } });
  } finally { db.close(); }
});
test("generator parity: Bool accepts only canonical booleans at every depth", () => {
  expect(compiledCreate.InputValidator.safeParse({ ...compiledInput, enabled: 1 }).success).toBe(false);
  expect(compiledCreate.InputValidator.safeParse({ ...compiledInput, details: { ...compiledInput.details, enabled: 1 } }).success).toBe(false);
});
test("generator parity: structured inputs require declared variant fields like Rust", () => {
  expect(compiledCreate.InputValidator.safeParse({ ...compiledInput, details: { _type: "Note" } }).success).toBe(false);
});

test("compiled recursive JSON preserves nested enums, dates, nulls, lists and dictionaries", async () => {
  const db = await batchDatabase();
  try {
    await db.execute("create table entries(id text primary key, release text, enabled integer, count integer, role text, details blob, updatedAt integer)");
    const manifest: BatchManifest = { version: 1, manifestVersion: compiledFingerprint, SessionValidator: compiledSession, queries: {
      [compiledCreate.id]: { ...compiledCreate, sql: compiledCreateSql },
    } };
    expect(compiledFingerprint).toMatch(/^sha256:[a-f0-9]{64}$/);
    const authority = { ...batchAuthority, manifest: compiledFingerprint, namespace: compiledCreate.primary_db };
    const details = { _type: "Bundle", when: "2026-01-01T00:00:00Z", role: "Member",
      children: [{ _type: "Note", count: 2, enabled: false }, { _type: "Bundle", when: 1767225600, role: { _type: "Admin" }, children: [], byName: {}, note: null }], byName: { empty: { _type: "Empty" } }, note: null };
    const request = { ...batchRequest([{ operation: compiledCreate.id, input: { ...compiledInput, details } }]), ...authority };
    const session = { userId: 7, role: "Member", unrelated: "value", applicationClaim: true, context: details };
    const effective = compiledSession.parse({ ...session, context: { _type: "Note", count: 2, enabled: 1, hostile__count: 999 } });
    expect(effective.context).toEqual({ _type: "Note", count: 2, enabled: true });
    expect(JSON.parse(buildArgs({}, effective, ["context"], [], [], ["context"]).session_context as string)).toEqual({ _type: "Note", count: 2, enabled: true });
    expect(compiledSession.safeParse({ ...session, context: { _type: "Note", enabled: true, hostile__count: 999 } }).success).toBe(false);
    expect((await runBatch(db, manifest, authority, request, session)).kind).toBe("success");
    const stored = (await db.execute("select json(details) as details from entries")).rows[0].details;
    expect(JSON.parse(stored as string)).toEqual({ ...details, when: 1767225600, role: { _type: "Member" } });
    for (const role of ["Member", { _type: "Member" }]) {
      const matchingSession = { ...session, role, context: { ...details, role } };
      const decoded = compiledSession.parse(matchingSession);
      const args = buildArgs({}, decoded, ["context", "role"], [], [], ["context"], compiledContextQuery.json_session_validators);
      expect(args.session_role).toBe("Member");
      expect(JSON.parse(args.session_context as string)).toEqual(JSON.parse(stored as string));
      const found = await run(db, { [compiledContextQuery.id]: { ...compiledContextQuery, sql: compiledContextSql } }, compiledContextQuery.id, {}, matchingSession);
      expect(found.response).toEqual({ entry: [{ id: compiledInput.id }] });
    }
    const different = await run(db, { [compiledContextQuery.id]: { ...compiledContextQuery, sql: compiledContextSql } }, compiledContextQuery.id, {}, {
      ...session, context: { ...details, role: "Admin" },
    });
    expect(different.response).toEqual({ entry: [] });
    const raw = { _type: "Raw", data: { arbitrary: [null, true, { _type: "Uninterpreted", extra: "retain" }] }, values: [1, null, 2], scalar: null };
    expect((await runBatch(db, manifest, authority, { ...request, operations: [{ operation: compiledCreate.id, input: { ...compiledInput, id: "raw", details: raw } }] }, session)).kind).toBe("success");
    expect(JSON.parse((await db.execute("select json(details) as details from entries where id = 'raw'")).rows[0].details as string)).toEqual(raw);
    expect(compiledCreate.InputValidator.safeParse({ ...compiledInput, details: { ...raw, data: undefined } }).success).toBe(false);
    expect(compiledCreate.InputValidator.safeParse({ ...compiledInput, details: { ...raw, data: null } }).success).toBe(false);
    for (const data of [{ lost: undefined }, { lossy: NaN }, { lossy: Infinity }]) {
      expect(compiledCreate.InputValidator.safeParse({ ...compiledInput, details: { ...raw, data } }).success).toBe(false);
    }
    for (const invalid of [
      { ...details, note: undefined }, { ...details, when: null },
      { ...details, children: [{ _type: "Note", count: 1 }] },
      { ...details, children: [{ _type: "Note", count: 1, enabled: true, unknown: "must not strip" }] },
      { ...details, byName: { bad: { _type: "Note", count: 1.5, enabled: true } } },
    ]) {
      expect(await runBatch(db, manifest, authority, { ...request, operations: [{ operation: compiledCreate.id, input: { ...compiledInput, details: invalid } }] }, session))
        .toMatchObject({ kind: "error", error: { errorType: "InvalidRequest", index: 0 } });
    }
    const malformedSession = { ...session, context: { ...details, children: [{ _type: "Note", count: 1 }] } };
    expect(await runBatch(db, manifest, authority, { ...request, operations: [] }, malformedSession))
      .toEqual({ kind: "error", error: { errorType: "InvalidSession", message: "InvalidSession" } });
  } finally { db.close(); }
});

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
