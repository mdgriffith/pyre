// @ts-nocheck
import { expect, test } from 'bun:test';
import 'fake-indexeddb/auto';
import loadElm from '../dist/engine.mjs';
import { IndexedDBStorage, IndexedDbService } from './service/indexeddb';

async function until(check: () => boolean | Promise<boolean>) {
  for (let i = 0; i < 200; i++) {
    if (await check()) return;
    await Bun.sleep(5);
  }
  throw new Error('Handoff timed out');
}

test('register before catchup; reconcile buffered deltas per observed key, not the final page watermark', async () => {
  const oldXhr = globalThis.XMLHttpRequest;
  const requests: any[] = [];
  const mutations: any[] = [];
  class Xhr {
    listeners: Record<string, (() => void)[]> = {};
    status = 200; statusText = 'OK'; responseURL = ''; response = ''; responseType = ''; timeout = 0;
    addEventListener(type, fn) { (this.listeners[type] ??= []).push(fn); }
    open(_method, url) { this.responseURL = url; }
    setRequestHeader() {}
    abort() {}
    getAllResponseHeaders() { return ''; }
    send(body) { (this.responseURL.includes('/db/') ? mutations : requests).push({ request: JSON.parse(body), respond: (page) => { this.response = JSON.stringify(page); this.listeners.load.forEach(fn => fn()); } }); }
  }
  globalThis.XMLHttpRequest = Xhr;
  const storage = new IndexedDBStorage(`handoff-${crypto.randomUUID()}`);
  const schema = { tables: { notes: { name: 'notes', links: {}, indices: [{ field: 'id', unique: true, primary: true }] } }, queryFieldToTable: { notes: 'notes' } };
  const Elm = loadElm(Object.create(globalThis));
  const app = Elm.Main.init({ flags: { schema, server: { baseUrl: 'http://test', catchupPath: '/sync', databaseId: 'main' }, liveSync: { transport: 'sse' } } });
  const service = new IndexedDbService(storage);
  service.attachPorts(app);
  let registrationRequested = false;
  let status = '';
  app.ports.sseOut.subscribe(() => { registrationRequested = true; });
  app.ports.syncStateOut.subscribe(state => { status = state.status; });
  const send = (revision, data) => app.ports.receiveSSEMessage.send({ type: 'delta', databaseId: 'main', databaseEpoch: 'epoch', serverRevision: revision, data });
  const row = (id, body) => ({ table_name: 'notes', headers: ['id', 'body', 'updatedAt'], rows: [[id, body, 10]] });
  const page = (revision, id, body, more) => ({ syncVersion: 2, databaseId: 'main', databaseEpoch: 'epoch', serverRevision: revision, tables: { notes: { changes: [{ op: 'row', id, row: { id, body, updatedAt: 10 } }], permission_hash: 'timestamp-v2:test', last_seen_updated_at: 10, last_seen_primary_key: id, last_seen_delete_sequence: 0 } }, has_more: more });
  try {
    await until(() => registrationRequested);
    expect(requests).toHaveLength(0);
    app.ports.receiveSSEMessage.send({ type: 'connected', databaseId: 'main', databaseEpoch: 'epoch', connectionId: 'one' });
    await until(() => requests.length === 1);
    // Both messages arrive before the first page is persisted, including while
    // the client has not learned the database epoch yet.
    send(11, [row(1, 'new one'), row(2, 'stale two')]);
    requests[0].respond(page(10, 1, 'old one', true));
    await until(() => requests.length === 2);
    app.ports.receiveQueryManagerMessage.send({ type: 'sendMutation', requestId: 'mutation', mutationId: 'change', baseUrl: 'http://test/db', input: {} });
    await until(() => mutations.length === 1);
    mutations[0].respond({ databaseEpoch: 'epoch', serverRevision: 11, result: {}, sync: { type: 'delta', databaseId: 'main', databaseEpoch: 'epoch', serverRevision: 11, data: [row(4, 'mutation response')] } });
    requests[1].respond(page(12, 2, 'new two', false));
    await until(async () => status === 'live' && (await storage.getAllRows('notes')).length === 3);
    expect(await storage.getAllRows('notes')).toEqual([{ id: 1, body: 'new one', updatedAt: 10 }, { id: 2, body: 'new two', updatedAt: 10 }, { id: 4, body: 'mutation response', updatedAt: 10 }]);
    expect(requests).toHaveLength(2);

    send(14, [{ table_name: 'notes', headers: ['$delete'], rows: [[1]] }]);
    await until(async () => (await storage.getAllRows('notes')).length === 2 && status === 'live');
    send(13, [row(1, 'late resurrection'), row(3, 'independent key')]);
    await until(async () => (await storage.getAllRows('notes')).some(row => row.id === 3));
    expect((await storage.getAllRows('notes')).map(row => row.id)).toEqual([2, 3, 4]);
    expect(requests).toHaveLength(2);

    app.ports.receiveSSEMessage.send({ type: 'error', error: 'connection interrupted' });
    await until(() => status !== 'live');
    expect(requests).toHaveLength(2);
    app.ports.receiveSSEMessage.send({ type: 'connected', databaseId: 'main', databaseEpoch: 'epoch', connectionId: 'two' });
    await until(() => requests.length === 3);
    expect(requests[2].request.syncCursor.tables.notes.last_seen_primary_key).toBeNull();
    app.ports.receiveSSEMessage.send({ type: 'syncRequired', databaseId: 'main' });
    requests[2].respond(page(15, 2, 'same-second offline update', true));
    await until(async () => (await storage.getAllRows('notes')).find(row => row.id === 2)?.body === 'same-second offline update');
    await until(() => requests.length === 4);
    requests[3].respond(page(16, 3, 'independent key', false));
    await until(() => requests.length === 5);
    expect(requests[4].request.syncCursor.tables.notes.last_seen_primary_key).toBeNull();
    requests[4].respond(page(16, 3, 'independent key', false));
    await until(() => status === 'live');

    app.ports.receiveSSEMessage.send({ type: 'syncRequired', databaseId: 'main' });
    await until(() => requests.length === 6);
    send(17, [{ table_name: 'notes', headers: ['id', 'body', 'updatedAt'], rows: Array.from({ length: 5001 }, () => [99, 'must rescan', 10]) }]);
    requests[5].respond(page(16, 3, 'independent key', false));
    await until(() => requests.length === 7);
    expect(requests[6].request.syncCursor.tables.notes.last_seen_updated_at).toBeNull();
    expect(requests[6].request.syncCursor.tables.notes.last_seen_delete_sequence).toBe(0);
    requests[6].respond(page(17, 99, 'from full rescan', false));
    await until(async () => status === 'live' && (await storage.getAllRows('notes')).some(row => row.id === 99 && row.body === 'from full rescan'));

    app.ports.receiveSSEMessage.send({ type: 'syncRequired', databaseId: 'main' });
    await until(() => requests.length === 8);
    requests[7].respond({ syncVersion: 1 });
    await Bun.sleep(20);
    send(18, [row(100, 'buffered during retry')]);
    await until(() => requests.length === 9);
    expect((await storage.getAllRows('notes')).some(row => row.id === 100)).toBe(false);
    requests[8].respond(page(18, 100, 'from catchup after failure', false));
    await until(async () => status === 'live' && (await storage.getAllRows('notes')).some(row => row.id === 100 && row.body === 'from catchup after failure'));

    // An empty later page still advances storage's watermark, but must not
    // reject a buffered update newer than this key's own observation.
    app.ports.receiveSSEMessage.send({ type: 'syncRequired', databaseId: 'main' });
    await until(() => requests.length === 10);
    requests[9].respond(page(18, 100, 'from catchup after failure', true));
    await until(() => requests.length === 11);
    send(19, [row(100, 'updated after observation')]);
    const emptyPage = page(20, 100, 'unused', false);
    emptyPage.tables.notes.changes = [];
    requests[10].respond(emptyPage);
    await until(async () => status === 'live' && (await storage.getAllRows('notes')).some(row => row.id === 100 && row.body === 'updated after observation'));
    expect(await storage.getServerRevision()).toBe(20);
    expect(requests).toHaveLength(11);
  } finally {
    globalThis.XMLHttpRequest = oldXhr;
    await storage.deleteDatabase();
  }
});
