// @ts-nocheck
import { expect, test } from 'bun:test';
import 'fake-indexeddb/auto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createClient } from '@libsql/client';
import { z } from 'zod';
import initWasm from '../../server/wasm/pyre_wasm.js';
import { ensureDatabase, loadSchemaFromDatabase } from '../../server/schema';
import { catchup, rotateDatabaseEpoch } from '../../server/sync';
import { runWithSync } from '../../server/query-sync';
import loadElm from './test-engine';
import { IndexedDBStorage, IndexedDbService } from './service/indexeddb';
import { EntityStreamService } from './service/entity-stream';

await initWasm({ module_or_path: await Bun.file(new URL('../../server/wasm/pyre_wasm_bg.wasm', import.meta.url)).arrayBuffer() });

async function waitFor(check: () => boolean | Promise<boolean>) {
  for (let i = 0; i < 300; i++) {
    if (await check()) return;
    await Bun.sleep(5);
  }
  throw new Error('Timed out waiting for durable sync');
}

const source = `record Note {
    @public
    id Id.Int @id
    body String
}
record Token {
    @public
    id Id.Uuid @id
}
record Child {
    @public
    id Id.Int @id
    noteId Note.id
}
`;
const tables = ['notes', 'tokens', 'children'];
const schema = {
  tables: Object.fromEntries(tables.map((name) => [name, { name, links: {}, indices: [{ field: 'id', unique: true, primary: true }] }])),
  queryFieldToTable: Object.fromEntries(tables.map((name) => [name, name])),
};

test('SQLite/WASM direct deltas, Elm/IndexedDB handoff, offline resume and epoch reset', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'pyre-durable-'));
  const db = createClient({ url: `file:${join(directory, 'server.db')}` });
  const oldXhr = globalThis.XMLHttpRequest;
  const storage = new IndexedDBStorage(`durable-${crypto.randomUUID()}`);
  let hold = false;
  const held: (() => void)[] = [];
  const requests: any[] = [];
  let stage = 'initialize';
  class Xhr {
    listeners: Record<string, (() => void)[]> = {};
    status = 200; statusText = 'OK'; responseURL = ''; response = ''; responseType = ''; timeout = 0;
    addEventListener(type: string, fn: () => void) { (this.listeners[type] ??= []).push(fn); }
    open(_method: string, url: string) { this.responseURL = url; }
    setRequestHeader() {}
    abort() {}
    getAllResponseHeaders() { return ''; }
    send(body: string) {
      const request = JSON.parse(body);
      requests.push(request);
      const shouldHold = hold;
      hold = false;
      void catchup(db, request.syncCursor, {}, 1, 'main', request.databaseEpoch).then((page) => {
        const deliver = () => {
          this.response = JSON.stringify(page);
          (this.listeners.load ?? []).forEach((fn) => fn());
        };
        if (shouldHold) held.push(deliver); else deliver();
      });
    }
  }
  globalThis.XMLHttpRequest = Xhr;
  const entities = new EntityStreamService();
  const events: any[] = [];
  const sources: string[] = [];
  entities.subscribe({ tables: tables.map((tableName) => ({ tableName, where: { body: 'old' } })) }, (batch) => { events.push(...batch.changes); sources.push(batch.source); });
  const start = () => {
    const Elm = loadElm(Object.create(globalThis));
    const app = Elm.Main.init({ flags: { schema, server: { baseUrl: 'http://test', catchupPath: '/sync', databaseId: 'main' }, liveSync: { transport: 'sse' } } });
    const service = new IndexedDbService(storage, undefined, (groups, source) => entities.handleTableDelta(groups, source, 'main'), () => entities.reset('main'));
    service.attachPorts(app);
    const results: any[] = [];
    const errors: string[] = [];
    let status = '';
    app.ports.queryClientOut.subscribe((message) => { if (message.type === 'full') results.push(message.result); });
    app.ports.syncStateOut.subscribe((state) => { status = state.status; });
    app.ports.errorOut.subscribe((error) => errors.push(error));
    return {
      app, results, errors, live: () => status === 'live',
      wake: (message = { type: 'syncRequired', databaseId: 'main' }) => app.ports.receiveSSEMessage.send(message),
      query: async () => {
        app.ports.receiveQueryClientMessage.send({ type: 'register', queryId: 'all', querySource: { notes: { id: true, body: true }, tokens: { id: true }, children: { id: true } }, queryInput: {} });
        await Bun.sleep(5);
        return results.at(-1);
      },
    };
  };
  try {
    await ensureDatabase(db, 'Db', source);
    stage = 'install cascade';
    // Pyre currently doesn't declare cascade policy; install the SQLite FK and
    // rerun the normal migration path to regenerate its capture triggers.
    const childSql = String((await db.execute("select sql from sqlite_master where name = 'children'")).rows[0].sql);
    await db.executeMultiple(`drop table children; ${childSql.replace(/\)\s*$/, ', foreign key (noteId) references notes(id) on delete cascade)')};`);
    stage = 'regenerate capture';
    await ensureDatabase(db, 'Db', source);
    stage = 'load schema';
    await loadSchemaFromDatabase('main', db);
    await db.execute('pragma foreign_keys = on');
    await db.executeMultiple("insert into notes (id, body, updatedAt) values (1, 'old', 10); insert into tokens (id) values ('007'); insert into children (id, noteId) values (8, 1);");
    const engine = start();
    stage = 'initial catchup';
    await waitFor(engine.live);
    expect((await engine.query()).notes).toEqual([{ id: 1, body: 'old' }]);
    expect((await storage.getAllRows('tokens'))[0].id).toBe('007');
    expect(requests[0].syncCursor.version).toBe(2);
    const epoch = await storage.getDatabaseEpoch();

    const queries = { remove: { id: 'remove', operation: 'delete', sql: [{ include: true, params: [], sql: "delete from notes returning json_object('id', id) as note" }], session_args: [], optional_input_args: [], json_input_args: [], InputValidator: z.object({}), SessionValidator: z.object({}) } };
    const beforeLive = requests.length;
    const deleted = await runWithSync(db, queries, 'remove', {}, {}, new Map([['client', { session: {}, databaseId: 'main' }], ['other', { session: {}, databaseId: 'other' }]]), 'main');
    await deleted.sync((id, message) => { expect(id).toBe('client'); engine.wake(message); });
    await waitFor(async () => (await storage.getAllRows('notes')).length === 0 && engine.live());
    expect(requests.length).toBe(beforeLive);
    expect(sources).toContain('live');
    hold = true;
    engine.wake();
    await waitFor(() => held.length === 1);
    // The held page contains deletes; reinsert with the SAME timestamp while it
    // is in flight. A second wake must not be lost during catchup/persistence.
    await db.execute("insert into notes (id, body, updatedAt) values (1, 'new', 10)");
    engine.wake();
    held.shift()!();
    await waitFor(async () => (await storage.getAllRows('notes'))[0]?.body === 'new' && engine.live());
    expect((await engine.query()).notes).toEqual([{ id: 1, body: 'new' }]);
    expect(await storage.getAllRows('children')).toEqual([]);
    expect((await engine.query()).children).toEqual([]);
    expect(events).toContainEqual({ tableName: 'notes', id: 1, op: 'delete' });
    expect(events).toContainEqual({ tableName: 'children', id: 8, op: 'delete' });

    engine.wake({ type: 'delta', databaseId: 'main', databaseEpoch: epoch, serverRevision: 1, data: [{ table_name: 'notes', headers: ['id', 'body'], rows: [[1, 'late stale row']] }] });
    await Bun.sleep(30);
    expect((await engine.query()).notes).toEqual([{ id: 1, body: 'new' }]);
    const beforeFailure = await storage.getSyncCursor();
    const writePage = storage.putCatchupPage.bind(storage);
    let failNextWrite = true;
    storage.putCatchupPage = (...args) => {
      if (failNextWrite) { failNextWrite = false; return Promise.reject(new Error('injected storage failure')); }
      return writePage(...args);
    };
    await db.executeMultiple("insert into notes (id, body) values (99, 'uncached'); delete from notes; delete from tokens;");
    engine.wake();
    await waitFor(() => engine.errors.some((message) => message.includes('injected storage failure')));
    expect(await storage.getSyncCursor()).toEqual(beforeFailure);
    expect((await storage.getAllRows('notes'))[0].body).toBe('new');
    expect((await engine.query()).notes).toEqual([{ id: 1, body: 'new' }]);
    engine.wake();
    await waitFor(async () => (await storage.getAllRows('notes')).length === 0 && (await storage.getAllRows('tokens')).length === 0 && engine.live());
    expect((await engine.query()).notes).toEqual([]);
    expect((await engine.query()).tokens).toEqual([]);
    expect(events).toContainEqual({ tableName: 'notes', id: 99, op: 'delete' });
    expect(events).toContainEqual({ tableName: 'tokens', id: '007', op: 'delete' });
    await db.execute("insert into notes (id, body) values (3, 'offline victim')");
    const delayedRegistry = new Map();
    const delayedQueries = { update: { ...queries.remove, id: 'update', operation: 'update', sql: [
      { include: false, params: [], sql: "update notes set body = 'offline victim', updatedAt = unixepoch() where id = 3" },
      { include: true, params: [], sql: "select json_array(json_object('table_name', 'notes', 'headers', json_array('id', 'body', 'updatedAt'), 'rows', (select json_group_array(json_array(id, body, updatedAt)) from notes where id = 3))) as _affectedRows" },
    ] } };
    const delayedPublication = await runWithSync(db, delayedQueries, 'update', {}, {}, delayedRegistry, 'main');
    engine.wake();
    await waitFor(async () => (await storage.getAllRows('notes'))[0]?.id === 3 && engine.live());
    const persisted = await storage.getSyncCursor();
    await db.execute('delete from notes where id = 3');
    const requestCount = requests.length;
    // Keep the safe timestamp interval, but replay retained tombstones to
    // restore absent-key observations before accepting delayed publications.
    const restarted = start();
    await waitFor(restarted.live);
    expect(requests[requestCount].syncCursor.tables).toEqual(Object.fromEntries(Object.entries(persisted.tables).map(([name, cursor]) => [name, { ...cursor, last_seen_primary_key: null, last_seen_delete_sequence: 0 }])));
    expect(await storage.getAllRows('notes')).toEqual([]);
    expect((await restarted.query()).notes).toEqual([]);
    // This genuine mutation committed before the consumed delete, but selects
    // its recipients only now, after the new stream registered and caught up.
    delayedRegistry.set('restarted', { session: {}, databaseId: 'main' });
    const beforeDelayedPublication = requests.length;
    await delayedPublication.sync((_id, message) => restarted.wake(message));
    await Bun.sleep(30);
    expect(await storage.getAllRows('notes')).toEqual([]);
    expect((await restarted.query()).notes).toEqual([]);
    expect(requests.length).toBe(beforeDelayedPublication);

    // Offline delete and epoch rotation: restored old cursors are reset before
    // applying the new baseline, then late old-epoch notifications are harmless.
    await db.execute("insert into notes (id, body, updatedAt) values (2, 'baseline', 1), (4, 'baseline two', 1), (5, 'baseline three', 1)");
    const newEpoch = await rotateDatabaseEpoch(db);
    restarted.wake({ type: 'syncRequired', databaseId: 'main', databaseEpoch: newEpoch });
    await waitFor(async () => await storage.getDatabaseEpoch() === newEpoch && (await storage.getAllRows('notes'))[0]?.body === 'baseline' && restarted.live());
    restarted.wake({ type: 'delta', databaseId: 'main', databaseEpoch: epoch, serverRevision: 999999, data: [{ table_name: 'notes', headers: ['id', 'body'], rows: [[1, 'obsolete epoch']] }] });
    await Bun.sleep(30);
    expect((await restarted.query()).notes).toEqual([{ id: 2, body: 'baseline' }, { id: 4, body: 'baseline two' }, { id: 5, body: 'baseline three' }]);
    await expect(catchup(db, { tables: {} }, {}, 1, 'main')).rejects.toThrow('protocol 2 required');
  } catch (error) {
    throw new Error(`Durable sync failed during ${stage}`, { cause: error });
  } finally {
    globalThis.XMLHttpRequest = oldXhr;
    await storage.deleteDatabase();
    db.close();
    rmSync(directory, { recursive: true, force: true });
  }
}, 15000);

test('IndexedDB deletion/cursor writes are atomic, typed, idempotent, and reject stale pages', async () => {
  const storage = new IndexedDBStorage(`atomic-${crypto.randomUUID()}`);
  const cursor = (sequence: number) => ({ tables: { notes: { permission_hash: 'v2:hash', last_seen_updated_at: sequence, last_seen_primary_key: 1, last_seen_delete_sequence: sequence } } });
  try {
    await storage.putCatchupPage('epoch', 1, cursor(1), [{ table_name: 'notes', headers: ['id', 'body'], rows: [[1, 'integer'], ['1', 'text']] }]);
    await expect(storage.putCatchupPage('epoch', 2, cursor(2), [
      { table_name: 'notes', headers: ['$delete'], rows: [[1]] },
      { table_name: 'notes', headers: ['body'], rows: [['invalid missing key']] },
    ])).rejects.toThrow();
    expect((await storage.getAllRows('notes')).length).toBe(2);
    expect(await storage.getSyncCursor()).toEqual(cursor(1));
    await storage.putCatchupPage('epoch', 3, cursor(3), [{ table_name: 'notes', headers: ['$delete'], rows: [[1], [999], [999]] }]);
    expect(await storage.getAllRows('notes')).toEqual([{ id: '1', body: 'text' }]);
    expect(await storage.getSyncCursor()).toEqual(cursor(3));
    await expect(storage.putCatchupPage('epoch', 3, cursor(2), [{ table_name: 'notes', headers: ['id'], rows: [[1]] }])).rejects.toThrow('Stale deletion cursor');
    const oldPermissions = cursor(2);
    oldPermissions.tables.notes.permission_hash = 'v2:old-permissions';
    await expect(storage.putCatchupPage('epoch', 2, oldPermissions, [{ table_name: 'notes', headers: ['id'], rows: [[1]] }])).rejects.toThrow('Stale catchup revision');
    expect(await storage.getAllRows('notes')).toEqual([{ id: '1', body: 'text' }]);
    expect(await storage.getSyncCursor()).toEqual(cursor(3));
    await storage.resetForDatabaseEpoch('new');
    await expect(storage.putCatchupPage('epoch', 4, cursor(4), [{ table_name: 'notes', headers: ['id'], rows: [[1]] }])).rejects.toThrow('epoch');
    expect(await storage.getAllRows('notes')).toEqual([]);
    // SQLite BINARY order is UTF-8, which differs from JS UTF-16 for these keys.
    await storage.putCatchupPage('new', 0, { tables: { tokens: { permission_hash: 'v2:text', last_seen_updated_at: 0, last_seen_primary_key: '\uE000' } } }, []);
    await storage.putCatchupPage('new', 0, { tables: { tokens: { permission_hash: 'v2:text', last_seen_updated_at: 0, last_seen_primary_key: '\u{10000}' } } }, []);
  } finally {
    await storage.deleteDatabase();
  }
});
