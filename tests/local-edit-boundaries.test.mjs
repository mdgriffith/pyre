import './typescript-loader.mjs';
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { createClient } from '@libsql/client';
import { z } from 'zod';

const { PyreClient, LocalEditsRuntime } = await import('../packages/client/src-ts/index.ts');
const { runWithSync } = await import('../packages/server/query-sync.ts');
const { run } = await import('../packages/server/query.ts');
const { ensureDatabase, loadSchemaFromDatabase } = await import('../packages/server/schema.ts');
const { default: initWasm } = await import('../packages/server/wasm/pyre_wasm.js');
await initWasm({ module_or_path: readFileSync(new URL('../packages/server/wasm/pyre_wasm_bg.wasm', import.meta.url)) });

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const turn = () => new Promise(resolve => setImmediate(resolve));
const fence = { databaseId: 'main', instance: 'tab', authGeneration: 1, namespace: 'Main', manifest: 'm1', databaseEpoch: 'e1' };

test('final persistence failure remains observable without delaying lifetime completion', async () => {
  const persistence = deferred();
  const failures = [];
  const runtime = new LocalEditsRuntime({
    fence, minimumSafeRevision: 0, operations: [],
    prepare: async () => { throw Error('unused'); }, replacement: async () => {},
  }, { send() {}, install: () => () => {}, persist: () => persistence.promise });
  runtime.onEditFailure(event => failures.push(event));
  const unsubscribedFailures = [];
  const unsubscribe = runtime.onEditFailure(event => unsubscribedFailures.push(event));
  let ended = false;
  runtime.ended.then(() => { ended = true; });
  runtime.receiveEnvelope({ events: [{ ...fence, type: 'lifetimeEnded' }], queries: [] });
  await turn();
  assert.equal(ended, true, 'lifetime completion must not wait for storage');
  unsubscribe();
  persistence.reject(Error('disk unavailable'));
  await turn();
  assert.equal(failures.length, 1);
  assert.equal(failures[0].code, 'PersistenceFailed');
  assert.equal(failures[0].certainty, undefined);
  assert.deepEqual(unsubscribedFailures, []);
});

function internal(ended = deferred()) {
  const callbacks = [];
  return {
    end: ended.resolve, callbacks,
    getLocalEdits: () => ({ ended: ended.promise, dispose() {} }),
    disconnect() {}, startSync() {}, onDevtoolsEvent: () => () => {},
    onSyncState(callback) { callback({ status: 'not_started', tables: {} }); },
    run(_db, _query, _input, callback) {
      callbacks.push(callback);
      return { update() {}, unsubscribe() {} };
    },
    async onEntityChanges(_subscription, callback) { callbacks.push(callback); return () => {}; },
  };
}
const config = { schema: { tables: {}, queryFieldToTable: {} }, cacheNamespace: 'boundary-test',
  server: { baseUrl: 'https://unused.invalid', localEdits: () => ({}) } };

for (const kind of ['query', 'entity']) test(`${kind} registration cannot attach a retired client under the new generation`, async () => {
  const first = internal();
  const second = internal();
  const creation = deferred();
  let attempts = 0;
  const client = await PyreClient.create({ ...config,
    createInternalClient: () => ++attempts === 1 ? creation.promise : Promise.resolve(second),
  });
  const results = [];
  const registration = kind === 'query'
    ? client.run('main', { operation: 'query', queryShape: {} }, {}, value => results.push(value))
    : client.onEntityChanges('main', { tables: [{ tableName: 'notes' }] }, value => results.push(value));
  // Retirement is already observable when the original asynchronous creation completes.
  first.end();
  creation.resolve(first);
  await registration;
  await turn();
  first.callbacks.forEach(callback => callback('stale'));
  second.callbacks.forEach(callback => callback('current'));
  assert.deepEqual(results, ['current']);
  client.disconnect();
});

for (const kind of ['query', 'entity']) test(`${kind} initialization failure leaves no orphan subscription`, async () => {
  const current = internal();
  let attempts = 0;
  const client = await PyreClient.create({ ...config, createInternalClient: async () => {
    if (++attempts === 1) throw Error('initialization failed');
    return attempts === 2 ? current : internal();
  } });
  const register = () => kind === 'query'
    ? client.run('main', { operation: 'query', queryShape: {} }, {}, () => {})
    : client.onEntityChanges('main', { tables: [{ tableName: 'notes' }] }, () => {});
  await assert.rejects(register(), /initialization failed/);
  await client.getOrCreateClient('main');
  assert.equal(current.callbacks.length, 0);
  current.end();
  await turn();
  assert.equal(attempts, 2, 'a rejected registration must not keep automatic recreation alive');
  client.disconnect();
});

test('disconnect during registration does not recreate or bind a client', async () => {
  const current = internal();
  const creation = deferred();
  let attempts = 0;
  const client = await PyreClient.create({ ...config, createInternalClient: () => {
    attempts++; return creation.promise;
  } });
  const pending = client.run('main', { operation: 'query', queryShape: {} }, {}, () => {});
  client.disconnect();
  creation.resolve(current);
  await pending;
  assert.equal(attempts, 1);
  assert.equal(current.callbacks.length, 0);
});

test('failed automatic recreation reports the error and permits explicit recovery', async () => {
  const first = internal();
  const second = internal();
  const errors = [];
  let attempts = 0;
  const client = await PyreClient.create({ ...config, onError: error => errors.push(error),
    createInternalClient: async () => {
      attempts++;
      if (attempts === 2) throw Error('bootstrap unavailable');
      return attempts === 1 ? first : second;
    },
  });
  const results = [];
  await client.setSyncedDatabases(['main']);
  await client.run('main', { operation: 'query', queryShape: {} }, {}, value => results.push(value));
  first.end();
  await turn();
  assert.equal(errors.length, 1);
  assert.equal(errors[0].message, 'bootstrap unavailable');
  await client.syncDatabase('main');
  second.callbacks.forEach(callback => callback('recovered'));
  assert.deepEqual(results, ['recovered']);
  client.disconnect();
});

test('schema refresh failure after commit preserves revision and requests catchup', async () => {
  const directory = mkdtempSync(new URL('../target/publication-boundary-', import.meta.url));
  const db = createClient({ url: `file:${directory}/test.db` });
  try {
    await ensureDatabase(db, 'Main', 'record Note {\n    @public\n    id Id.Uuid @id\n    body String\n}\n');
    await loadSchemaFromDatabase(directory, db);
    const transaction = db.transaction.bind(db);
    db.transaction = async mode => {
      const tx = await transaction(mode);
      const commit = tx.commit.bind(tx);
      tx.commit = async () => {
        await commit();
        // A concurrent refresh invalidates evidence before its asynchronous read fails.
        const execute = db.execute;
        db.execute = async () => { throw Error('refresh unavailable'); };
        try { await assert.rejects(loadSchemaFromDatabase(directory, db)); }
        finally { db.execute = execute; }
      };
      return tx;
    };
    const query = { id: 'noop', operation: 'transaction', primary_db: 'Main',
      InputValidator: z.object({}), SessionValidator: z.object({}), ReturnData: z.object({}),
      session_args: [], optional_input_args: [], json_input_args: [],
      sql: [{ include: false, params: [], sql: 'select 1' }],
    };
    const sent = [];
    const result = await runWithSync(db, { noop: query }, 'noop', {}, {},
      new Map([['reader', { session: {} }]]), directory, 'origin',
      (id, message) => sent.push({ id, message }));
    assert.equal((await db.execute('select server_revision from _pyre_sync')).rows[0].server_revision, 1);
    assert.equal(result.response.serverRevision, 1);
    assert.equal(result.response.sync.type, 'syncRequired');
    assert.equal(sent[0].message.type, 'syncRequired');
    assert.equal(sent[0].message.serverRevision, 1);

    const committed = await run(db, { noop: query }, 'noop', {}, {}, undefined,
      async () => { throw Error('publication unavailable'); }, undefined,
      { mode: 'sync', commitSyncRevision: true });
    assert.equal(committed.response.serverRevision, 2, 'execution exposes commit evidence before publication');
    await assert.rejects(committed.sync(() => {}), /publication unavailable/);
    assert.equal(committed.response.serverRevision, 2, 'publication cannot erase commit evidence');
  } finally { db.close(); rmSync(directory, { recursive: true, force: true }); }
});

test('named publication is ordered across handles for the same database ID', async () => {
  const directory = mkdtempSync(new URL('../target/publication-order-', import.meta.url));
  const url = `file:${directory}/test.db`;
  const first = createClient({ url });
  const second = createClient({ url });
  const delivery = deferred();
  const delivering = deferred();
  const delivered = [];
  let pendingFirst, pendingSecond;
  try {
    await ensureDatabase(first, 'Main', 'record Note {\n    @public\n    id Id.Uuid @id\n    body String\n}\n');
    await loadSchemaFromDatabase(directory, first);
    const query = { id: 'update', operation: 'transaction', primary_db: 'Main',
      InputValidator: z.object({ id: z.string() }), SessionValidator: z.object({}),
      session_args: [], optional_input_args: [], json_input_args: [],
      sql: [
        { include: false, params: ['id'], sql: "insert into notes(id, body) values($id, 'changed')" },
        { include: true, params: ['id'], sql: `select json_array(json_object(
          'table_name', 'notes', 'headers', json_array('id', 'body', 'updatedAt'),
          'rows', json_array(json_array(id, body, updatedAt)))) as _affectedRows from notes where id = $id` },
      ],
    };
    const send = async (_id, message) => {
      if (message.serverRevision === 1) { delivering.resolve(); await delivery.promise; }
      delivered.push(message);
    };
    const run = (db, id) => runWithSync(db, { update: query }, 'update', { id }, {},
      new Map([['reader', { session: {} }]]), directory, undefined, send);
    pendingFirst = run(first, '00000000-0000-0000-0000-000000000001');
    void pendingFirst.then(() => delivering.reject(Error('Expected first publication')), delivering.reject);
    await delivering.promise;
    pendingSecond = run(second, '00000000-0000-0000-0000-000000000002');
    await turn();
    assert.deepEqual(delivered, [], 'later revision must not overtake unresolved earlier publication');
    delivery.resolve();
    await Promise.all([pendingFirst, pendingSecond]);
    assert.deepEqual(delivered.map(message => message.serverRevision), [1, 2]);
    assert.ok(delivered.every(message => message.type === 'delta'));
    assert.deepEqual(delivered.map(message => message.data[0].rows[0][0]), [
      '00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000002',
    ]);
  } finally {
    delivery.resolve();
    await Promise.allSettled([pendingFirst, pendingSecond]);
    first.close(); second.close(); rmSync(directory, { recursive: true, force: true });
  }
});
