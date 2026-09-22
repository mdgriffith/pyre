// @ts-nocheck
import { expect, test } from 'bun:test';
import loadElm from '../dist/engine.mjs';
import { EntityStreamService } from './service/entity-stream';

const turn = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
const one = '00000000-0000-7000-8000-000000000001';
const two = '00000000-0000-7000-8000-000000000002';
const schema = {
  tables: { notes: { name: 'notes', links: {}, indices: [] } },
  queryFieldToTable: { notes: 'notes' },
};
const initial = [{ id: one, title: 'Initial', other: 'Initial' }, { id: two, title: 'Second', other: 'Initial' }];
const groups = (rows) => [{ table_name: 'notes', headers: ['id', 'title', 'other'], rows }];
const envelope = (revision, rows) => ({
  serverRevision: revision,
  result: { ok: true },
  sync: { type: 'delta', databaseId: 'test', databaseEpoch: 'epoch-1', serverRevision: revision, data: groups(rows) },
});

async function harness(run, key = 'id') {
  const clientSchema = { ...schema, tables: { notes: { ...schema.tables.notes, indices: [{ field: key, primary: true, unique: true }] } } };
  const initialRows = initial.map(({ id, ...row }) => ({ ...row, [key]: id, ...(key === 'id' ? {} : { id: 'ordinary' }) }));
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
      schema: clientSchema, server: { baseUrl: 'http://test', catchupPath: '/sync', databaseId: 'test' },
      liveSync: { transport: 'sse' }, sync: { autoStart: false },
    } });
    const visible = [];
    const writes = [];
    const results = [];
    const queryResults = [];
    const entities = new EntityStreamService({ notes: key });
    app.ports.visibleStateOut.subscribe((message) => {
      visible.push(message);
      entities.handleVisibleState(message.snapshot, message.source, 'test');
    });
    app.ports.indexedDbOut.subscribe((message) => writes.push(message));
    app.ports.queryManagerOut.subscribe((message) => results.push(message));
    app.ports.queryClientOut.subscribe((message) => queryResults.push(message));
    app.ports.receiveIndexedDbMessage.send({ type: 'initialData', data: {
      tables: { notes: initialRows }, cursor: { tables: {} }, databaseEpoch: 'epoch-1', lastAppliedServerRevision: null,
      ...restored,
    } });
    await turn();
    await turn();
    return {
      app, visible, writes, results, entities,
      async crud(requestId, kind, input) {
        app.ports.receiveQueryManagerMessage.send({
          type: 'sendMutation', requestId, mutationId: kind, baseUrl: 'http://test/db', input,
           optimistic: { queryField: 'notes', kind, where: { field: key, input: key },
             set: kind === 'delete' ? [] : [...new Set([key, 'id', 'title', 'other'])].map(field => ({ field, input: field })) },
        });
        await turn();
      },
      async batch(requestId, operations) {
        app.ports.receiveQueryManagerMessage.send({
          type: 'sendMutation', requestId, mutationId: '$batch', baseUrl: 'http://test/db',
          input: operations.map(({ input }) => ({ queryId: 'update', input })),
          optimistic: operations.map(({ input, where = 'id' }) => ({ input, optimistic: {
            queryField: 'notes', where: { field: where, input: where }, set: [{ field: 'title', input: 'title' }],
          } })),
        });
        await turn();
      },
      async edit(requestId, title, id = one) {
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
           querySource: { notes: { [key]: true, id: true, title: true, other: true } }, queryInput: {} });
        await turn();
        return queryResults.at(-1).result.notes;
      },
    };
  }
  try { await run({ client, requests, setCatchupResponse: (response) => { catchupResponse = response; } }); } finally { globalThis.XMLHttpRequest = original; }
}

test('custom-key create/delete, query tracking, entity removals and restored fences ignore ordinary id', async () => {
  await harness(async ({ client, requests }) => {
    const a = await client();
    const events = [];
    a.entities.subscribe({ tables: [{ tableName: 'notes' }] }, event => events.push(event));
    const key = '00000000-0000-7000-8000-000000000003';
    const row = { noteKey: key, id: 'ordinary', title: 'Created', other: 'Value' };
    const response = (revision, removed = false) => ({
      ...envelope(revision, []),
      sync: { ...envelope(revision, []).sync, data: [{ table_name: 'notes',
        headers: removed ? ['noteKey', '_pyre_removed'] : ['noteKey', 'id', 'title', 'other'],
        rows: removed ? [[key, true]] : [[key, 'ordinary', 'Created', 'Value']] }] },
    });
    await a.rows(); // Keep a subscribed query across the create and deletion.
    await a.crud('create', 'create', row);
    expect((await a.rows()).at(-1)).toEqual(row);
    expect(events.at(-1).changes[0].id).toBe(key);
    requests[0].complete(response(1));
    await turn();
    await a.crud('delete-rejected', 'delete', { noteKey: key });
    expect((await a.rows()).map(row => row.noteKey)).toEqual([one, two]);
    expect(events.at(-1).changes).toEqual([{ tableName: 'notes', id: key, op: 'remove', row: { noteKey: key } }]);
    requests[1].complete({}, 403);
    await turn();
    expect((await a.rows()).at(-1)).toEqual(row);
    await a.crud('delete', 'delete', { noteKey: key });
    requests[2].complete(response(3, true));
    await turn();
    const restored = await client({ rowRevisions: [['notes', key, 3]] });
    restored.app.ports.receiveSSEMessage.send(response(2).sync);
    await turn();
    expect((await restored.rows()).map(row => row.noteKey)).toEqual([one, two]);
    expect((await a.rows()).map(row => row.noteKey)).toEqual([one, two]);
  }, 'noteKey');
});

test('two clients share incremental authority; origin confirms without live delivery', async () => {
  await harness(async ({ client, requests }) => {
    const a = await client();
    const b = await client();
    await a.edit('a', 'Predicted');
    expect((await a.rows())[0].title).toBe('Predicted');
    expect((await b.rows())[0].title).toBe('Initial');
    expect(a.writes.filter((m) => m.type === 'writeDelta').flatMap((m) => m.tableGroups).flatMap((g) => g.rows)).toEqual([]);
    requests[0].complete(envelope(1, [[one, 'Normalized', 'Server']]));
    await turn();
    await b.live(1, [[one, 'Normalized', 'Server']]);
    expect(await a.rows()).toEqual(await b.rows());
    expect((await a.rows())[0]).toEqual({ id: one, title: 'Normalized', other: 'Server' });
    expect(a.visible.at(-1).data).toEqual(groups([[one, 'Server', 'Normalized']]).map((g) => ({ ...g, headers: ['id', 'other', 'title'] })));
    expect(requests).toHaveLength(1);
  });
});

test('create/delete replay shares query visibility; removals fence stale upserts and rejected deletes restore rows', async () => {
  await harness(async ({ client, requests }) => {
    const a = await client();
    const b = await client();
    const id = '00000000-0000-7000-8000-000000000003';
    await a.crud('create', 'create', { id, title: 'New', other: 'Local' });
    expect((await a.rows()).map(row => row.id)).toEqual([one, two, id]);
    expect((await b.rows()).map(row => row.id)).toEqual([one, two]);
    requests[0].complete(envelope(1, [[id, 'Normalized', 'Server']]));
    await turn();
    await b.live(1, [[id, 'Normalized', 'Server']]);
    await a.crud('reject-delete', 'delete', { id });
    expect((await a.rows()).map(row => row.id)).toEqual([one, two]);
    requests[1].complete({}, 403);
    await turn();
    expect((await a.rows()).at(-1).title).toBe('Normalized');
    await a.crud('delete', 'delete', { id });
    const response = envelope(3, []);
    response.sync.data = [{ table_name: 'notes', headers: ['id', '_pyre_removed'], rows: [[id, true]] }];
    requests[2].complete(response);
    b.app.ports.receiveSSEMessage.send(response.sync);
    await turn();
    await a.live(2, [[id, 'Stale', 'Old']]);
    await b.live(2, [[id, 'Stale', 'Old']]);
    expect(await a.rows()).toEqual(await b.rows());
    expect((await a.rows()).map(row => row.id)).toEqual([one, two]);
    expect(a.writes.filter(m => m.type === 'writeDelta' && m.serverRevision === 3).some(m => m.tableGroups.some(g => g.headers.includes('_pyre_removed')))).toBe(true);
  });
});

test('incomplete creates stay server-only; removal cannot be undone by pending create replay', async () => {
  await harness(async ({ client, requests }) => {
    const a = await client();
    const id = '00000000-0000-7000-8000-000000000003';
    await a.crud('incomplete', 'create', { id, title: 'Missing default' });
    expect((await a.rows()).map(row => row.id)).toEqual([one, two]);
    requests[0].complete({}, 403);
    await turn();
    await a.crud('create', 'create', { id, title: 'New', other: 'Local' });
    const response = envelope(2, []);
    response.sync.data = [{ table_name: 'notes', headers: ['id', '_pyre_removed'], rows: [[id, true]] }];
    a.app.ports.receiveSSEMessage.send(response.sync);
    await turn();
    expect((await a.rows()).map(row => row.id)).toEqual([one, two]);
    requests[1].complete(envelope(1, [[id, 'Created', 'Server']]));
    await turn();
    expect((await a.rows()).map(row => row.id)).toEqual([one, two]);
  });
});

test('batch captures repeated and multiple rows in order and publishes once; rejection preserves later intent', async () => {
  await harness(async ({ client, requests }) => {
    const a = await client();
    const before = a.visible.length;
    await a.batch('batch', [
      { input: { id: one, title: 'Intermediate' } },
      { input: { id: two, title: 'Other' } },
      { input: { id: one, title: 'Final' } },
    ]);
    expect(a.visible.length - before).toBe(1);
    expect((await a.rows()).map(row => row.title)).toEqual(['Final', 'Other']);
    expect(requests).toHaveLength(1);
    expect(requests[0].input.map(op => op.input.title)).toEqual(['Intermediate', 'Other', 'Final']);
    await a.edit('later', 'Later', two);
    const rejecting = a.visible.length;
    requests[0].complete({}, 403);
    await turn();
    expect(a.visible.length - rejecting).toBe(1);
    expect((await a.rows()).map(row => row.title)).toEqual(['Initial', 'Later']);
    requests[1].complete(envelope(2, [[two, 'Normalized later', 'Server']]));
    await turn();
    expect((await a.rows()).map(row => row.title)).toEqual(['Initial', 'Normalized later']);
  });
});

test('later batch confirmation shields every affected field until an earlier batch settles', async () => {
  await harness(async ({ client, requests }) => {
    const a = await client();
    await a.batch('earlier', [{ input: { id: one, title: 'A' } }, { input: { id: two, title: 'B' } }]);
    await a.batch('later', [{ input: { id: one, title: 'C' } }, { input: { id: two, title: 'D' } }]);
    const before = a.visible.length;
    requests[1].complete(envelope(2, [[one, 'Normalized C', 'Server'], [two, 'Normalized D', 'Server']]));
    await turn();
    expect(a.visible.length - before).toBe(1);
    expect((await a.rows()).map(row => row.title)).toEqual(['Normalized C', 'Normalized D']);
    requests[0].complete(envelope(1, [[one, 'Old A', 'Old'], [two, 'Old B', 'Old']]));
    await turn();
    expect((await a.rows()).map(row => row.title)).toEqual(['Normalized C', 'Normalized D']);
  });
});

test('batch selection sees preceding operations before publication', async () => {
  await harness(async ({ client, requests }) => {
    const a = await client();
    // The second operation selects the value written by the first operation.
    a.app.ports.receiveQueryManagerMessage.send({
      type: 'sendMutation', requestId: 'dependent', mutationId: '$batch', baseUrl: 'http://test/db', input: [],
      optimistic: [
        { input: { id: one, title: 'Selected' }, optimistic: { queryField: 'notes', where: { field: 'id', input: 'id' }, set: [{ field: 'title', input: 'title' }] } },
        { input: { match: 'Selected', title: 'Final' }, optimistic: { queryField: 'notes', where: { field: 'title', input: 'match' }, set: [{ field: 'title', input: 'title' }] } },
      ],
    });
    await turn();
    expect((await a.rows())[0].title).toBe('Final');
    requests[0].complete({}, 403);
    await turn();
    expect((await a.rows())[0].title).toBe('Initial');
  });
});

test('invalidation fences a whole held batch from readers and persistence', async () => {
  await harness(async ({ client, requests }) => {
    const a = await client();
    await a.batch('held-batch', [{ input: { id: one, title: 'A' } }, { input: { id: two, title: 'B' } }]);
    a.app.ports.receiveSSEMessage.send({ type: 'invalidate', databaseId: 'test', databaseEpoch: 'epoch-1', serverRevision: 3 });
    await turn();
    const writes = a.writes.length;
    requests[0].complete(envelope(2, [[one, 'Old A', 'Secret'], [two, 'Old B', 'Secret']]));
    await turn();
    expect(await a.rows()).toEqual([]);
    expect(a.writes.length).toBe(writes);
    expect(a.results.at(-1).result).toMatchObject({ ok: false });
    expect(a.entities.snapshot().size).toBe(0);
  });
});

test('reload restores per-row revisions independently and keeps the invalidation floor', async () => {
  await harness(async ({ client }) => {
    const a = await client({ lastAppliedServerRevision: 3, rowRevisions: [['notes', one, 1], ['notes', two, 3]] });
    await a.live(2, [[one, 'Delayed valid', 'Server'], [two, 'Stale', 'Server']]);
    expect((await a.rows()).map((row) => row.title)).toEqual(['Delayed valid', 'Second']);
    const reset = await client({ tables: { notes: [initial[1]] }, lastAppliedServerRevision: 3, revisionFloor: 3, rowRevisions: [['notes', two, 3]] });
    reset.app.ports.receiveSSEMessage.send({ type: 'invalidate', databaseId: 'test', databaseEpoch: 'epoch-1', serverRevision: 1 });
    await turn();
    await reset.live(2, [[one, 'Must not return', 'Secret']]);
    expect((await reset.rows()).map((row) => row.id)).toEqual([two]);
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
    await a.live(4, [[one, 'During reset', 'Secret']]);
    expect(await a.rows()).toEqual([]);
  });
});

test('catchup cannot replace a newer live row or persist a stale version', async () => {
  await harness(async ({ client, setCatchupResponse }) => {
    const a = await client();
    await a.live(5, [[one, 'New', 'New']]);
    setCatchupResponse({ databaseId: 'test', databaseEpoch: 'epoch-1', serverRevision: 4, has_more: false,
      tables: { notes: { rows: [{ id: one, title: 'Old', other: 'Old' }], permission_hash: '', last_seen_updated_at: null } } });
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
    await a.live(1, [[one, 'Remote', 'Changed remotely']]);
    expect((await a.rows())[0]).toEqual({ id: one, title: 'B', other: 'Changed remotely' });
    requests[0].complete({}, 403);
    await turn();
    expect((await a.rows())[0]).toEqual({ id: one, title: 'B', other: 'Changed remotely' });
    requests[1].complete({}, 403);
    await turn();
    expect((await a.rows())[0]).toEqual({ id: one, title: 'Remote', other: 'Changed remotely' });
    expect(a.visible.at(-1).data[0].rows).toEqual([[one, 'Changed remotely', 'Remote']]);
  });
});

test('out-of-order incremental responses retain distinct rows and server normalization', async () => {
  await harness(async ({ client, requests }) => {
    const a = await client();
    await a.edit('a', 'A', one);
    await a.edit('b', 'B', two);
    requests[1].complete(envelope(2, [[two, 'Normalized B', 'Server B']]));
    await turn();
    requests[0].complete(envelope(1, [[one, 'Normalized A', 'Server A']]));
    await turn();
    expect(await a.rows()).toEqual([
      { id: one, title: 'Normalized A', other: 'Server A' },
      { id: two, title: 'Normalized B', other: 'Server B' },
    ]);
    await a.live(1, [[two, 'Stale', 'Stale']]);
    expect((await a.rows())[1].title).toBe('Normalized B');
  });
});

test('later acknowledgement shields normalization until earlier rejection settles', async () => {
  await harness(async ({ client, requests }) => {
    const a = await client();
    await a.edit('a', 'A');
    await a.edit('b', 'B');
    requests[1].complete(envelope(2, [[one, 'Normalized B', 'Server']]));
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
    requests[0].complete(envelope(99, [[one, 'Must not return', 'Secret']]));
    await turn();
    expect(await a.rows()).toEqual([]);
    expect(a.writes.slice(writesBefore)).toEqual([]);
    expect(a.results.at(-1).result).toBeDefined();
  });
});
