// @ts-nocheck
import { expect, test } from "bun:test";
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createClient } from "@libsql/client";
import { z } from "zod";
import { runWithSync } from "./query-sync";
import initWasm from "./wasm/pyre_wasm.js";
import { ensureDatabase, loadSchemaFromDatabase } from "./schema";
import { catchup } from './sync';

await initWasm({ module_or_path: await Bun.file(new URL('./wasm/pyre_wasm_bg.wasm', import.meta.url)).arrayBuffer() });

const queries = {
  remove: {
    id: "remove", operation: "delete", sql: [{ include: true, params: [], sql: "delete from notes returning json_object('id', id) as note" }],
    session_args: [], optional_input_args: [], json_input_args: [], InputValidator: z.object({}), SessionValidator: z.object({}),
  },
};

test('catchup cannot certify a timestamp past an in-flight writer and releases its page barrier', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'pyre-fence-'));
  const url = `file:${join(directory, 'db')}`;
  const writer = createClient({ url });
  const reader = createClient({ url });
  try {
    await writer.execute('pragma journal_mode = wal');
    await ensureDatabase(writer, 'Db', 'record Note {\n    @public\n    id Id.Int @id\n}\n');
    await loadSchemaFromDatabase('main', writer);
    await reader.execute('pragma busy_timeout = 0');
    const tx = await writer.transaction('write');
    try {
      await tx.execute('insert into notes (id, updatedAt) values (1, 10)');
      await expect(catchup(reader, { version: 2, tables: {} }, {}, 1, 'main')).rejects.toThrow();
      await tx.commit();
    } finally { tx.close(); }
    const page = await catchup(reader, { version: 2, tables: {} }, {}, 1, 'main');
    expect(page.tables.notes.changes[0].id).toBe(1);
    expect(page.snapshotTimestamp).toBeGreaterThanOrEqual(10);
    await writer.execute('delete from notes');
  } finally { writer.close(); reader.close(); rmSync(directory, { recursive: true, force: true }); }
});

test("deletes broadcast transaction-stamped direct IDs only to scoped connections", async () => {
  const directory = mkdtempSync(join(tmpdir(), 'pyre-live-'));
  const db = createClient({ url: `file:${join(directory, 'db')}` });
  try {
    await ensureDatabase(db, 'Db', 'record Note {\n    @public\n    id Id.Int @id\n}\n');
    await loadSchemaFromDatabase('main', db);
    await db.execute('insert into notes (id) values (1)');
    const registry = new Map([
      ["same", { session: {}, databaseId: "main" }],
      ["other", { session: {}, databaseId: "other" }],
      ["origin", { session: {}, databaseId: "main" }],
    ]);
    const result = await runWithSync(db, queries, "remove", {}, {}, registry, "main", "origin");
    registry.set('late', { session: {}, databaseId: 'main' });
    const sent: unknown[] = [];
    await result.sync((id, message) => sent.push({ id, message }));
    expect(sent).toEqual(['same', 'late'].map(id => ({ id, message: expect.objectContaining({ type: 'delta', serverRevision: 2, data: [{ table_name: 'notes', headers: ['$delete'], rows: [[1]] }] }) })));
    expect(result.response).toMatchObject({ syncVersion: 2, serverRevision: 2, sync: { type: "delta" }, result: { note: [{ id: 1 }] } });
  } finally { db.close(); rmSync(directory, { recursive: true, force: true }); }
});

test("a rolled back mutation never returns a publication callback", async () => {
  const directory = mkdtempSync(join(tmpdir(), 'pyre-rollback-'));
  const db = createClient({ url: `file:${join(directory, 'db')}` });
  try {
    await ensureDatabase(db, 'Db', 'record Note {\n    @public\n    id Id.Int @id\n}\n');
    await db.execute('insert into notes (id) values (1)');
    const broken = { ...queries, remove: { ...queries.remove, sql: [...queries.remove.sql, { include: false, params: [], sql: 'insert into missing values (1)' }] } };
    await expect(runWithSync(db, broken, 'remove', {}, {})).rejects.toThrow();
    expect((await db.execute('select id from notes')).rows).toHaveLength(1);
    expect((await db.execute('select * from _pyre_sync_tombstones')).rows).toHaveLength(0);
  } finally { db.close(); rmSync(directory, { recursive: true, force: true }); }
});

test('live upserts are permission filtered and keep their precommit revision, including the origin response', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'pyre-live-rows-'));
  const db = createClient({ url: `file:${join(directory, 'db')}` });
  try {
    await ensureDatabase(db, 'Db', 'session {\n    userId Int\n}\nrecord Note {\n    id Id.Int @id\n    ownerId Int\n    body String\n    @allow(*) { ownerId == Session.userId }\n}\n');
    await loadSchemaFromDatabase('main', db);
    await db.execute("insert into notes (id, ownerId, body) values (1, 1, 'old')");
    const update = { ...queries.remove, id: 'change', operation: 'update', sql: [
      { include: false, params: [], sql: "update notes set body = 'committed', updatedAt = unixepoch() where id = 1" },
      { include: true, params: [], sql: "select json_array(json_object('table_name', 'notes', 'headers', json_array('id', 'ownerId', 'body', 'updatedAt'), 'rows', (select json_group_array(json_array(id, ownerId, body, updatedAt)) from notes where id = 1))) as _affectedRows" },
    ] };
    const result = await runWithSync(db, { change: update }, 'change', {}, { userId: 1 }, new Map([
      ['allowed', { session: { userId: 1 }, databaseId: 'main' }],
      ['hidden', { session: { userId: 2 }, databaseId: 'main' }],
    ]), 'main');
    await db.execute("update notes set body = 'later' where id = 1");
    const sent = new Map();
    await result.sync((id, message) => sent.set(id, message));
    expect(sent.get('allowed')).toMatchObject({ type: 'delta', serverRevision: 2 });
    expect(sent.get('allowed').data[0].rows[0][2]).toBe('committed');
    expect(sent.get('hidden').data).toEqual([]);
    expect(result.response.sync.data[0].rows[0][2]).toBe('committed');
    expect(result.response.serverRevision).toBe(2);

    const remove = { ...update, sql: [...update.sql, { include: false, params: [], sql: 'delete from notes where id = 1' }] };
    const deleted = await runWithSync(db, { change: remove }, 'change', {}, { userId: 1 }, undefined, 'main');
    await deleted.sync(() => {});
    const groups = deleted.response.sync.data;
    expect(groups[0]).toEqual({ table_name: 'notes', headers: ['$delete'], rows: [[1]] });
    expect(groups.slice(1).flatMap(group => group.rows)).toEqual([]);
  } finally { db.close(); rmSync(directory, { recursive: true, force: true }); }
});
