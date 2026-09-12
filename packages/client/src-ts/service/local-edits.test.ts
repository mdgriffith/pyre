// @ts-nocheck
import { afterAll, afterEach, expect, test, spyOn } from 'bun:test';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import vm from 'node:vm';
import { LocalEditsRuntime, edit, batch } from './local-edits';
import { QueryManagerService } from './query-manager';
import { QueryClientService } from './query-client';
import { EntityStreamService } from './entity-stream';
import { IndexedDBStorage } from './indexeddb';
import { PyreClient } from '../index';
import { elmLocalEdits } from './elm-local-edits';

const root = new URL('../../../../', import.meta.url).pathname;
const temp = mkdtempSync(`${root}target/ts-local-edits-`);
execFileSync('npm', ['exec', '--yes', '--package=elm@0.19.1-6', '--', 'elm', 'make', 'src/Main.elm', '--optimize', `--output=${temp}/main.js`], {
  cwd: `${root}packages/client`, stdio: 'pipe',
});
const compiled = readFileSync(`${temp}/main.js`, 'utf8');
afterAll(() => rmSync(temp, { recursive: true, force: true }));
const tick = () => Bun.sleep(10);
async function until(predicate) { for (let i = 0; i < 100 && !predicate(); i++) await tick(); expect(Boolean(predicate())).toBe(true); }
function deferred() { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
const fence = { databaseId: 'main', instance: 'tab', authGeneration: 1, namespace: 'Main', manifest: 'm1', databaseEpoch: 'e1' };
const key = '00000000-0000-0000-0000-000000000001';
const row = { key, name: 'base', note: 'server' };
const schema = { tables: { users: { name: 'users', primaryKey: { name: 'key', kind: 'uuid' }, links: {}, indices: [] } }, queryFieldToTable: { users: 'users' } };
const command = { id: 'command@m1', parseInput: value => { if (!value || typeof value !== 'object') throw Error(); return value; }, decodeResult: value => { if (typeof value !== 'string') throw Error(); return value; } };
const operation = kind => ({
  id: `${kind}@m1`, parseInput: value => { if (!value || typeof value.key !== 'string') throw Error(); return value; },
  decodeResult: value => { if (!value || value.id !== key) throw Error(); return value; },
  predict: input => ({ safe: true, kind, table: 'users', id: input.key, fields: kind === 'create' ? input : Object.fromEntries(Object.entries(input).filter(([k]) => k !== 'key')), writableFields: ['name', 'note'], materializedFields: ['key', 'name', 'note'] }),
});
const update = operation('update'), create = operation('create'), remove = operation('delete');
const all = [command, update, create, remove];
const runtimes = [];
afterEach(async () => { for (const runtime of runtimes.splice(0)) runtime.dispose(); await tick(); });

function harness(options = {}) {
  const context = vm.createContext({ setTimeout, clearTimeout, console });
  vm.runInContext(compiled, context);
  const app = context.Elm.Main.init({ flags: { schema, server: { baseUrl: 'https://never-used.invalid', catchupPath: '/unused' }, sync: { autoStart: false } } });
  const manager = new QueryManagerService();
  const queries = new QueryClientService();
  const entities = new EntityStreamService(schema);
  const preparations = [], writes = [], reads = [], publications = [], persisted = [], failures = [], lifecycle = [], ingress = [], envelopes = [];
  const runtime = new LocalEditsRuntime({
    fence, minimumSafeRevision: 0, operations: all, timeoutMs: 1000,
    prepare: async (request, signal) => {
      preparations.push({ request, signal });
      if (options.preparation) await options.preparation.promise;
      return { dispatch: dispatchSignal => { const response = deferred(); writes.push({ request, signal: dispatchSignal, ...response }); return response.promise; }, dispose: () => { preparations.find(p => p.request === request).disposed = true; } };
    },
    replacement: (request, signal) => { const response = deferred(); reads.push({ request, signal, ...response }); return response.promise; },
    ...options,
  }, {
    send: message => { ingress.push(message.message); manager.sendLocalEdits(message); },
    install: (publication, changes) => {
      const notifyQueries = queries.installPublication(changes);
      const notifyEntities = entities.installVisible(publication.tables, fence.databaseId);
      publications.push(publication);
      return () => { notifyQueries(); notifyEntities(); };
    },
    persist: async (tables, revision, storedFence) => { persisted.push({ tables, revision, fence: storedFence }); if (options.persistence) await options.persistence.promise; },
  });
  app.ports.queryManagerOut.subscribe(message => envelopes.push(message));
  manager.attachPorts(app);
  manager.setLocalEdits(runtime);
  queries.attachPorts(app);
  runtime.onEditFailure(event => failures.push(event));
  runtime.onLifecycle(event => lifecycle.push(event));
  runtimes.push(runtime);
  runtime.start();
  const h = { runtime, app, queries, entities, preparations, writes, reads, publications, persisted, failures, lifecycle, ingress, envelopes,
    rows: () => publications.at(-1)?.tables.users ?? [],
    async replace(rows = [row], revision = 0, index = reads.length - 1) {
      const read = reads[index];
      read.resolve({ ...read.request, type: 'replacement', scope: 'database', complete: true, serverRevision: revision, tables: { users: { rows } } });
      await tick();
    },
    accepted(index = 0, revision = 1, values) {
      const write = writes[index];
      const response = { ...fence, requestId: write.request.requestId, status: 'accepted', commitRevision: revision,
        results: write.request.operations.map((op, index) => ({ index, operation: op.operation, value: values ? values[index] : op.operation === command.id ? 'named-result' : { id: key } })),
        reconciliation: { kind: 'replaceRequired', atLeast: revision, invalidate: false } };
      write.resolve(response); return response;
    },
    rejected(index = 0) { writes[index].resolve({ ...fence, requestId: writes[index].request.requestId, status: 'rejected', code: 'TargetNotWritable' }); },
  };
  return h;
}
async function ready(options) { const h = harness(options); await until(() => h.reads.length === 1); await h.replace(); return h; }

test.each([
  ['commitRevision', { commitRevision: 1 }],
  ['results', { results: [] }],
  ['reconciliation', { reconciliation: { kind: 'replaceRequired', atLeast: 1, invalidate: false } }],
])('rejected response carrying %s is malformed outcome-unknown', async (_field, evidence) => {
  const h = await ready();
  const first = h.runtime.submit(edit(command, {}));
  h.runtime.submit(edit(command, {}));
  await until(() => h.writes.length === 1);
  h.writes[0].resolve({ ...fence, requestId: h.writes[0].request.requestId, status: 'rejected', code: 'PermissionDenied', ...evidence });
  expect(await first.confirmed).toEqual({ kind: 'outcomeUnknown', code: 'MalformedResponse' });
  await tick();
  expect(h.writes).toHaveLength(1);
  expect(h.lifecycle.some(event => event.requestId === first.requestId && event.state === 'rejected')).toBe(false);
  expect(h.failures).toContainEqual(expect.objectContaining({ requestId: first.requestId, phase: 'transport', code: 'MalformedResponse', certainty: 'unknown' }));
  expect(h.publications.at(-1)?.invalid).toBe(true);
});

test.skipIf(!process.env.PYRE_GENERATED_EDITS)('real generated builders execute through worker and services', async () => {
  const { Main, Records, Commands, batch: generatedBatch, operations, manifestVersion } = await import(process.env.PYRE_GENERATED_EDITS!);
  const { User, Audit, Token } = Records;
  const { planKey } = await import('@pyre/core/local-edits');
  const complete = Token.create({ key, text: 'known', updatedAt: 0 })[planKey].operations[0];
  expect(complete.definition.predict(complete.input)).toMatchObject({ safe: true, kind: 'create', id: key });
  const integer = Audit.create({ message: 'audit' })[planKey].operations[0];
  expect(integer.definition.predict).toBeUndefined();
  const serverOwned = User.create({ key, name: 'new', fixed: 'x' })[planKey].operations[0];
  expect(serverOwned.definition.predict).toBeUndefined();
  const generatedFence = { ...fence, namespace: Main.name, manifest: manifestVersion };
  const h = await ready({ fence: generatedFence, operations });
  const db = h.runtime.bind(Main);
  const input = { key, name: 'created', fixed: 'x', note: null, status: { _type: 'Closed', reason: 'original' } };
  const edits = [User.create(input), Audit.create({ message: 'audit' }), User.update(key, { note: null }), Commands.rename({ key, name: 'final' })];
  const plan = generatedBatch(edits);
  input.status.reason = 'mutated'; edits.length = 0;
  const receipt = db.submit(plan);
  await until(() => h.writes.length === 1);
  const request = h.writes[0].request;
  expect(request.operations).toHaveLength(4);
  expect(request.operations[0].input.status.reason).toBe('original');
  expect(h.ingress.find(m => m.type === 'submit').operations.every(op => !op.prediction)).toBe(true);
  const values = [{ id: key }, { id: 7 }, { id: key }, { user: [{ name: 'final' }] }];
  h.writes[0].resolve({ ...generatedFence, requestId: request.requestId, status: 'accepted', commitRevision: 1,
    results: request.operations.map((op, index) => ({ index, operation: op.operation, value: values[index] })),
    reconciliation: { kind: 'replaceRequired', atLeast: 1, invalidate: false } });
  await until(() => h.reads.length === 2);
  await h.replace([{ key, name: 'final', note: null }], 1);
  expect((await receipt.confirmed).kind).toBe('confirmed');
  expect((await receipt.confirmed).result[1]).toEqual({ id: 7 });
  expect((await db.submit(User.update(key, {})).confirmed).kind).toBe('rejected');
  expect((await db.submit(User.update(key, { fixed: 'forbidden' })).confirmed).kind).toBe('rejected');
  expect(() => h.runtime.bind({ ...Main, name: 'Other' })).toThrow('Namespace mismatch');
  expect(await db.submit(generatedBatch([])).confirmed).toMatchObject({ kind: 'confirmed', result: [] });
});

test.skipIf(!process.env.PYRE_GENERATED_EDITS)('real generated builders use server manifest IDs and result codecs', async () => {
  const { Main, Records, Commands, batch: generatedBatch, operations, manifestVersion } = await import(process.env.PYRE_GENERATED_EDITS!);
  const { User, Audit } = Records;
  const { manifest } = await import(process.env.PYRE_GENERATED_EDITS!.replace('edits.ts', 'manifest.ts'));
  const { createClient } = await import('@libsql/client');
  const serverModule = '../../../server/query';
  const { runBatch } = await import(serverModule);
  const sql = createClient({ url: `file:${temp}/generated.db` });
  try {
    await sql.executeMultiple(`create table users(key text primary key, name text, note text, reviewer text, status blob, fixed text, updatedAt integer);
      create table audits(id integer primary key, message text, label text default 'audit', updatedAt integer);
      create table _pyre_sync(id integer primary key, database_epoch text, server_revision integer);
      insert into _pyre_sync values(1,'e1',0);`);
    const generatedFence = { ...fence, namespace: Main.name, manifest: manifestVersion };
    const responses = [];
    const h = await ready({ fence: generatedFence, operations, prepare: async request => ({ dispatch: async () => {
      const result = await runBatch(sql, manifest, generatedFence, request, {});
      responses.push(result);
      if (result.kind !== 'success') throw new Error(JSON.stringify(result));
      return result.response;
    } }) });
    const db = h.runtime.bind(Main);
    const receipt = db.submit(generatedBatch([User.create({ key, name: 'new', fixed: 'x' }), Audit.create({ message: 'audit' }),
      User.update(key, { note: null }), Commands.rename({ key, name: 'final' })]));
    await until(() => responses.length === 1);
    expect(responses[0].kind).toBe('success');
    await until(() => h.reads.length === 2);
    await h.replace([{ key, name: 'final', note: null }], 1);
    const outcome = await receipt.confirmed;
    expect(outcome.kind).toBe('confirmed');
    expect(outcome.result[0]).toEqual({ id: key });
    expect(outcome.result[1]).toEqual({ id: 1 });
    expect(outcome.result[3]).toEqual({ user: [{ name: 'final' }] });
    const removed = db.submit(User.delete(key));
    await until(() => responses.length === 2);
    await until(() => h.reads.length === 3);
    await h.replace([], 2);
    expect((await removed.confirmed).result).toEqual({ id: key });
    expect((await sql.execute('select * from users')).rows).toEqual([]);
  } finally { sql.close(); }
});

test.skipIf(!process.env.PYRE_GENERATED_EDITS)('real generated builders prove prediction and validate server input codecs', async () => {
  const { Main, Records, operations } = await import(process.env.PYRE_GENERATED_EDITS!);
  const { Token, NullableToken, DefaultToken, PrivateToken, Clock, Marker, User, Audit } = Records;
  const { planKey } = await import('@pyre/core/local-edits');
  const descriptor = plan => plan[planKey].operations[0];
  for (const plan of [PrivateToken.create({ key, text: 'visible', updatedAt: 0 }), PrivateToken.update(key, { text: 'hidden' }), PrivateToken.delete(key),
    DefaultToken.create({ key, text: 'explicit', updatedAt: 0 }), Clock.create({ key, at: 1700000000 }), Marker.update(key, { text: 'next' })]) {
    expect(descriptor(plan).definition.predict).toBeUndefined();
  }
  const omitted = descriptor(NullableToken.create({ key, updatedAt: 0 }));
  expect(omitted.definition.predict(omitted.input)).toBeNull();
  for (const text of [null, 'value']) {
    const complete = descriptor(NullableToken.create({ key, text, updatedAt: 0 }));
    expect(complete.definition.predict(complete.input)).toMatchObject({ safe: true, fields: { key, text, updatedAt: 0 } });
  }
  const updatePlan = descriptor(Token.update(key, { text: 'next' }));
  expect(updatePlan.definition.predict(updatePlan.input)).toMatchObject({ kind: 'update', fields: { text: 'next' } });
  const h = await ready({ fence: { ...fence, namespace: Main.name, manifest: Main.manifest }, operations });
  const db = h.runtime.bind(Main);
  for (const invalid of [Clock.create({ key, at: 'not-a-date' }), Clock.create({ key, at: 1.5 }), User.delete('bad-uuid'),
    User.update(key, { name: null }), Audit.create({ message: 'x', label: null }), Audit.create({ id: 42, message: 'x' })]) {
    expect((await db.submit(invalid).confirmed).kind).toBe('rejected');
  }
  expect(h.ingress.filter(message => message.type === 'submit')).toHaveLength(0);
  expect(() => User.update(key, { key, name: 'no primary key setter' })).toThrow('InvalidEdit');
  const { manifest } = await import(process.env.PYRE_GENERATED_EDITS!.replace('edits.ts', 'manifest.ts'));
  const marker = descriptor(Marker.update(key, { text: 'next' }));
  expect(manifest.queries[marker.definition.id].sql.some(statement => statement.sql.includes('updatedAt = unixepoch()'))).toBe(true);
});

test('scoped plans capture namespace values as well as nested operation inputs', async () => {
  const { scopedEdit, scopedBatch, planKey } = await import('@pyre/core/local-edits');
  const scope = { name: fence.namespace, manifest: fence.manifest };
  const builder = scopedBatch(scope);
  const plan = scopedEdit(scope, command, { nested: ['original'] });
  scope.name = 'mutated'; scope.manifest = 'mutated';
  const combined = builder([plan]);
  expect(combined[planKey].namespace).toEqual({ name: fence.namespace, manifest: fence.manifest });
  expect(plan[planKey].namespace).toEqual(combined[planKey].namespace);
});

test('named result codecs retain non-JSON decoded values', async () => {
  const dated = { ...command, decodeResult: value => new Date(value) };
  const h = await ready({ operations: [dated] });
  const receipt = h.runtime.submit(edit(dated, {}));
  await until(() => h.writes.length === 1);
  h.accepted(0, 1, ['2026-01-01T00:00:00.000Z']);
  await until(() => h.reads.length === 2);
  await h.replace([row], 1);
  expect((await receipt.confirmed).result).toEqual(new Date('2026-01-01T00:00:00.000Z'));
});

test('production prepare handshake captures nested inputs and batches before async credentials, preserving order', async () => {
  const preparation = deferred();
  const h = await ready({ preparation });
  expect(Object.keys(h.reads[0].request).sort()).toEqual([...Object.keys(fence), 'version', 'requestId', 'target'].sort());
  expect(h.reads[0].request.version).toBe(1);
  const input = { key, name: 'captured', note: { nested: ['original'] } };
  const firstEdit = edit(update, input);
  input.name = 'mutated'; input.note.nested[0] = 'mutated';
  const plans = [firstEdit, edit(command, { nested: ['named'] })];
  const first = h.runtime.submit(batch(plans));
  plans.reverse();
  const second = h.runtime.submitNamed(command.id, { later: true });
  await until(() => h.preparations.length === 1);
  expect(h.writes).toHaveLength(0);
  // Mixed batch is entirely nonoptimistic.
  expect(h.rows()).toEqual([row]);
  expect(h.ingress.find(m => m.type === 'submit').operations.map(o => o.operation)).toEqual([update.id, command.id]);
  preparation.resolve();
  await until(() => h.writes.length === 1);
  const request = h.writes[0].request;
  expect(request.operations[0].input).toEqual({ key, name: 'captured', note: { nested: ['original'] } });
  expect(Object.keys(request).sort()).toEqual([...Object.keys(fence), 'version', 'requestId', 'sequence', 'operations'].sort());
  expect(request.operations.every(op => !('prediction' in op))).toBe(true);
  expect(h.ingress.filter(m => m.type === 'prepared')).toHaveLength(1);
  expect(h.lifecycle.filter(e => e.requestId === first.requestId).map(e => e.state)).toEqual(['queued', 'sent']);
  h.accepted(0, 1, [{ id: key }, 'typed']);
  await until(() => h.writes.length === 2 && h.reads.length === 2);
  let confirmed = false; first.confirmed.then(() => { confirmed = true; });
  await tick(); expect(confirmed).toBe(false);
  await h.replace([{ ...row, name: 'server correction' }], 1);
  expect(await first.confirmed).toEqual({ kind: 'confirmed', commitRevision: 1, result: [{ id: key }, 'typed'] });
  h.rejected(1);
  expect((await second.confirmed).kind).toBe('rejected');
});

test('all query and entity readers observe one installed envelope; rejected create and delete rollback leave no ghosts', async () => {
  const h = await ready();
  const results = [], entityBatches = [], initialFromQuery = [];
  h.queries.registerQuery({ queryId: 'all', querySource: { users: { '@fields': ['key', 'name', 'note'] } }, input: {} }, result => {
    results.push(result);
    const unsub = h.entities.subscribeVisible({ tables: [{ tableName: 'users' }] }, batch => initialFromQuery.push(batch)); unsub();
  });
  h.entities.subscribeVisible({ tables: [{ tableName: 'users' }] }, batch => entityBatches.push(batch));
  await tick();
  const deletion = h.runtime.submit(edit(remove, { key }));
  await until(() => h.writes.length === 1);
  expect(h.rows()).toEqual([]);
  expect(entityBatches.at(-1).changes).toContainEqual({ tableName: 'users', id: key, op: 'remove' });
  expect(initialFromQuery.at(-1).changes).toEqual([]);
  h.rejected(); await deletion.confirmed; await tick();
  expect(h.rows()).toEqual([row]);
  expect(entityBatches.at(-1).changes[0].row).toEqual(row);
  h.runtime.receiveHint({ ...fence, type: 'syncRequired', reconciliation: { kind: 'replaceRequired', atLeast: 1, invalidate: false } });
  await until(() => h.reads.length === 2); await h.replace([], 1);
  const creation = h.runtime.submit(edit(create, row));
  await until(() => h.writes.length === 2); expect(h.rows()).toEqual([row]);
  h.rejected(1); await creation.confirmed; await tick();
  expect(h.rows()).toEqual([]);
  expect(entityBatches.at(-1).changes).toContainEqual({ tableName: 'users', id: key, op: 'remove' });
  expect(initialFromQuery.at(-1).changes).toEqual([]);
  expect(h.failures.filter(f => f.certainty === 'rejected')).toHaveLength(2);
  expect(results.length).toBeGreaterThan(3);
});

test('only complete authoritative replacements are persisted, ordered with invalidation and not optimistic visible', async () => {
  const persistence = deferred();
  const h = await ready({ persistence });
  const receipt = h.runtime.submit(edit(update, { key, name: 'local' }));
  await until(() => h.writes.length === 1);
  expect(h.rows()[0].name).toBe('local');
  expect(h.persisted).toHaveLength(1); expect(h.persisted[0].tables).toBeNull();
  persistence.resolve(); await tick();
  expect(h.persisted.at(-1).tables).toEqual({ users: [row] });
  h.accepted(); await until(() => h.reads.length === 2);
  await h.replace([{ ...row, name: 'corrected' }], 1);
  expect((await receipt.confirmed).kind).toBe('confirmed');
  expect(h.persisted.at(-1)).toEqual({ tables: { users: [{ ...row, name: 'corrected' }] }, revision: 1, fence });
  h.runtime.receiveHint({ ...fence, type: 'syncRequired', reconciliation: { kind: 'replaceRequired', atLeast: 2, invalidate: true, minimumSafeRevision: 2 } });
  await tick(); expect(h.rows()).toEqual([]); expect(h.persisted.at(-1).tables).toBeNull();
});

test('timeout is unknown without retries; late acceptance updates subscriptions but not the resolved promise', async () => {
  const h = await ready({ timeoutMs: 80 });
  const receipt = h.runtime.submitNamed(command.id, {});
  const states = []; receipt.subscribe(e => states.push(e.state));
  h.runtime.submitNamed(command.id, { later: true });
  await until(() => h.writes.length === 1);
  expect((await receipt.confirmed).kind).toBe('outcomeUnknown');
  await tick(); expect(h.writes).toHaveLength(1);
  expect(h.failures.filter(f => f.certainty === 'unknown')).toHaveLength(1);
  h.accepted();
  await until(() => h.reads.length === 2 && h.writes.length === 2);
  await h.replace([row], 1);
  expect(states).toEqual(['queued', 'sent', 'outcomeUnknown', 'accepted', 'confirmed']);
  expect((await receipt.confirmed).kind).toBe('outcomeUnknown');
});

test.each(['count', 'operation', 'index', 'codec', 'missingFence', 'malformed'])('invalid response %s cannot expose typed results or release ordering', async defect => {
  const h = await ready();
  const receipt = h.runtime.submitNamed(command.id, {});
  h.runtime.submitNamed(command.id, {});
  await until(() => h.writes.length === 1);
  let response = { ...fence, requestId: receipt.requestId, status: 'accepted', commitRevision: 1, reconciliation: { kind: 'replaceRequired', atLeast: 1, invalidate: false }, results: [{ index: 0, operation: command.id, value: 'result' }] };
  if (defect === 'count') response.results = [];
  if (defect === 'operation') response.results[0].operation = update.id;
  if (defect === 'index') response.results[0].index = 1;
  if (defect === 'codec') response.results[0].value = { private: 'row' };
  if (defect === 'missingFence') delete response.instance;
  if (defect === 'malformed') response = null;
  h.writes[0].resolve(response);
  expect((await receipt.confirmed).kind).toBe('outcomeUnknown');
  expect(h.writes).toHaveLength(1);
  expect(JSON.stringify(h.failures)).not.toContain('private');
});

test('same-lifetime SSE replacement quarantines sent overlay without manufacturing an unknown outcome', async () => {
  const h = await ready();
  const receipt = h.runtime.submit(edit(update, { key, name: 'local' }));
  await until(() => h.writes.length === 1);
  h.runtime.receiveHint({ ...fence, type: 'syncRequired', reconciliation: { kind: 'replaceRequired', atLeast: 1, invalidate: false } });
  await until(() => h.reads.length === 2); await h.replace([{ ...row, name: 'newer server' }], 1);
  expect(h.rows()[0].name).toBe('newer server'); expect(h.failures).toEqual([]);
  expect(h.lifecycle.at(-1).state).toBe('sent');
  h.accepted(); expect((await receipt.confirmed).kind).toBe('confirmed');
  expect(h.rows()[0].name).toBe('newer server');
});

test('offline preparation is cancelled safely, reconnect resumes queue, disconnect after dispatch is unknown', async () => {
  const preparation = deferred(); const h = await ready({ preparation });
  const receipt = h.runtime.submitNamed(command.id, {});
  await until(() => h.preparations.length === 1);
  h.runtime.setConnected(false); preparation.resolve(); await tick();
  expect(h.writes).toEqual([]); expect(h.preparations[0].disposed).toBe(true);
  expect(h.failures).toEqual([]);
  h.runtime.setConnected(true); await until(() => h.writes.length === 1);
  h.runtime.setConnected(false);
  expect((await receipt.confirmed).kind).toBe('outcomeUnknown');
  expect(h.writes[0].signal.aborted).toBe(true);
  h.runtime.setConnected(true); await tick(); expect(h.writes).toHaveLength(1);
});

test('preparation failure and unsent cancel are definite rejections, empty batch has no I/O or publication', async () => {
  const h = await ready({ prepare: async () => { throw Error('secret credentials'); } });
  const before = h.publications.length;
  expect(await h.runtime.submit(batch([])).confirmed).toEqual({ kind: 'confirmed', result: [], commitRevision: undefined });
  expect(h.publications).toHaveLength(before);
  expect(h.writes).toEqual([]);
  const receipt = h.runtime.submitNamed(command.id, {});
  expect((await receipt.confirmed).kind).toBe('rejected');
  expect(h.failures.at(-1).phase).toBe('preparation');
  expect(JSON.stringify(h.failures)).not.toContain('secret');
  h.runtime.setConnected(false);
  const cancelled = h.runtime.submitNamed(command.id, {}); cancelled.cancel();
  expect((await cancelled.confirmed).kind).toBe('rejected');
});

test('disposal delivers acceptedUnreconciled and unsent rejection before detaching subscriptions and transports', async () => {
  let cleaned = false;
  const h = await ready({ subscribeHints: () => () => { cleaned = true; } });
  const accepted = h.runtime.submitNamed(command.id, {});
  await until(() => h.writes.length === 1); h.accepted(); await until(() => h.reads.length === 2);
  h.runtime.setConnected(false);
  const queued = h.runtime.submitNamed(command.id, {});
  const observed = []; accepted.subscribe(event => observed.push(event.state));
  h.runtime.dispose();
  expect((await accepted.confirmed).kind).toBe('acceptedUnreconciled');
  expect((await queued.confirmed).kind).toBe('rejected');
  await tick(); expect(cleaned).toBe(true); expect(h.rows()).toEqual([]);
  expect(observed.at(-1)).toBe('acceptedUnreconciled');
  expect(h.failures.filter(e => e.phase === 'lifetime')).toHaveLength(2);
  const count = h.lifecycle.length;
  h.runtime.receiveResponse(accepted.requestId, { ...fence, requestId: accepted.requestId, status: 'rejected', code: 'PermissionDenied' });
  await tick(); expect(h.lifecycle).toHaveLength(count);
  expect(h.persisted.at(-1).tables).toBeNull();
});

test('failed catchup retains accepted knowledge; read-only retry confirms and stale fences do not settle', async () => {
  const h = await ready(); const receipt = h.runtime.submitNamed(command.id, {});
  await until(() => h.writes.length === 1);
  h.runtime.receiveResponse(receipt.requestId, { ...fence, instance: 'old', requestId: receipt.requestId, status: 'rejected', code: 'PermissionDenied' });
  await tick(); expect(h.lifecycle.at(-1).state).toBe('sent');
  h.accepted(); await until(() => h.reads.length === 2); h.reads[1].reject(Error('private'));
  await until(() => h.failures.some(f => f.phase === 'reconciliation'));
  expect(h.lifecycle.at(-1).state).toBe('accepted');
  h.runtime.retryCatchup(); await until(() => h.reads.length === 3); await h.replace([row], 1);
  expect((await receipt.confirmed).kind).toBe('confirmed'); expect(h.writes).toHaveLength(1);
});

test('epoch mismatch ends the old lifetime instead of adopting foreign revision', async () => {
  const h = await ready(); const receipt = h.runtime.submitNamed(command.id, {});
  await until(() => h.writes.length === 1);
  h.runtime.receiveResponse(receipt.requestId, { ...fence, databaseEpoch: 'e2', requestId: receipt.requestId, status: 'rejected', code: 'PermissionDenied' });
  expect((await receipt.confirmed).kind).toBe('outcomeUnknown'); await tick(); expect(h.rows()).toEqual([]);
  expect(h.runtime.fence.databaseEpoch).toBe('e1');
});

test('entity complete publication removes filter exits, absent keys, and cross-table rows in one batch', () => {
  const stream = new EntityStreamService(schema), batches = [];
  stream.subscribeVisible({ tables: [{ tableName: 'users', where: { name: 'base' } }] }, b => batches.push(b));
  stream.installVisible({ users: [row] })();
  const publish = stream.installVisible({ users: [{ ...row, name: 'no longer matching' }] });
  const initial = []; stream.subscribeVisible({ tables: [{ tableName: 'users' }] }, b => initial.push(b));
  expect(initial[0].changes[0].row.name).toBe('no longer matching');
  publish(); expect(batches.at(-1).changes).toEqual([{ tableName: 'users', id: key, op: 'remove' }]);
});

test('storage replacement uses one transaction for complete rows, epoch, fence, coverage and cursor eviction', async () => {
  const storage = new IndexedDBStorage('test', schema), calls = [];
  let tx;
  storage.getDB = async () => ({ transaction(names, mode) {
    calls.push(['transaction', names, mode]);
    tx = { objectStore: name => ({
      get: () => { const request = { result: JSON.stringify([fence.databaseId, fence.instance, fence.authGeneration, fence.namespace, fence.manifest, fence.databaseEpoch]) }; queueMicrotask(() => request.onsuccess()); return request; },
      clear: () => calls.push(['clear', name]), put: (value, key) => calls.push(['put', name, value, key]),
    }), abort() { tx.onabort(); } };
    setTimeout(() => tx.oncomplete(), 0); return tx;
  } });
  await storage.replaceAuthoritative({ users: [row] }, 9, fence);
  expect(calls.filter(c => c[0] === 'transaction')).toEqual([['transaction', ['tables', 'syncCursor', 'meta'], 'readwrite']]);
  expect(calls).toContainEqual(['put', 'meta', 9, 'lastAppliedServerRevision']);
  expect(calls).toContainEqual(['put', 'meta', fence, 'localEditsFence']);
  expect(calls).toContainEqual(['clear', 'syncCursor']);
  const before = calls.length;
  await expect(storage.replaceAuthoritative({ users: [row, row] }, 10, fence)).rejects.toThrow('Duplicate');
  expect(calls).toHaveLength(before);
  await storage.replaceAuthoritative(null, null, fence);
  expect(calls.slice(before).filter(c => c[0] === 'put').map(c => c[3])).toEqual(['localEditsOwner']);
});

test('public bound database and existing compiled named run share synchronous invocation ordering', async () => {
  const h = await ready();
  const client = await PyreClient.create({ schema, cacheNamespace: 'test', server: { baseUrl: 'https://unused.invalid', localEdits: () => ({}) },
    createInternalClient: async () => ({
      getLocalEdits: () => h.runtime,
      run: () => { throw Error('must not escape to a separate named transport'); },
      onDevtoolsEvent: () => () => {}, onSyncState: callback => { callback({ status: 'not_started', tables: {} }); return () => {}; },
      disconnect: () => h.runtime.dispose(),
    }),
  });
  const db = await client.localEdits('main');
  const input = { nested: ['first'] }, results = [];
  void client.run('main', { operation: 'mutation', id: command.id }, input, value => results.push(value));
  input.nested[0] = 'changed';
  const receipt = db.submit(edit(update, { key, name: 'second' }));
  const submissions = h.ingress.filter(m => m.type === 'submit');
  expect(submissions.map(m => m.operations[0].operation)).toEqual([command.id, update.id]);
  expect(submissions[0].operations[0].input).toEqual({ nested: ['first'] });
  await until(() => h.writes.length === 1); h.accepted(); await until(() => h.reads.length === 2 && h.writes.length === 2);
  await h.replace([row], 1);
  expect(results).toEqual([{ ok: true, value: 'named-result' }]);
  h.rejected(1); await receipt.confirmed;
  client.disconnect();
});

test('duplicate evidence cannot replace the first accepted typed result before confirmation', async () => {
  const h = await ready(), receipt = h.runtime.submitNamed(command.id, {});
  await until(() => h.writes.length === 1);
  const evidence = value => ({ ...fence, requestId: receipt.requestId, status: 'accepted', commitRevision: 1,
    reconciliation: { kind: 'replaceRequired', atLeast: 1, invalidate: false }, results: [{ index: 0, operation: command.id, value }] });
  h.runtime.receiveResponse(receipt.requestId, evidence('first'));
  h.runtime.receiveResponse(receipt.requestId, evidence('second'));
  await until(() => h.reads.length === 2); await h.replace([row], 1);
  expect(await receipt.confirmed).toEqual({ kind: 'confirmed', commitRevision: 1, result: 'first' });
});

test('malformed and insufficient complete replacement cannot advance coverage or confirm', async () => {
  const h = await ready();
  const receipt = h.runtime.submitNamed(command.id, {});
  await until(() => h.writes.length === 1); h.accepted(0, 3); await until(() => h.reads.length === 2);
  const request = h.reads[1].request;
  h.reads[1].resolve({ ...request, type: 'replacement', scope: 'database', complete: false, serverRevision: 9, tables: { users: { rows: [] } } });
  await until(() => h.failures.some(f => f.phase === 'reconciliation'));
  expect(h.rows()).toEqual([row]); expect(h.persisted.at(-1).revision).toBe(0);
  h.runtime.retryCatchup(); await until(() => h.reads.length === 3);
  await h.replace([], 2);
  expect(h.persisted.at(-1).revision).toBe(0); expect(h.lifecycle.at(-1).state).toBe('accepted');
  h.runtime.retryCatchup(); await until(() => h.reads.length === 4); await h.replace([], 3);
  expect((await receipt.confirmed).kind).toBe('confirmed'); expect(h.rows()).toEqual([]);
});

test('late read below a new security minimum never republishes revoked rows', async () => {
  const h = await ready();
  h.runtime.receiveHint({ ...fence, type: 'syncRequired', reconciliation: { kind: 'replaceRequired', atLeast: 1, invalidate: false } });
  await until(() => h.reads.length === 2);
  h.runtime.receiveHint({ ...fence, type: 'syncRequired', reconciliation: { kind: 'replaceRequired', atLeast: 3, invalidate: true, minimumSafeRevision: 3 } });
  await tick(); expect(h.rows()).toEqual([]);
  await h.replace([row], 1); await until(() => h.reads.length === 3);
  expect(h.rows()).toEqual([]); expect(h.reads[2].request.target).toBe(3);
  await h.replace([], 3); expect(h.rows()).toEqual([]); expect(h.persisted.at(-1).revision).toBe(3);
});

test('old lifetime persistence cannot clear a newer claimed cache', async () => {
  const storage = new IndexedDBStorage('test', schema), mutations = [];
  storage.getDB = async () => ({ transaction() {
    const tx = { objectStore: () => ({
      get() { const request = { result: 'new owner' }; queueMicrotask(() => request.onsuccess()); return request; },
      clear: () => mutations.push('clear'), put: () => mutations.push('put'),
    }) };
    setTimeout(() => tx.oncomplete(), 0); return tx;
  } });
  await storage.replaceAuthoritative({ users: [row] }, 1, fence);
  await storage.replaceAuthoritative(null, null, fence);
  expect(mutations).toEqual([]);
});

test('initial offline state performs no adapter I/O, reconnect initializes before ordered dispatch', async () => {
  const h = harness({ connected: false });
  const receipt = h.runtime.submitNamed(command.id, {});
  await tick(); expect(h.reads).toEqual([]); expect(h.preparations).toEqual([]);
  expect(h.lifecycle.at(-1).state).toBe('queued');
  h.runtime.setConnected(true); await until(() => h.reads.length === 1); await h.replace();
  await until(() => h.writes.length === 1); h.rejected(); expect((await receipt.confirmed).kind).toBe('rejected');
});

test('preparation timeout definitely rejects, disposes late preparation, and never dispatches it', async () => {
  const preparation = deferred(), h = await ready({ preparation, timeoutMs: 80 });
  const receipt = h.runtime.submitNamed(command.id, {});
  expect((await receipt.confirmed).kind).toBe('rejected');
  expect(h.failures.at(-1).certainty).toBe('rejected');
  expect(h.preparations[0].signal.aborted).toBe(true);
  preparation.resolve(); await tick(); expect(h.preparations[0].disposed).toBe(true); expect(h.writes).toEqual([]);
});

test('named generated operations suppress optimism and malformed inputs still produce failure receipts', async () => {
  const h = await ready();
  const receipt = h.runtime.submitNamed(update.id, { key, name: 'not yet visible' });
  await until(() => h.writes.length === 1); expect(h.rows()).toEqual([row]);
  expect(h.ingress.find(m => m.type === 'submit').operations[0]).not.toHaveProperty('prediction');
  h.rejected(); await receipt.confirmed;
  expect((await h.runtime.submitNamed(command.id, { invalid: undefined }).confirmed).kind).toBe('rejected');
  expect(h.failures.at(-1).phase).toBe('validation');
  expect(h.writes).toHaveLength(1);
});

test('dispose during preparation delivers final events before cleanup and closes even when preparation never returns', async () => {
  const h = await ready({ preparation: deferred() });
  const receipt = h.runtime.submitNamed(command.id, {});
  await until(() => h.preparations.length === 1);
  let final = false; receipt.subscribe(event => { if (event.state === 'rejected') final = true; });
  await h.runtime.dispose();
  expect(final).toBe(true); expect((await receipt.confirmed).kind).toBe('rejected');
  expect(h.preparations[0].signal.aborted).toBe(true); expect(h.writes).toEqual([]);
});

test('invalid accepted result codecs cannot discard authenticated security invalidation', async () => {
  const h = await ready(), receipt = h.runtime.submitNamed(command.id, {});
  await until(() => h.writes.length === 1);
  h.writes[0].resolve({ ...fence, requestId: receipt.requestId, status: 'accepted', commitRevision: 3,
    results: [{ index: 0, operation: command.id, value: 123 }],
    reconciliation: { kind: 'replaceRequired', atLeast: 3, invalidate: true, minimumSafeRevision: 3 } });
  expect((await receipt.confirmed).kind).toBe('outcomeUnknown');
  await tick();
  expect(h.rows()).toEqual([]);
  expect(h.persisted.at(-1).tables).toBeNull();
  expect(h.reads.at(-1).request.target).toBe(3);
});

test('an older safety hint cannot manufacture a barrier after newer uncertainty', async () => {
  const h = await ready();
  h.runtime.receiveHint({ ...fence, type: 'syncRequired', reconciliation: { kind: 'replaceRequired', atLeast: 3, invalidate: true } });
  await tick(); expect(h.rows()).toEqual([]);
  h.runtime.receiveHint({ ...fence, type: 'syncRequired', reconciliation: { kind: 'replaceRequired', atLeast: 1, invalidate: true, minimumSafeRevision: 1 } });
  await tick(); expect(h.reads).toHaveLength(1);
  h.runtime.receiveHint({ ...fence, type: 'syncRequired', reconciliation: { kind: 'replaceRequired', atLeast: 3, invalidate: true, minimumSafeRevision: 3 } });
  await until(() => h.reads.length === 2);
  await h.replace([], 3);
  expect(h.persisted.at(-1).revision).toBe(3);
});

test('revision-free security uncertainty cannot be cleared by a replayed barrier hint', async () => {
  const h = await ready();
  h.runtime.receiveHint({ ...fence, type: 'syncRequired' });
  await tick(); expect(h.rows()).toEqual([]);
  h.runtime.receiveHint({ ...fence, type: 'syncRequired', reconciliation: { kind: 'replaceRequired', atLeast: 0, invalidate: true, minimumSafeRevision: 0 } });
  await tick();
  expect(h.reads).toHaveLength(1);
  expect(h.rows()).toEqual([]);
});

test('unknown timeout releases transport resources but still accepts late definitive evidence', async () => {
  const h = await ready({ timeoutMs: 80 }), receipt = h.runtime.submitNamed(command.id, {});
  await until(() => h.writes.length === 1);
  expect((await receipt.confirmed).kind).toBe('outcomeUnknown');
  expect(h.writes[0].signal.aborted).toBe(true);
  expect(h.preparations[0].disposed).toBe(true);
  h.accepted();
  await until(() => h.reads.length === 2);
  await h.replace([row], 1);
  expect(h.lifecycle.at(-1).state).toBe('confirmed');
  expect((await receipt.confirmed).kind).toBe('outcomeUnknown');
});

test('lazy named submissions reserve order before initialization observers can submit', async () => {
  const h = await ready(), initialized = deferred();
  const client = await PyreClient.create({ schema, cacheNamespace: 'test', server: { baseUrl: 'https://unused.invalid', localEdits: () => ({}) },
    createInternalClient: () => initialized.promise,
  });
  let observed = false;
  client.onDevtoolsEvent(event => {
    if (event.operation === 'sync.not_started' && !observed) {
      observed = true;
      void client.run('main', { operation: 'mutation', id: command.id }, { order: 2 }, () => {});
    }
  });
  const first = client.run('main', { operation: 'mutation', id: command.id }, { order: 1 }, () => {});
  initialized.resolve({
    getLocalEdits: () => h.runtime,
    run: (_db, module, input) => h.runtime.submitNamed(module.id, input),
    onDevtoolsEvent: () => () => {}, onSyncState: callback => { callback({ status: 'not_started', tables: {} }); return () => {}; },
    disconnect: () => h.runtime.dispose(),
  });
  await first;
  expect(observed).toBe(true);
  expect(h.ingress.filter(m => m.type === 'submit').map(m => m.operations[0].input.order)).toEqual([1, 2]);
  client.disconnect();
});

test('disconnect during lazy initialization fences old submissions even when the database is reopened', async () => {
  const old = await ready(), current = await ready({ fence: { ...fence, instance: 'new' } });
  const initialized = deferred(); let creations = 0;
  const internal = h => ({
    getLocalEdits: () => h.runtime,
    onDevtoolsEvent: () => () => {}, onSyncState: callback => { callback({ status: 'not_started', tables: {} }); return () => {}; },
    disconnect: () => h.runtime.dispose(),
  });
  const client = await PyreClient.create({ schema, cacheNamespace: 'test', server: { baseUrl: 'https://unused.invalid', localEdits: () => ({}) },
    createInternalClient: () => ++creations === 1 ? initialized.promise : Promise.resolve(internal(current)),
  });
  const outcomes = [];
  const first = client.run('main', { operation: 'mutation', id: command.id }, { old: true }, outcome => outcomes.push(outcome));
  client.disconnect();
  await client.localEdits('main');
  initialized.resolve(internal(old));
  await first; await tick();
  expect(old.ingress.filter(m => m.type === 'submit')).toEqual([]);
  expect(current.ingress.filter(m => m.type === 'submit')).toEqual([]);
  expect(outcomes[0].outcome.kind).toBe('rejected');
  expect(await client.localEdits('main')).toBe(current.runtime);
  void client.run('main', { operation: 'mutation', id: command.id }, { current: true }, () => {});
  expect(current.ingress.filter(m => m.type === 'submit')).toHaveLength(1);
  client.disconnect();
});

test.each(['bound', 'lazy'])('public %s named calls report capture failures and retain immutable invocation intent', async mode => {
  const h = await ready(), initialized = deferred();
  const internal = {
    getLocalEdits: () => h.runtime,
    onDevtoolsEvent: () => () => {}, onSyncState: callback => { callback({ status: 'not_started', tables: {} }); return () => {}; },
    disconnect: () => h.runtime.dispose(),
  };
  const client = await PyreClient.create({ schema, cacheNamespace: 'test', server: { baseUrl: 'https://unused.invalid', localEdits: () => ({}) },
    createInternalClient: () => initialized.promise,
  });
  if (mode === 'bound') { initialized.resolve(internal); await client.localEdits('main'); }
  const outcomes = [], invalid = { bad: undefined }, valid = { nested: ['captured'] };
  try {
    const rejected = client.run('main', { operation: 'mutation', id: command.id }, invalid, result => outcomes.push(result));
    const submitted = client.run('main', { operation: 'mutation', id: command.id }, valid, result => outcomes.push(result));
    // Repairing invalid input after invocation must not silently turn it into a write.
    invalid.bad = 'repaired'; valid.nested[0] = 'changed';
    initialized.resolve(internal);
    await expect(rejected).resolves.toBeUndefined();
    await submitted;
    await until(() => h.writes.length === 1 && outcomes.length === 1);
    expect(outcomes).toEqual([{ ok: false, error: 'rejected', outcome: { kind: 'rejected', code: 'InvalidEdit' } }]);
    expect(h.failures).toHaveLength(1);
    expect(h.failures[0]).toMatchObject({ ...fence, phase: 'validation', code: 'InvalidEdit', certainty: 'rejected' });
    expect(h.lifecycle.find(event => event.requestId === h.failures[0].requestId).state).toBe('rejected');
    expect(h.writes[0].request.operations[0].input).toEqual({ nested: ['captured'] });
    expect(h.ingress.filter(message => message.type === 'submit')).toHaveLength(1);
    expect(h.preparations).toHaveLength(1);
    h.rejected(); await until(() => outcomes.length === 2);
  } finally { client.disconnect(); }
});

function elmEffect(requestId, order = 1, databaseId = 'main') {
  return { type: 'elm-local-edits', databaseId, requestId,
    operations: [{ namespace: 'Main', manifest: 'm1', operation: update.id, input: { key, name: String(order) } }] };
}
function bridgePorts(throws = false, resultName = 'pyre_receiveQueryDelta') {
  let receive;
  const events = [], errors = [];
  return { events, errors, emit: message => receive?.(message), config: {
    app: { ports: {
      pyreStoreOut: { subscribe: cb => { receive = cb; }, unsubscribe: cb => { if (receive === cb) receive = undefined; } },
      [resultName]: { send: event => { events.push(event); if (throws) throw Error('observer'); } },
    } }, onError: error => errors.push(error),
  } };
}
function bridgeInternal(h) {
  return { getLocalEdits: () => h.runtime, disconnect: () => h.runtime.dispose(),
    onDevtoolsEvent: () => () => {}, onSyncState: cb => { cb({ status: 'not_started', tables: {} }); return () => {}; } };
}

test.skipIf(!process.env.PYRE_GENERATED_EDITS)('production Elm bridge shares generated descriptors with the typed namespace handle', async () => {
  const { Main, Records, Commands, operations } = await import(process.env.PYRE_GENERATED_EDITS!);
  const { Audit } = Records;
  const { planKey } = await import('@pyre/core/local-edits');
  const generatedFence = { ...fence, namespace: Main.name, manifest: Main.manifest };
  const h = await ready({ fence: generatedFence, operations }), ports = bridgePorts();
  const client = await PyreClient.create({ schema, cacheNamespace: 'test', server: { baseUrl: 'https://unused.invalid', localEdits: () => { throw Error('no second config'); } },
    createInternalClient: async () => bridgeInternal(h) });
  try {
    client.attachElmBridge(ports.config);
    const db = await client.localEdits('main', Main);
    const plans = [Audit.create({ message: 'audit' }), Commands.rename({ key, name: 'renamed' })];
    const ops = plans.flatMap(plan => plan[planKey].operations.map(op => ({
      namespace: Main.name, manifest: Main.manifest, operation: op.definition.id, input: op.input,
    })));
    ports.emit({ type: 'elm-local-edits', databaseId: 'main', requestId: 'generated', operations: ops });
    const typed = db.submit(Commands.rename({ key, name: 'typed' }));
    await until(() => h.writes.length === 1);
    h.writes[0].resolve({ ...generatedFence, requestId: h.writes[0].request.requestId, status: 'accepted', commitRevision: 1,
      results: ops.map((op, index) => ({ index, operation: op.operation, value: index === 0 ? { id: 7 } : { user: [{ name: 'renamed' }] } })),
      reconciliation: { kind: 'replaceRequired', atLeast: 1, invalidate: false } });
    await until(() => h.reads.length === 2); await h.replace([row], 1);
    expect(ports.events.find(e => e.state === 'confirmed')).toMatchObject({ requestId: 'generated', results: [
      { index: 0, operation: ops[0].operation, value: { id: 7 } },
      { index: 1, operation: ops[1].operation, value: { user: [{ name: 'renamed' }] } },
    ] });
    expect(h.ingress.filter(m => m.type === 'submit').map(m => m.operations.length)).toEqual([2, 1]);
    client.disconnect(); await typed.confirmed;
  } finally { client.disconnect(); }
});

test.each(['bound', 'lazy'])('production Elm bridge preserves mixed %s FIFO, snapshots and effect deduplication', async mode => {
  const h = await ready(), initialized = deferred(), ports = bridgePorts();
  const client = await PyreClient.create({ schema, cacheNamespace: 'test',
    server: { baseUrl: 'https://unused.invalid', localEdits: () => { throw Error('must use runtime descriptors'); } },
    createInternalClient: () => initialized.promise });
  client.attachElmBridge(ports.config);
  if (mode === 'bound') { initialized.resolve(bridgeInternal(h)); await client.localEdits('main'); }
  const first = elmEffect('elm-1');
  ports.emit(first); ports.emit(first);
  ports.emit({ type: 'mutate', databaseId: 'main', requestId: 'named', mutationId: command.id, mutationInput: { order: 2 } });
  const third = client.run('main', { operation: 'mutation', id: command.id }, { order: 3 }, () => {});
  ports.emit(elmEffect('elm-4', 4));
  first.operations[0].input.name = '99';
  initialized.resolve(bridgeInternal(h)); await third;
  const submissions = h.ingress.filter(m => m.type === 'submit');
  expect(submissions.map(m => m.operations[0].input.order ?? Number(m.operations[0].input.name))).toEqual([1, 2, 3, 4]);
  expect(submissions[0].operations[0].prediction.safe).toBe(true);
  expect(submissions[1].operations[0]).not.toHaveProperty('prediction');
  await until(() => ports.events.some(e => e.requestId === 'elm-4' && e.state === 'locallyApplied'));
  expect(ports.errors).toEqual([]);
  client.disconnect(); await h.runtime.ended; await tick();
  expect(ports.events.some(e => e.requestId === 'elm-4' && e.state === 'rejected')).toBe(true);
  expect(ports.events.some(e => e.requestId === 'elm-4' && e.type === 'failure')).toBe(true);
});

test('Elm bridge distinguishes model incarnations while rejecting exact stale effect replays', async () => {
  const h = await ready(), events = [];
  const bridge = elmLocalEdits(() => ({ runtime: h.runtime, operations: all }), event => events.push(event));
  const first = elmEffect('elm:first:1');
  const reset = elmEffect('elm:second:1', 2);
  bridge.forward(first); bridge.forward(first);
  bridge.forward(reset); bridge.forward(reset);
  const submissions = h.ingress.filter(message => message.type === 'submit');
  expect(submissions).toHaveLength(2);
  expect(submissions.map(message => message.operations[0].input.name)).toEqual(['1', '2']);
  bridge.dispose();
});

test('production Elm bridge isolates databases, result-port observers and validation failures', async () => {
  const a = await ready(), b = await ready({ fence: { ...fence, databaseId: 'other', instance: 'other' } });
  const ports = bridgePorts(true, 'customResults');
  const client = await PyreClient.create({ schema, cacheNamespace: 'test', server: { baseUrl: 'https://unused.invalid', localEdits: () => ({}) },
    createInternalClient: async config => bridgeInternal(config.databaseId === 'main' ? a : b) });
  client.attachElmBridge({ ...ports.config, queryResultPort: 'customResults' });
  await client.localEdits('main'); await client.localEdits('other');
  const invalid = elmEffect('bad'); invalid.operations[0].manifest = 'stale';
  expect(() => ports.emit(invalid)).not.toThrow();
  expect(ports.events.filter(e => e.requestId === 'bad').map(e => e.type)).toEqual(['lifecycle', 'failure']);
  expect(a.failures).toMatchObject([{ phase: 'validation', certainty: 'rejected', code: 'InvalidEdit' }]);
  ports.emit(elmEffect('same')); ports.emit(elmEffect('same', 2, 'other')); ports.emit(elmEffect('same'));
  expect(a.ingress.filter(m => m.type === 'submit')).toHaveLength(1);
  expect(b.ingress.filter(m => m.type === 'submit')).toHaveLength(1);
  await until(() => a.writes.length === 1);
  a.accepted(); await until(() => a.reads.length === 2); await a.replace([row], 1);
  const confirmed = ports.events.find(e => e.databaseId === 'main' && e.state === 'confirmed');
  expect(confirmed).toMatchObject({ requestId: 'same', results: [{ index: 0, operation: update.id, value: { id: key } }] });
  expect(ports.events.filter(e => e.databaseId === 'main' && e.requestId === 'same' && e.type === 'lifecycle').map(e => e.state))
    .toEqual(['locallyApplied', 'sent', 'accepted', 'confirmed']);
  client.disconnect(); await Promise.all([a.runtime.ended, b.runtime.ended]); await tick();
  expect(ports.errors).toEqual([]);
});

test('production Elm bridge handles absent ports and failed lazy initialization without parser errors', async () => {
  const ports = bridgePorts();
  const client = await PyreClient.create({ schema, cacheNamespace: 'test', server: { baseUrl: 'https://unused.invalid', localEdits: () => ({}) },
    createInternalClient: async () => { throw Error('initialization failed'); } });
  expect(() => client.attachElmBridge({ app: {} })()).not.toThrow();
  client.attachElmBridge(ports.config); ports.emit(elmEffect('failed')); ports.emit(elmEffect('failed'));
  await until(() => ports.events.length === 2);
  expect(ports.events.map(e => e.type)).toEqual(['lifecycle', 'failure']);
  expect(ports.errors).toEqual([]);
  delete ports.config.app.ports.pyre_receiveQueryDelta;
  client.attachElmBridge(ports.config); ports.emit(elmEffect('missing-result'));
  await tick(); client.disconnect();
});

test('production Elm bridge fences lazy effects on disconnect and does not submit them to a reopened database', async () => {
  const old = await ready(), current = await ready({ fence: { ...fence, instance: 'new' } });
  const initialized = deferred(), ports = bridgePorts(); let creations = 0;
  const client = await PyreClient.create({ schema, cacheNamespace: 'test', server: { baseUrl: 'https://unused.invalid', localEdits: () => ({}) },
    createInternalClient: () => ++creations === 1 ? initialized.promise : Promise.resolve(bridgeInternal(current)) });
  client.attachElmBridge(ports.config); ports.emit(elmEffect('old'));
  client.disconnect(); ports.emit(elmEffect('after-disconnect'));
  expect(ports.events.map(event => [event.type, event.code])).toEqual([['lifecycle', 'Disposed'], ['failure', 'Disposed']]);
  await client.localEdits('main'); initialized.resolve(bridgeInternal(old));
  await until(() => ports.events.some(e => e.type === 'failure'));
  expect(ports.events.every(e => e.requestId === 'old')).toBe(true);
  expect(old.ingress.filter(m => m.type === 'submit')).toEqual([]);
  expect(current.ingress.filter(m => m.type === 'submit')).toEqual([]);
  expect(ports.events).toHaveLength(2);
  client.disconnect();
});

test('Elm bridge consumes scheduled effects only once and disposal rejects unscheduled work', async () => {
  const h = await ready(), events = [], delayed = [];
  const binding = { runtime: h.runtime, operations: all };
  const bridge = elmLocalEdits(() => binding, event => events.push(event), (_database, submit) => {
    delayed.push(submit);
    if (delayed.length === 1) { submit(binding); throw Error('scheduler threw after submitting'); }
  });
  bridge.forward(elmEffect('once'));
  await until(() => events.some(event => event.requestId === 'once'));
  expect(events.some(event => event.state === 'rejected')).toBe(false);
  delayed[0](binding);
  expect(h.ingress.filter(message => message.type === 'submit')).toHaveLength(1);
  bridge.forward(elmEffect('delayed'));
  bridge.stop();
  expect(events.filter(event => event.requestId === 'delayed').map(event => event.type)).toEqual(['lifecycle', 'failure']);
  delayed[1](binding);
  await h.runtime.dispose();
  bridge.dispose();
  expect(events.filter(event => event.requestId === 'delayed')).toHaveLength(2);
  expect(h.ingress.filter(message => message.type === 'submit')).toHaveLength(1);
});

test('single-database production bridge uses its configured descriptors and delivers final disposal events', async () => {
  const mocks = ['init', 'beginLocalEdits', 'replaceAuthoritative'].map(method => spyOn(IndexedDBStorage.prototype, method).mockResolvedValue(undefined));
  const ports = bridgePorts(true), preparations = [];
  const client = await PyreClient.create({ schema, cacheNamespace: 'test', server: { baseUrl: 'https://unused.invalid', localEdits: databaseId => ({
    fence: { ...fence, databaseId }, minimumSafeRevision: 0, operations: all,
    replacement: async request => ({ ...request, type: 'replacement', scope: 'database', complete: true, serverRevision: 0, tables: { users: { rows: [row] } } }),
    prepare: request => { preparations.push(request); return new Promise(() => {}); },
  }) } });
  try {
    const internal = await client.getOrCreateClient('main');
    internal.attachElmBridge(ports.config);
    ports.emit(elmEffect('single')); ports.emit(elmEffect('single'));
    await until(() => preparations.length === 1);
    internal.disconnect(); await internal.getLocalEdits().ended;
    expect(ports.events.some(e => e.requestId === 'single' && e.state === 'rejected')).toBe(true);
    expect(ports.events.some(e => e.requestId === 'single' && e.type === 'failure')).toBe(true);
    expect(ports.errors).toEqual([]);
  } finally { client.disconnect(); await tick(); mocks.forEach(mock => mock.mockRestore()); }
});
