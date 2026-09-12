// @ts-nocheck
// Browser entry bundled against the freshly built client, not test doubles.
import { PyreClient } from 'fixture:client';
import * as g from 'fixture:typescript/edits/Main.ts';
import { schemaMetadataByNamespace } from 'fixture:typescript/core/schema.ts';
import { meta } from 'fixture:typescript/core/queries/metadata/readIssues.ts';

export async function verify(fence) {
  const checked = [], failures = [], entities = new Map(), queries = [];
  const assert = (value, label) => { if (!value) throw new Error(label); };
  const equal = (a, b, label) => assert(JSON.stringify(a) === JSON.stringify(b), `${label}: ${JSON.stringify({ a, b })}`);
  const until = async (check, label) => {
    for (let n = 0; n < 500; n++) { if (await check()) return; await new Promise(resolve => setTimeout(resolve, 10)); }
    throw new Error(`Timed out: ${label}`);
  };
  const post = async (path, body, signal) => {
    const response = await fetch(path, { method: 'POST', body: JSON.stringify(body), signal });
    if (!response.ok) throw new Error(await response.text());
    return response.json();
  };
  let writes = 0, reads = 0, hintCount = 0, closed = 0, release;
  let hold = false;
  const config = () => ({ schema: schemaMetadataByNamespace._default, cacheNamespace: 'native', indexedDbName: 'edits', server: { baseUrl: '', localEdits: () => ({
    fence, operations: g.operations, minimumSafeRevision: 0,
    prepare: async request => {
      if (hold) await new Promise(resolve => { release = resolve; });
      return { dispatch: signal => { writes++; return post('/batch', request, signal); } };
    },
    replacement: (request, signal) => { reads++; return post('/replacement', request, signal); },
    subscribeHints: receive => {
      const events = new EventSource('/events');
      events.onmessage = event => { hintCount++; receive(JSON.parse(event.data)); };
      return () => { events.close(); closed++; };
    },
  }) } });
  let client = await PyreClient.create(config());
  const cacheName = client.getInternalIndexedDbName('one');
  // Inspect native stores directly, independently of the storage service.
  const cache = () => new Promise((resolve, reject) => {
    const request = indexedDB.open(cacheName);
    request.onerror = () => reject(request.error);
    request.onsuccess = () => {
      const db = request.result, result = {};
      const tx = db.transaction([...db.objectStoreNames]);
      for (const name of db.objectStoreNames) tx.objectStore(name).getAll().onsuccess = event => { result[name] = event.target.result; };
      tx.oncomplete = () => { db.close(); resolve(result); };
      tx.onerror = () => { db.close(); reject(tx.error); };
    };
  });
  const projected = () => [...entities.values()].map(({ id, title }) => ({ id, title })).sort((a, b) => a.id.localeCompare(b.id));
  const agree = () => equal(queries.at(-1)?.issue, projected(), 'query/entity agreement');
  let runtime;
  try {
    await client.onEntityChanges('one', { tables: [{ tableName: 'issues' }] }, batch => {
      for (const change of batch.changes) { if (change.op === 'remove') entities.delete(change.id); else entities.set(change.id, change.row); }
    });
    await client.run('one', { ...meta, operation: 'query' }, {}, result => { queries.push(result); });
    await until(() => reads > 0 && queries.at(-1)?.issue?.length === 0, 'read-only startup');
    equal(writes, 0, 'startup must not write');
    checked.push('read-only catchup');
    runtime = await client.localEdits('one');
    const database = await client.localEdits('one', g.Main);
    database.onEditFailure(event => failures.push(event));
    const id = '00000000-0000-4000-8000-000000000001';
    const submit = async plan => { const result = await database.submit(plan).confirmed; equal(result.kind, 'confirmed', 'receipt'); agree(); return result; };
    await submit(g.Records.Issue.create({ id, title: 'original', owner: 'me' }));
    await until(async () => JSON.stringify(await cache()).includes('original'), 'authoritative persistence');
    const beforePending = await cache();
    hold = true;
    const pending = database.submit(g.Records.Issue.update(id, { title: 'pending-only' }));
    await until(() => release, 'pending update preparation');
    // Permission-dependent Issue edits deliberately have no safe prediction.
    equal(queries.at(-1).issue, [{ id, title: 'original' }], 'unsafe prediction withheld');
    agree();
    equal(await cache(), beforePending, 'pending must not enter any IndexedDB store');
    hold = false; release();
    equal((await pending.confirmed).kind, 'confirmed', 'pending confirmation');
    checked.push('pending not persisted');

    const app = window.Elm.Test.init();
    const observed = [], completed = [];
    app.ports.observed.subscribe(value => observed.push(value));
    app.ports.completed.subscribe(value => completed.push(value));
    client.attachElmBridge({ app, receivePort: 'effectOut', queryResultPort: 'incoming' });
    await until(() => observed.length === 1, 'generated Elm bridge');
    assert(observed[0].auditId === 1 && observed[0].timestamps[0] > 0, 'Elm authoritative result decoding');
    equal(queries.at(-1).issue, [{ id, title: 'elm' }], 'Elm update'); agree();
    // Public Audit.delete does have a generated safe prediction. Observe it
    // through the entity reader while native storage retains the real row.
    const audits = new Map();
    await client.onEntityChanges('one', { tables: [{ tableName: 'audits' }] }, batch => {
      for (const change of batch.changes) { if (change.op === 'remove') audits.delete(change.id); else audits.set(change.id, change.row); }
    });
    assert(audits.has(1), 'authoritative audit visible before optimistic deletion');
    await until(async () => JSON.stringify(await cache()).includes('"audit"'), 'audit persisted');
    const beforeOptimistic = await cache();
    hold = true; release = undefined;
    const deletion = database.submit(g.Records.Audit.delete(1));
    await until(() => release && !audits.has(1), 'safe optimistic delete');
    equal(await cache(), beforeOptimistic, 'optimistic deletion must not persist');
    hold = false; release();
    equal((await deletion.confirmed).kind, 'confirmed', 'optimistic delete confirmed');
    await until(async () => !(await cache()).tables.some(row => row.tableName === 'audits' && row.identity === 1), 'authoritative audit deletion persisted');
    for (const action of ['create', 'nullable', 'delete', 'rollback']) {
      app.ports.perform.send(action);
      await until(() => completed.at(-1)?.action === action, `Elm ${action}`);
      equal(completed.at(-1).state, action === 'rollback' ? 'rejected' : 'confirmed', `Elm ${action} outcome`);
      agree();
    }
    equal(projected(), [{ id, title: 'elm' }], 'no Elm rollback ghost');
    await submit(g.Records.Issue.delete(id));
    equal(projected(), [], 'delete removes readers');
    await until(async () => !JSON.stringify((await cache()).tables).includes('"issues"'), 'persisted issue deletion');
    checked.push('TS/Elm reader agreement');

    const ghost = crypto.randomUUID();
    const beforeFailure = failures.length;
    database.submit(g.batch([g.Records.Issue.create({ id: ghost, title: 'ghost', owner: 'me' }), g.Records.Issue.update(crypto.randomUUID(), { title: 'missing' })]));
    await until(() => failures.length > beforeFailure, 'fire-and-forget failure');
    equal(failures.at(-1).certainty, 'rejected', 'failure certainty');
    equal(failures.at(-1).code, 'TargetNotWritable', 'real executor rejection');
    equal(projected(), [], 'no rejected ghost'); agree();
    assert(!JSON.stringify(await cache()).includes(ghost), 'no persisted ghost');
    checked.push('fire-and-forget rollback');

    const beforeHint = reads;
    await post('/hint', {});
    await until(() => hintCount > 0 && reads > beforeHint, 'native EventSource replacement');
    await until(() => queries.at(-1)?.issue?.[0]?.title === 'external', 'hint installs external commit');
    agree();
    await submit(g.Records.Issue.delete('00000000-0000-4000-8000-000000000099'));
    checked.push('EventSource hint');

    hold = true; release = undefined;
    const stale = database.submit(g.Records.Issue.create({ id: ghost, title: 'old-auth', owner: 'me' }));
    await until(() => release, 'prepared old-auth edit');
    const beforeDispose = writes;
    client.disconnect();
    await runtime.ended;
    equal((await stale.confirmed).kind, 'rejected', 'undispatched disposal');
    release(); hold = false;
    equal(closed, 1, 'EventSource cleanup');
    fence = await post('/auth', {});
    client = await PyreClient.create(config());
    runtime = await client.localEdits('one');
    const newQueries = [];
    await client.run('one', { ...meta, operation: 'query' }, {}, result => newQueries.push(result));
    await until(() => newQueries.at(-1)?.issue?.length === 0, 'new auth read-only catchup');
    equal((await database.submit(g.Records.Issue.delete(id)).confirmed).kind, 'rejected', 'old binding fenced');
    equal(writes, beforeDispose, 'no stale or read-only writes');
    assert(!JSON.stringify(await cache()).includes(ghost), 'old auth cannot resurrect cache');
    checked.push('auth/dispose fencing');
    return checked;
  } finally { client.disconnect(); await runtime?.ended; }
}
