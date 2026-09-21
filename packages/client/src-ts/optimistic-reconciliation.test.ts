// @ts-nocheck
import { expect, test } from 'bun:test';
import loadElm from '../dist/engine.mjs';
import { EntityStreamService } from './service/entity-stream';

const turn = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
const schema = {
  tables: { notes: { name: 'notes', links: {}, indices: [] } },
  queryFieldToTable: { notes: 'notes' },
};
const initial = [{ id: 1, title: 'Initial', other: 'Initial' }, { id: 2, title: 'Second', other: 'Initial' }];
const groups = (rows) => [{ table_name: 'notes', headers: ['id', 'title', 'other'], rows }];
const envelope = (revision, rows) => ({
  serverRevision: revision,
  result: { ok: true },
  sync: { type: 'delta', databaseId: 'test', databaseEpoch: 'epoch-1', serverRevision: revision, data: groups(rows) },
});

async function harness(run) {
  const original = globalThis.XMLHttpRequest;
  const requests = [];
  let catchupResponse = { databaseId: 'test', databaseEpoch: 'epoch-1', tables: {}, has_more: false };
  class Xhr {
    listeners = {};
    status = 200;
    statusText = 'OK';
    response = '';
    responseURL = '';
    addEventListener(type, callback) { (this.listeners[type] ??= []).push(callback); }
    open(_method, url) { this.responseURL = url; }
    setRequestHeader() {}
    getAllResponseHeaders() { return ''; }
    abort() {}
    send(body) {
      const complete = (response, status = 200) => {
        this.status = status;
        this.response = JSON.stringify(response);
        (this.listeners.load ?? []).forEach((callback) => callback());
      };
      if (this.responseURL.endsWith('/sync')) {
        queueMicrotask(() => complete(catchupResponse));
      } else {
        requests.push({ input: JSON.parse(body), complete });
      }
    }
  }
  globalThis.XMLHttpRequest = Xhr;
  async function client(restored = {}) {
    const Elm = loadElm(Object.create(globalThis));
    const app = Elm.Main.init({ flags: {
      schema, server: { baseUrl: 'http://test', catchupPath: '/sync', databaseId: 'test' },
      liveSync: { transport: 'sse' }, sync: { autoStart: false },
    } });
    const visible = [];
    const writes = [];
    const results = [];
    const queryResults = [];
    const entities = new EntityStreamService();
    app.ports.visibleStateOut.subscribe((message) => {
      visible.push(message);
      entities.handleVisibleState(message.snapshot, message.source, 'test');
    });
    app.ports.indexedDbOut.subscribe((message) => writes.push(message));
    app.ports.queryManagerOut.subscribe((message) => results.push(message));
    app.ports.queryClientOut.subscribe((message) => queryResults.push(message));
    app.ports.receiveIndexedDbMessage.send({ type: 'initialData', data: {
      tables: { notes: initial }, cursor: { tables: {} }, databaseEpoch: 'epoch-1', lastAppliedServerRevision: null,
      ...restored,
    } });
    await turn();
    await turn();
    return {
      app, visible, writes, results, entities,
      async edit(requestId, title, id = 1) {
        app.ports.receiveQueryManagerMessage.send({
          type: 'sendMutation', requestId, mutationId: 'update', baseUrl: 'http://test/db',
          input: { id, title }, optimistic: {
            queryField: 'notes', where: { field: 'id', input: 'id' }, set: [{ field: 'title', input: 'title' }],
          },
        });
        await turn();
      },
      async live(revision, rows) {
        app.ports.receiveSSEMessage.send(envelope(revision, rows).sync);
        await turn();
      },
      async rows() {
        app.ports.receiveQueryClientMessage.send({ type: 'register', queryId: 'notes',
          querySource: { notes: { id: true, title: true, other: true } }, queryInput: {} });
        await turn();
        return queryResults.at(-1).result.notes;
      },
    };
  }
  try { await run({ client, requests, setCatchupResponse: (response) => { catchupResponse = response; } }); } finally { globalThis.XMLHttpRequest = original; }
}

test('two clients share incremental authority; origin confirms without live delivery', async () => {
  await harness(async ({ client, requests }) => {
    const a = await client();
    const b = await client();
    await a.edit('a', 'Predicted');
    expect((await a.rows())[0].title).toBe('Predicted');
    expect((await b.rows())[0].title).toBe('Initial');
    expect(a.writes.filter((m) => m.type === 'writeDelta').flatMap((m) => m.tableGroups).flatMap((g) => g.rows)).toEqual([]);
    requests[0].complete(envelope(1, [[1, 'Normalized', 'Server']]));
    await turn();
    await b.live(1, [[1, 'Normalized', 'Server']]);
    expect(await a.rows()).toEqual(await b.rows());
    expect((await a.rows())[0]).toEqual({ id: 1, title: 'Normalized', other: 'Server' });
    expect(a.visible.at(-1).data).toEqual(groups([[1, 'Server', 'Normalized']]).map((g) => ({ ...g, headers: ['id', 'other', 'title'] })));
    expect(requests).toHaveLength(1);
  });
});

test('reload restores per-row revisions independently and keeps the invalidation floor', async () => {
  await harness(async ({ client }) => {
    const a = await client({ lastAppliedServerRevision: 3, rowRevisions: [['notes', 1, 1], ['notes', 2, 3]] });
    await a.live(2, [[1, 'Delayed valid', 'Server'], [2, 'Stale', 'Server']]);
    expect((await a.rows()).map((row) => row.title)).toEqual(['Delayed valid', 'Second']);
    const reset = await client({ tables: { notes: [initial[1]] }, lastAppliedServerRevision: 3, revisionFloor: 3, rowRevisions: [['notes', 2, 3]] });
    reset.app.ports.receiveSSEMessage.send({ type: 'invalidate', databaseId: 'test', databaseEpoch: 'epoch-1', serverRevision: 1 });
    await turn();
    await reset.live(2, [[1, 'Must not return', 'Secret']]);
    expect((await reset.rows()).map((row) => row.id)).toEqual([2]);
  });
});

test('engine snapshots include initial and late optimistic readers, rollback and reset removals', async () => {
  await harness(async ({ client, requests }) => {
    const a = await client();
    const subscription = { tables: [{ tableName: 'notes' }] };
    expect(a.entities.createBatchFromRows(subscription, a.entities.snapshot(), 'indexeddb-initial').changes).toHaveLength(2);
    await a.edit('pending', 'Pending');
    expect(a.entities.createBatchFromRows(subscription, a.entities.snapshot(), 'indexeddb-initial').changes[0].row.title).toBe('Pending');
    const batches = [];
    a.entities.subscribe(subscription, (batch) => batches.push(batch));
    requests[0].complete({}, 403);
    await turn();
    expect(batches.at(-1).changes[0].row.title).toBe('Initial');
    a.app.ports.receiveSSEMessage.send({ type: 'invalidate', databaseId: 'test', databaseEpoch: 'epoch-1', serverRevision: 3 });
    await turn();
    expect(batches.at(-1).changes.map((change) => change.op)).toEqual(['remove', 'remove']);
    expect(a.entities.snapshot().size).toBe(0);
    await a.live(4, [[1, 'During reset', 'Secret']]);
    expect(await a.rows()).toEqual([]);
  });
});

test('catchup cannot replace a newer live row or persist a stale version', async () => {
  await harness(async ({ client, setCatchupResponse }) => {
    const a = await client();
    await a.live(5, [[1, 'New', 'New']]);
    setCatchupResponse({ databaseId: 'test', databaseEpoch: 'epoch-1', serverRevision: 4, has_more: false,
      tables: { notes: { rows: [{ id: 1, title: 'Old', other: 'Old' }], permission_hash: '', last_seen_updated_at: null } } });
    const before = a.writes.length;
    a.app.ports.receiveSSEMessage.send({ type: 'syncRequired', databaseId: 'test', databaseEpoch: 'epoch-1', serverRevision: 6 });
    await turn();
    await turn();
    expect((await a.rows())[0].title).toBe('New');
    expect(a.writes.slice(before).filter((m) => m.type === 'writeDelta').flatMap((m) => m.tableGroups).flatMap((g) => g.rows)).toEqual([]);
    expect(a.writes.filter((m) => m.type === 'writeServerRevision').at(-1).serverRevision).toBe(5);
  });
});

test('rejection preserves later intent and unrelated authoritative fields', async () => {
  await harness(async ({ client, requests }) => {
    const a = await client();
    await a.edit('a', 'A');
    await a.edit('b', 'B');
    await a.live(1, [[1, 'Remote', 'Changed remotely']]);
    expect((await a.rows())[0]).toEqual({ id: 1, title: 'B', other: 'Changed remotely' });
    requests[0].complete({}, 403);
    await turn();
    expect((await a.rows())[0]).toEqual({ id: 1, title: 'B', other: 'Changed remotely' });
    requests[1].complete({}, 403);
    await turn();
    expect((await a.rows())[0]).toEqual({ id: 1, title: 'Remote', other: 'Changed remotely' });
    expect(a.visible.at(-1).data[0].rows).toEqual([[1, 'Changed remotely', 'Remote']]);
  });
});

test('out-of-order incremental responses retain distinct rows and server normalization', async () => {
  await harness(async ({ client, requests }) => {
    const a = await client();
    await a.edit('a', 'A', 1);
    await a.edit('b', 'B', 2);
    requests[1].complete(envelope(2, [[2, 'Normalized B', 'Server B']]));
    await turn();
    requests[0].complete(envelope(1, [[1, 'Normalized A', 'Server A']]));
    await turn();
    expect(await a.rows()).toEqual([
      { id: 1, title: 'Normalized A', other: 'Server A' },
      { id: 2, title: 'Normalized B', other: 'Server B' },
    ]);
    await a.live(1, [[2, 'Stale', 'Stale']]);
    expect((await a.rows())[1].title).toBe('Normalized B');
  });
});

test('later acknowledgement shields normalization until earlier rejection settles', async () => {
  await harness(async ({ client, requests }) => {
    const a = await client();
    await a.edit('a', 'A');
    await a.edit('b', 'B');
    requests[1].complete(envelope(2, [[1, 'Normalized B', 'Server']]));
    await turn();
    expect((await a.rows())[0].title).toBe('Normalized B');
    requests[0].complete({}, 403);
    await turn();
    expect((await a.rows())[0].title).toBe('Normalized B');
  });
});

test('a held pre-reset response cannot restore query state or issue persistence writes', async () => {
  await harness(async ({ client, requests, setCatchupResponse }) => {
    const a = await client();
    await a.edit('held', 'Pending');
    setCatchupResponse({ type: 'reset', databaseId: 'test', databaseEpoch: 'epoch-2' });
    a.app.ports.receiveSSEMessage.send({ type: 'syncRequired', databaseId: 'test', databaseEpoch: 'epoch-2', serverRevision: 1 });
    await turn();
    await turn();
    expect(await a.rows()).toEqual([]);
    const writesBefore = a.writes.length;
    requests[0].complete(envelope(99, [[1, 'Must not return', 'Secret']]));
    await turn();
    expect(await a.rows()).toEqual([]);
    expect(a.writes.slice(writesBefore)).toEqual([]);
    expect(a.results.at(-1).result).toBeDefined();
  });
});
