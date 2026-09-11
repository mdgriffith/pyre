import { afterEach, expect, mock, spyOn, test } from "bun:test";
import { z } from "zod";
import { namespace, planKey, scopedEdit, type Edit } from "@pyre/core/local-edits";
import { localEdits, type BindOptions } from "../../local-edits";
import { Main, Project, Task, Audit, Commands, batch, type ProjectId } from "../../../../target/local-edits-fixture/typescript/edits";
import { manifest } from "../../../../target/local-edits-fixture/typescript/server";
import { openDatabase } from "./database";
import type { BatchSyncRecipient } from "../../query-sync";
import * as execution from "../../query-sync";

const cleanup: (() => void)[] = [];
afterEach(() => { for (const close of cleanup.splice(0)) close(); });
async function setup() {
  const { database, close } = await openDatabase();
  cleanup.push(close);
  const options: BindOptions<Main> = { database, databaseId: "authorized-seed", namespace: Main, manifest, session: { userId: 7 } };
  const revision = async () => Number((await database.execute("select server_revision from _pyre_sync")).rows[0].server_revision);
  const count = async (table: "projects" | "tasks" | "audits") => Number((await database.execute(`select count(*) as n from ${table}`)).rows[0].n);
  return { database, options, edits: localEdits.bind(options), revision, count };
}

test("generated tuple batch: related UUIDs, integer identities, mixed named commands, zero subscribers", async () => {
  const { edits, database, revision } = await setup();
  const transaction = database.transaction.bind(database);
  let writes = 0;
  database.transaction = async mode => { if (mode === "write") writes++; return transaction(mode); };
  const id = crypto.randomUUID() as ProjectId;
  const taskId = crypto.randomUUID();
  const outcome = await edits.submit(batch([
    Project.create({ id, name: "project", owner: 7 }),
    Task.create({ id: taskId, project: id, title: "task", owner: 7 }),
    Audit.create({ message: "first" }),
    Commands.namedAudit({ message: "named" }),
    Audit.create({ message: "last" }),
  ]));
  expect(outcome.kind).toBe("confirmed");
  if (outcome.kind !== "confirmed") return;
  expect(outcome.result.slice(0, 3)).toEqual([{ id }, { id: taskId }, { id: 1 }]);
  expect(outcome.result[3].audit[0]).toMatchObject({ id: 2, message: "named" });
  expect(outcome.result[3].audit[0].updatedAt).toBeInstanceOf(Date);
  expect(outcome.result[4]).toEqual({ id: 3 });
  expect(outcome.commitRevision).toBe(1);
  expect((await database.execute("select project from tasks")).rows[0].project).toBe(id);
  expect(await revision()).toBe(1);
  expect(writes).toBe(1);
});

test("operation N forbidden, missing, and SQL constraint failures roll back all preceding operations", async () => {
  for (const kind of ["forbidden", "missing", "constraint"] as const) {
    const { edits, count, revision } = await setup();
    const id = crypto.randomUUID() as ProjectId;
    const failure = kind === "forbidden" ? Project.create({ id, name: "forbidden", owner: 8 })
      : kind === "missing" ? Project.update(crypto.randomUUID() as ProjectId, { name: "missing" })
      : Project.create({ id, name: "duplicate", owner: 7 });
    const result = await edits.submit(batch([
      Project.create({ id, name: "prefix", owner: 7 }),
      Audit.create({ message: "also rolled back" }),
      failure,
    ]));
    expect(result).toEqual({ kind: "rejected", index: 2,
      code: kind === "constraint" ? "TransactionFailed" : "TargetNotWritable" });
    expect(await count("projects")).toBe(0);
    expect(await count("audits")).toBe(0);
    expect(await revision()).toBe(0);
  }
});

test("explicit session is required and decoded; no implicit administrator", async () => {
  const { options, count } = await setup();
  for (const session of [undefined, null, {}, { userId: "7" }]) {
    expect(() => localEdits.bind({ ...options, session: session as any })).toThrow("InvalidSession");
  }
  const forbidden = localEdits.bind({ ...options, session: { userId: 8, isAdmin: true } });
  expect(await forbidden.submit(Audit.create({ message: "denied" }))).toMatchObject({
    kind: "rejected", code: "TargetNotWritable", index: 0,
  });
  expect(await count("audits")).toBe(0);
});

test("binding rejects manifest/namespace/target mismatches and descriptor SQL is never authority", async () => {
  const { options, edits, count } = await setup();
  expect(() => localEdits.bind({ ...options, databaseId: " " })).toThrow();
  expect(() => localEdits.bind({ ...options, manifest: { ...manifest, manifestVersion: "wrong" } })).toThrow("InvalidRequest");
  const operation = Audit.create({ message: "no" })[planKey].operations[0].definition;
  for (const scope of [namespace<Main>("Other", Main.manifest), namespace<Main>(Main.name, "stale")]) {
    expect(await edits.submit(scopedEdit(scope, operation, { message: "no" }))).toEqual({ kind: "rejected", code: "InvalidRequest" });
  }
  const other = localEdits.bind({ ...options, namespace: namespace<Main>("Other", Main.manifest) });
  expect(await other.submit(scopedEdit(namespace<Main>("Other", Main.manifest), operation, { message: "no" })))
    .toMatchObject({ kind: "rejected", code: "InvalidRequest" });
  expect(await edits.submit(scopedEdit(Main, { ...operation, id: "unknown" }, { message: "no" })))
    .toMatchObject({ kind: "rejected", index: 0 });
  expect(await count("audits")).toBe(0);
});

test("generated update cardinality is strict even when compiled SQL accidentally targets many rows", async () => {
  const { options, edits, database, revision } = await setup();
  const created = await edits.submit(batch([Audit.create({ message: "one" }), Audit.create({ message: "two" })]));
  if (created.kind !== "confirmed") throw new Error(JSON.stringify(created));
  const edit = Audit.update(created.result[0].id, { message: "changed" });
  const id = edit[planKey].operations[0].definition.id;
  const query = manifest.queries[id];
  const broken = { ...manifest, queries: { ...manifest.queries, [id]: { ...query, sql: [{ include: true,
    params: [], sql: "update audits set message = 'changed' returning id as _pyreEditId" }] } } };
  expect(await localEdits.bind({ ...options, manifest: broken }).submit(edit)).toMatchObject({
    kind: "rejected", code: "TargetNotWritable", index: 0,
  });
  expect((await database.execute("select message from audits order by id")).rows).toEqual([{ message: "one" }, { message: "two" }]);
  expect(await revision()).toBe(1);
});

test("captures binding authority, compiled metadata, plan inputs and decoders before async work", async () => {
  const { options, database } = await setup();
  const source = Audit.create({ message: "captured" })[planKey];
  const id = source.operations[0].definition.id;
  const sql = structuredClone(manifest.queries[id].sql);
  options.manifest = { ...manifest, queries: { ...manifest.queries, [id]: { ...manifest.queries[id], sql } } };
  const edits = localEdits.bind(options);
  options.session.userId = 8;
  options.databaseId = "changed";
  sql[0].sql = "invalid SQL";
  const definition = { ...source.operations[0].definition };
  const input = { message: "captured" };
  const plan = { ...source, operations: [{ definition, input }] };
  const promise = edits.submit({ [planKey]: plan } as unknown as Edit<Main, { id: number }>);
  input.message = "mutated";
  definition.id = "mutated operation";
  definition.decodeResult = () => { throw new Error("mutated decoder"); };
  plan.result = () => { throw new Error("mutated result"); };
  const outcome = await promise;
  expect(outcome).toEqual({ kind: "confirmed", result: { id: 1 }, commitRevision: 1 });
  expect((await database.execute("select message from audits")).rows[0].message).toBe("captured");
});

test("publishes existing syncRequired protocol to current matching subscribers without origin registration", async () => {
  const { options, database } = await setup();
  const databaseEpoch = String((await database.execute("select database_epoch from _pyre_sync")).rows[0].database_epoch);
  const fence = { databaseId: options.databaseId, namespace: Main.name, manifest: Main.manifest,
    databaseEpoch, instance: "listener", authGeneration: 4 };
  const recipients = new Map<string, BatchSyncRecipient>();
  const sent: { id: string; message: any }[] = [];
  const edits = localEdits.bind({ ...options, connectedSessions: recipients, sendToSession: (id, message) => {
    if (id === "broken") throw new Error("delivery failed");
    sent.push({ id, message });
  } });
  const pending = edits.submit(Audit.create({ message: "published" }));
  recipients.set("broken", { session: { userId: 7 }, fence });
  recipients.set("active", { session: { userId: 7 }, fence });
  recipients.set("other-target", { session: { userId: 7 }, fence: { ...fence, databaseId: "other" } });
  expect(await pending).toEqual({ kind: "confirmed", result: { id: 1 }, commitRevision: 1 });
  expect(sent).toEqual([{ id: "active", message: { type: "syncRequired", ...fence, serverRevision: 1,
    reconciliation: { kind: "replaceRequired", atLeast: 1, invalidate: true, minimumSafeRevision: 1 } } }]);
});

test("postcommit decoder errors retain commit evidence, index and publication; never retry", async () => {
  const { options, count, revision } = await setup();
  const databaseEpoch = String((await options.database.execute("select database_epoch from _pyre_sync")).rows[0].database_epoch);
  options.connectedSessions = new Map([["active", { session: { userId: 7 }, fence: {
    databaseId: options.databaseId, namespace: Main.name, manifest: Main.manifest,
    databaseEpoch, instance: "active", authGeneration: 0,
  } }]]);
  let published = 0;
  options.sendToSession = () => { published++; };
  const operation = Audit.create({ message: "committed" })[planKey].operations[0].definition;
  const result = await localEdits.bind(options).submit(scopedEdit(Main, {
    ...operation, decodeResult() { throw new Error("bad codec"); },
  }, { message: "committed" }));
  expect(result).toEqual({ kind: "acceptedUnreconciled", code: "InvalidResult", index: 0, commitRevision: 1 });
  expect(await count("audits")).toBe(1);
  expect(await revision()).toBe(1);
  expect(published).toBe(1);
});

test("lost commit acknowledgement is unknown and does not replay the write", async () => {
  const { options, database, count } = await setup();
  const transaction = database.transaction.bind(database);
  let commits = 0;
  database.transaction = async mode => {
    const tx = await transaction(mode);
    const commit = tx.commit.bind(tx);
    tx.commit = async () => { commits++; await commit(); throw new Error("lost acknowledgement"); };
    return tx;
  };
  expect(await localEdits.bind(options).submit(Audit.create({ message: "once" }))).toEqual({ kind: "outcomeUnknown", code: "OutcomeUnknown" });
  expect(commits).toBe(1);
  expect(await count("audits")).toBe(1);
});

test("empty batches confirm [] with no database I/O, executor call, revision or publication", async () => {
  const { database, options, revision } = await setup();
  const io = mock(() => { throw new Error("Empty submission must not touch the database"); });
  const forbiddenDatabase = new Proxy(database, { get: () => io });
  const publish = mock(() => { throw new Error("Empty submission must not publish"); });
  const registrations = new Map<string, BatchSyncRecipient>();
  const iterate = spyOn(registrations, Symbol.iterator);
  const execute = spyOn(execution, "runBatchWithSync");
  try {
    const edits = localEdits.bind({ ...options, database: forbiddenDatabase, connectedSessions: registrations, sendToSession: publish });
    expect(await edits.submit(batch([]))).toEqual({ kind: "confirmed", result: [] });
    expect(io).not.toHaveBeenCalled();
    expect(execute).not.toHaveBeenCalled();
    expect(iterate).not.toHaveBeenCalled();
    expect(publish).not.toHaveBeenCalled();
  } finally { execute.mockRestore(); iterate.mockRestore(); }
  expect(await revision()).toBe(0);
});

test("single generated create, update and delete return their authorized identity", async () => {
  const { edits, count, revision } = await setup();
  const created = await edits.submit(Audit.create({ message: "original" }));
  if (created.kind !== "confirmed") throw new Error(JSON.stringify(created));
  const id = created.result.id;
  expect(await edits.submit(Audit.update(id, { message: "changed" }))).toEqual({ kind: "confirmed", result: { id }, commitRevision: 2 });
  expect(await edits.submit(Audit.delete(id))).toEqual({ kind: "confirmed", result: { id }, commitRevision: 3 });
  expect(await count("audits")).toBe(0);
  expect(await revision()).toBe(3);
});

test("unexpected executor throws or rejections resolve unknown, even if the write committed", async () => {
  for (const phase of ["throw", "reject", "postcommit"] as const) {
    const { edits, count, revision } = await setup();
    const original = execution.runBatchWithSync;
    const execute = spyOn(execution, "runBatchWithSync").mockImplementation((...args) => {
      if (phase === "throw") throw new Error("unexpected synchronous failure");
      if (phase === "reject") return Promise.reject(new Error("unexpected asynchronous failure"));
      return original(...args).then(() => { throw new Error("lost executor result after commit"); });
    });
    try {
      await expect(edits.submit(Audit.create({ message: phase }))).resolves.toEqual({ kind: "outcomeUnknown", code: "OutcomeUnknown" });
      expect(execute).toHaveBeenCalledTimes(1);
    } finally { execute.mockRestore(); }
    expect(await count("audits")).toBe(phase === "postcommit" ? 1 : 0);
    expect(await revision()).toBe(phase === "postcommit" ? 1 : 0);
  }
});

test("epoch lookup failure inside the queued transaction is a definite noncommit", async () => {
  const { database, edits, count } = await setup();
  const transaction = database.transaction.bind(database);
  const acquire = spyOn(database, "transaction").mockImplementation(async mode => {
    const tx = await transaction(mode);
    const execute = tx.execute.bind(tx);
    tx.execute = statement => statement === "select database_epoch from _pyre_sync where id = 1"
      ? Promise.reject(new Error("read failed")) : execute(statement);
    return tx;
  });
  try {
    await expect(edits.submit(Audit.create({ message: "not dispatched" }))).resolves.toEqual({ kind: "rejected", code: "TransactionFailed" });
  } finally { acquire.mockRestore(); }
  expect(await count("audits")).toBe(0);
});

test("seed bindings reserve the shared executor queue before any database I/O", async () => {
  const { database, options, edits } = await setup();
  const databaseEpoch = String((await database.execute("select database_epoch from _pyre_sync")).rows[0].database_epoch);
  const execute = database.execute.bind(database);
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  let entered!: () => void;
  const firstRead = new Promise<void>(resolve => { entered = resolve; });
  let reads = 0;
  database.execute = async statement => {
    if (++reads === 1) { entered(); await gate; }
    return execute(statement);
  };
  const first = edits.submit(Audit.create({ message: "first" }));
  await firstRead;
  const second = localEdits.bind(options).submit(Audit.create({ message: "second" }));
  const operation = Audit.create({ message: "third" })[planKey].operations[0];
  const authority = { databaseId: options.databaseId, namespace: Main.name, manifest: Main.manifest,
    instance: "network", authGeneration: 0 };
  const third = execution.runBatchWithSync(database, manifest, authority, {
    version: 1, ...authority, databaseEpoch, requestId: "third", sequence: 1,
    operations: [{ operation: operation.definition.id, input: operation.input }],
  }, options.session);
  await Bun.sleep(10);
  release();
  expect((await first).kind).toBe("confirmed");
  expect((await second).kind).toBe("confirmed");
  expect((await third).kind).toBe("success");
  expect((await database.execute("select message from audits order by id")).rows)
    .toEqual([{ message: "first" }, { message: "second" }, { message: "third" }]);
});

test("committed results must match operation count, index, manifest ID and generated codecs", async () => {
  for (const defect of ["count", "hole", "index", "operation", "codec", "assembly"] as const) {
    const { edits, count, revision } = await setup();
    const plan = batch([Audit.create({ message: "one" }), Audit.create({ message: "two" })]);
    const submitted = defect === "assembly" ? { [planKey]: { ...plan[planKey], result() { throw new Error("assembly failed"); } } } as typeof plan : plan;
    const original = execution.runBatchWithSync;
    const execute = spyOn(execution, "runBatchWithSync").mockImplementation(async (...args) => {
      const result = await original(...args);
      if (result.kind !== "success") throw new Error("Expected commit");
      if (defect === "count") result.response.results.pop();
      if (defect === "hole") delete result.response.results[1];
      if (defect === "index") result.response.results[1].index = 0;
      if (defect === "operation") result.response.results[1].operation = "wrong-manifest-id";
      if (defect === "codec") result.response.results[1].value = { id: "not-an-integer" };
      return result;
    });
    try {
      expect(await edits.submit(submitted)).toEqual({ kind: "acceptedUnreconciled", code: "InvalidResult", commitRevision: 1,
        ...(["hole", "index", "operation", "codec"].includes(defect) ? { index: 1 } : {}) });
    } finally { execute.mockRestore(); }
    expect(await count("audits")).toBe(2);
    expect(await revision()).toBe(1);
  }
});

test("compiled named result validation fails inside the transaction, before typed descriptor decoding", async () => {
  const { options, count, revision } = await setup();
  const named = Commands.namedAudit({ message: "invalid named result" });
  const id = named[planKey].operations[0].definition.id;
  const invalid = { ...manifest, queries: { ...manifest.queries, [id]: { ...manifest.queries[id], ReturnData: z.never() } } };
  const result = await localEdits.bind({ ...options, manifest: invalid }).submit(batch([Audit.create({ message: "prefix" }), named]));
  expect(result).toEqual({ kind: "rejected", code: "InvalidRequest", index: 1 });
  expect(await count("audits")).toBe(0);
  expect(await revision()).toBe(0);
});

test("empty generated updates reject as InvalidEdit before any I/O", async () => {
  const { database, edits } = await setup();
  const created = await edits.submit(Audit.create({ message: "original" }));
  if (created.kind !== "confirmed") throw new Error(JSON.stringify(created));
  const io = spyOn(database, "execute").mockRejectedValue(new Error("must not read"));
  try {
    expect(await edits.submit(Audit.update(created.result.id, {}))).toEqual({ kind: "rejected", code: "InvalidEdit", index: 0 });
    expect(io).not.toHaveBeenCalled();
  } finally { io.mockRestore(); }
});

test("malformed commit evidence cannot confirm an otherwise decodable result", async () => {
  for (const defect of ["manifest", "revision"] as const) {
    const { edits, count } = await setup();
    const original = execution.runBatchWithSync;
    const execute = spyOn(execution, "runBatchWithSync").mockImplementation(async (...args) => {
      const result = await original(...args);
      if (result.kind !== "success") throw new Error("Expected commit");
      if (defect === "manifest") result.response.manifest = "wrong-manifest";
      else delete result.response.commitRevision;
      return result;
    });
    try {
      expect(await edits.submit(Audit.create({ message: "committed" }))).toEqual({ kind: "outcomeUnknown", code: "OutcomeUnknown" });
    } finally { execute.mockRestore(); }
    expect(await count("audits")).toBe(1);
  }
});
