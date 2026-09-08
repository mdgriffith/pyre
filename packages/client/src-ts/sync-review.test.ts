// @ts-nocheck
// Permanent versions of the independent review's adversarial probes.
import { expect, test } from 'bun:test';
import 'fake-indexeddb/auto';
import loadElm from '../dist/engine.mjs';
import { IndexedDBStorage, IndexedDbService } from './service/indexeddb';

async function until(check) {
  for (let i = 0; i < 200; i++) {
    if (await check()) return;
    await Bun.sleep(5);
  }
  throw new Error('timeout');
}
const entry = (timestamp = 10, key = 2, deletion = 5) => ({ permission_hash: 'timestamp-v2:test', last_seen_updated_at: timestamp, last_seen_primary_key: key, last_seen_delete_sequence: deletion });
const row = (id, body, updatedAt = 10) => ({ table_name: 'notes', headers: ['id', 'body', 'updatedAt'], rows: [[id, body, updatedAt]] });
const page = (revision, changes, cursor = entry(), more = false, timestamp = 10) => ({ syncVersion: 2, databaseId: 'main', databaseEpoch: 'epoch', serverRevision: revision, snapshotTimestamp: timestamp, tables: { notes: { ...cursor, changes } }, has_more: more });
const change = (id, body, updatedAt = 10) => ({ op: 'row', id, row: { id, body, updatedAt } });
const deleted = id => ({ op: 'delete', id });

async function harness(run, seed = true, cached = []) {
  const previous = globalThis.XMLHttpRequest;
  const requests = [];
  class Xhr {
    listeners = {}; status = 200; statusText = 'OK'; responseURL = ''; response = ''; responseType = ''; timeout = 0;
    addEventListener(type, fn) { (this.listeners[type] ??= []).push(fn); }
    open(_method, url) { this.responseURL = url; }
    setRequestHeader() {} abort() {} getAllResponseHeaders() { return ''; }
    send(body) { requests.push({ request: JSON.parse(body), respond: result => { this.response = JSON.stringify(result); this.listeners.load.forEach(fn => fn()); } }); }
  }
  globalThis.XMLHttpRequest = Xhr;
  const storage = new IndexedDBStorage(`review-${crypto.randomUUID()}`);
  if (seed) await storage.putCatchupPage('epoch', 5, { tables: { notes: entry() } }, cached);
  const Elm = loadElm(Object.create(globalThis));
  const app = Elm.Main.init({ flags: { schema: { tables: { notes: { name: 'notes', links: {}, indices: [{ field: 'id', unique: true, primary: true }] } }, queryFieldToTable: { notes: 'notes' } }, server: { baseUrl: 'http://test', catchupPath: '/sync', databaseId: 'main' }, liveSync: { transport: 'sse' } } });
  new IndexedDbService(storage).attachPorts(app);
  let status = '';
  const errors = [];
  app.ports.syncStateOut.subscribe(state => { status = state.status; });
  app.ports.errorOut.subscribe(error => errors.push(error));
  const send = message => app.ports.receiveSSEMessage.send({ databaseId: 'main', databaseEpoch: 'epoch', ...message });
  app.ports.sseOut.subscribe(() => send({ type: 'connected', connectionId: 'one' }));
  try {
    await until(() => requests.length === 1);
    await run({ storage, requests, send, errors, live: () => status === 'live' });
  } finally {
    globalThis.XMLHttpRequest = previous;
    await storage.deleteDatabase();
  }
}

test('restored tombstone cursor prevents delayed old publication resurrection during handoff and steady state', () => harness(async ({ storage, requests, send, live }) => {
  // Key 1 was deleted at revision 5 and that deletion is already persisted.
  // The mock server returns that retained tombstone only if it is requested.
  expect(requests[0].request.syncCursor.tables.notes.last_seen_delete_sequence).toBe(0);
  send({ type: 'delta', serverRevision: 4, data: [row(1, 'obsolete')] });
  requests[0].respond(page(10, [deleted(1), change(2, 'current')]));
  await until(live);
  expect(await storage.getAllRows('notes')).toEqual([{ id: 2, body: 'current', updatedAt: 10 }]);
  send({ type: 'delta', serverRevision: 4, data: [row(1, 'obsolete again')] });
  send({ type: 'delta', serverRevision: 11, data: [row(3, 'new independent key')] });
  await until(async () => (await storage.getAllRows('notes')).some(row => row.id === 3) && live());
  expect((await storage.getAllRows('notes')).map(row => row.id)).toEqual([2, 3]);
  expect(requests).toHaveLength(1);
}));

test('empty snapshot watermark must not reject a delayed delta for an earlier observed key', () => harness(async ({ storage, requests, send, errors, live }) => {
  requests[0].respond(page(10, [deleted(1), change(1, 'old')], entry(10, 1), true));
  await until(() => requests.length === 2);
  send({ type: 'delta', serverRevision: 11, data: [row(1, 'new')] });
  requests[1].respond(page(12, [], entry(10, 1)));
  await until(() => errors.length > 0 || live());
  expect(errors).toEqual([]);
  expect((await storage.getAllRows('notes'))[0].body).toBe('new');
  expect(await storage.getServerRevision()).toBe(12);
}));

test('wake during scan rereads the earlier bucket, even after the final bucket advances', () => harness(async ({ storage, requests, send, live }) => {
  requests[0].respond(page(10, [change(1, 'old', 10)], entry(10, 1, 0), true));
  await until(() => requests.length === 2);
  send({ type: 'syncRequired', serverRevision: 11 });
  requests[1].respond(page(12, [change(2, 'later', 11)], entry(11, 2, 0), false, 11));
  await until(() => requests.length === 3);
  const start = requests[2].request.syncCursor.tables.notes?.last_seen_updated_at;
  expect(start == null || start <= 10).toBe(true);
  requests[2].respond(page(12, [change(1, 'new', 10), change(2, 'later', 11)], entry(11, 2, 0), false, 11));
  await until(live);
  expect((await storage.getAllRows('notes')).find(row => row.id === 1).body).toBe('new');
}, false));

test('overflow full scan replays consumed deletions and repairs an existing ghost', () => harness(async ({ storage, requests, send, live }) => {
  requests[0].respond(page(10, [deleted(99), change(2, 'current')], entry(10, 2, 1), true));
  await until(() => requests.length === 2);
  send({ type: 'delta', serverRevision: 11, data: [{ table_name: 'notes', headers: ['id'], rows: Array.from({ length: 5001 }, () => [99]) }] });
  requests[1].respond(page(10, [deleted(98)], entry(10, 2, 2), true));
  await until(() => requests.length === 3);
  expect(requests[2].request.syncCursor.tables.notes.last_seen_updated_at).toBeNull();
  expect(requests[2].request.syncCursor.tables.notes.last_seen_delete_sequence).toBe(0);
  expect((await storage.getAllRows('notes')).some(row => row.id === 1)).toBe(true);
  requests[2].respond(page(11, [deleted(1), change(2, 'current')]));
  await until(live);
  expect(await storage.getAllRows('notes')).toEqual([{ id: 2, body: 'current', updatedAt: 10 }]);
}, true, [row(1, 'persisted ghost')]));

test('pending wakes cannot starve buffered deltas and disjoint revisions remain deliverable', () => harness(async ({ storage, requests, send }) => {
  for (let round = 0; round < 4; round++) {
    await until(() => requests.length === round + 1);
    send({ type: 'syncRequired' });
    send({ type: 'delta', serverRevision: 11 + round * 2, data: [row(3, `live ${round}`)] });
    requests[round].respond(page(12 + round * 2, [change(2, 'snapshot')]));
    await until(() => requests.length === round + 2);
    expect((await storage.getAllRows('notes')).find(row => row.id === 3)?.body).toBe(`live ${round}`);
  }
  requests[4].respond(page(20, [change(2, 'snapshot')]));
}));

test('persisted checkpoint stays before same-second changes that can arrive after a page advances', () => harness(async ({ storage, requests, live }) => {
  requests[0].respond(page(10, [change(1, 'old', 10)], entry(10, 1, 0), true, 10));
  await until(() => requests.length === 2);
  requests[1].respond(page(12, [change(2, 'later', 11)], entry(11, 2, 0), false, 11));
  await until(live);
  expect((await storage.getSyncCursor()).tables.notes.last_seen_updated_at).toBe(10);
  expect((await storage.getSyncCursor()).tables.notes.last_seen_primary_key).toBeNull();
}, false));

test('delayed old row outside the persisted interval cannot overwrite a cached key after restart', () => harness(async ({ storage, requests, send, live }) => {
  requests[0].respond(page(10, [change(2, 'current')]));
  await until(live);
  send({ type: 'delta', serverRevision: 4, data: [row(1, 'stale', 5)] });
  await Bun.sleep(30);
  expect((await storage.getAllRows('notes')).find(row => row.id === 1).body).toBe('cached newer');
  expect(requests).toHaveLength(1);
}, true, [row(1, 'cached newer', 5)]));

test('unfenced version-3 checkpoints are invalidated once; version-4 restarts preserve safe state', async () => {
  const name = `unfenced-${crypto.randomUUID()}`;
  await new Promise<void>((resolve, reject) => {
    const request = indexedDB.open(name, 3);
    request.onupgradeneeded = () => {
      const rows = request.result.createObjectStore('tables', { keyPath: ['tableName', 'id'] });
      rows.createIndex('byTable', 'tableName');
      rows.createIndex('byUpdatedAt', 'updatedAt');
      request.result.createObjectStore('syncCursor');
      request.result.createObjectStore('meta');
    };
    request.onerror = () => reject(request.error);
    request.onsuccess = () => {
      const db = request.result;
      const tx = db.transaction(['tables', 'syncCursor', 'meta'], 'readwrite');
      tx.objectStore('tables').put({ tableName: 'notes', id: 1, body: 'missed same-second update', updatedAt: 10 });
      tx.objectStore('syncCursor').put({ tables: { notes: entry(11) } }, 'cursor');
      tx.objectStore('meta').put('epoch', 'databaseEpoch');
      tx.oncomplete = () => { db.close(); resolve(); };
      tx.onerror = () => reject(tx.error);
    };
  });
  const storage = new IndexedDBStorage(name);
  try {
    const db = await storage.init();
    expect(db.version).toBe(4);
    expect(await storage.getAllRows('notes')).toEqual([]);
    expect(await storage.getSyncCursor()).toEqual({ tables: {} });
    await storage.putCatchupPage('epoch', 12, { tables: { notes: entry(10) } }, []);
    const restarted = new IndexedDBStorage(name);
    expect(await restarted.getSyncCursor()).toEqual({ tables: { notes: entry(10) } });
    (await restarted.init()).close();
  } finally { await storage.deleteDatabase(); }
});
