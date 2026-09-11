// @ts-nocheck
// Invoked by tests/elm_local_edits.rs after generating both languages and compiling Elm.
import { test, expect } from 'bun:test';
import { createConnection } from 'node:net';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import { createClient } from '@libsql/client';
import initWasm from '../../../packages/server/wasm/pyre_wasm.js';
import { loadSchemaFromDatabase } from '../../../packages/server/schema';
import { runBatchWithSync, catchupReplacement } from '../../../packages/server/query-sync';
import { LocalEditsRuntime } from '../../../packages/client/src-ts/service/local-edits';
import { elmLocalEdits } from '../../../packages/client/src-ts/service/elm-local-edits';
import { QueryClientService } from '../../../packages/client/src-ts/service/query-client';
import { QueryManagerService } from '../../../packages/client/src-ts/service/query-manager';
import { EntityStreamService } from '../../../packages/client/src-ts/service/entity-stream';
import { planKey } from '@pyre/core/local-edits';

const directory = process.env.PYRE_ELM_EDIT_FIXTURE;
async function until(check) {
  for (let n = 0; n < 500 && !check(); n++) await Bun.sleep(10);
  expect(Boolean(check())).toBe(true);
}
function rust(kind, request, database = 'one') {
  return new Promise((resolve, reject) => {
    const [host, port] = process.env.PYRE_RUST_EDIT_ADDRESS.split(':');
    const socket = createConnection({ host, port: Number(port) });
    socket.setTimeout(5000, () => socket.destroy(new Error('Rust executor timed out')));
    let data = '';
    socket.on('error', reject);
    socket.on('connect', () => socket.write(JSON.stringify({ kind, request, database }) + '\n'));
    socket.on('data', chunk => { data += chunk; if (data.includes('\n')) { socket.end(); resolve(JSON.parse(data)); } });
  });
}

for (const server of ['typescript', 'rust']) test.skipIf(!directory)(
  `${server}: generated TS + Elm effects, compiled worker, SQLite executor and complete replacement`,
  async () => {
    const g = await import(`${directory}/typescript/edits/Main.ts`);
    const { manifest, databases } = await import(`${directory}/typescript/server.ts`);
    const { schemaMetadataByNamespace } = await import(`${directory}/typescript/core/schema.ts`);
    const schema = schemaMetadataByNamespace._default;
    const { meta: readIssues } = await import(`${directory}/typescript/core/queries/metadata/readIssues.ts`);
    await initWasm({ module_or_path: readFileSync(process.env.PYRE_CONFORMANCE_WASM ?? 'packages/server/wasm/pyre_wasm_bg.wasm') });
    const db = createClient({ url: `file:${directory}/${server}.db` });
    await databases._default.ensureDatabase(db);
    await loadSchemaFromDatabase('one', db);
    const epoch = (await db.execute('select database_epoch from _pyre_sync')).rows[0].database_epoch;
    const fence = { databaseId: 'one', instance: 'conformance', authGeneration: 1,
      namespace: g.Main.name, manifest: g.Main.manifest, databaseEpoch: epoch };
    const execute = (kind, request) => server === 'rust' ? rust(kind, request)
      : kind === 'batch' ? runBatchWithSync(db, manifest, fence, request, {})
      : catchupReplacement(db, manifest, fence, request, {});
    const writes = [], replacements = [], publications = [], failures = [];
    const context = vm.createContext({ setTimeout, clearTimeout, console });
    vm.runInContext(readFileSync(`${directory}/worker.js`, 'utf8'), context);
    const worker = context.Elm.Main.init({ flags: { schema, server: { baseUrl: '', catchupPath: '' }, sync: { autoStart: false } } });
    const queries = new QueryClientService(), manager = new QueryManagerService(), entities = new EntityStreamService(schema);
    const queryResults = [], entityBatches = [], readerMismatches = [];
    const runtime = new LocalEditsRuntime({ fence, minimumSafeRevision: 0, operations: g.operations,
      prepare: async request => ({ dispatch: async () => {
        writes.push(request);
        const result = await execute('batch', request);
        if (result.kind === 'success') return result.response;
        if (result.kind === 'unknown') throw new Error('OutcomeUnknown');
        return { ...fence, requestId: request.requestId, status: 'rejected', code: result.error.errorType, operationIndex: result.error.index };
      } }),
      replacement: async request => {
        const result = await execute('replacement', request);
        replacements.push(result);
        if (result.kind !== 'success') throw new Error(JSON.stringify(result));
        return result.response;
      },
    }, { send: message => manager.sendLocalEdits(message),
      install: (publication, changes) => {
        const notifyQueries = queries.installPublication(changes);
        const notifyEntities = entities.installVisible(publication.tables, fence.databaseId);
        publications.push(publication);
        return () => { notifyQueries(); notifyEntities(); };
      }, persist: async () => {} });
    manager.attachPorts(worker);
    manager.setLocalEdits(runtime);
    queries.attachPorts(worker);
    queries.registerQuery({ queryId: 'issues', querySource: readIssues.queryShape, input: {} }, update => {
      queryResults.push(update.result);
      const projected = (entities.getVisibleTables().issues ?? []).map(({ id, title }) => ({ id, title })).sort((a, b) => a.id.localeCompare(b.id));
      const actual = [...(update.result.issue ?? [])].sort((a, b) => a.id.localeCompare(b.id));
      if (JSON.stringify(projected) !== JSON.stringify(actual)) readerMismatches.push({ projected, actual });
    });
    entities.subscribeVisible({ tables: [{ tableName: 'issues' }, { tableName: 'audits' }] }, value => entityBatches.push(value), fence.databaseId);
    runtime.onEditFailure(event => failures.push(event));
    const database = runtime.bind(g.Main);
    const rows = table => publications.at(-1)?.tables[table] ?? [];
    const revision = async () => Number((await db.execute('select server_revision from _pyre_sync')).rows[0].server_revision);
    const submit = async plan => {
      const receipt = database.submit(plan);
      return await Promise.race([receipt.confirmed, Bun.sleep(5000).then(() => { throw new Error(`receipt timeout: ${JSON.stringify({ failures, replacements, publications: publications.slice(-2) })}`); })]);
    };
    let bridge, archiveRuntime, archiveBridge, archiveDb;
    try {
      runtime.start();
      try { await until(() => publications.some(p => !p.invalid)); }
      catch (error) { console.error({ schema, fence, replacements, failures, publications }); throw error; }
      const initialPublications = publications.length;
      expect(await submit(g.batch([]))).toEqual({ kind: 'confirmed', result: [] });
      expect(writes).toHaveLength(0);
      expect(publications).toHaveLength(initialPublications);
      expect(await revision()).toBe(0);

      const id = '00000000-0000-4000-8000-000000000001';
      const related = '00000000-0000-4000-8000-000000000002';
      const created = await submit(g.batch([
        g.Issue.create({ id, title: 'original', owner: 'me', payload: { items: [1, null] }, choice: 'Open' }),
        g.Issue.create({ id: related, title: 'related', owner: 'me', assignee: id }),
      ]));
      expect(created).toMatchObject({ kind: 'confirmed', result: [{ id }, { id: related }] });
      expect(rows('issues').find(row => row.id === related).assignee).toBe(id);
      const original = rows('issues').find(row => row.id === id);
      expect(original.payload).toEqual({ items: [1, null] });
      expect(original.createdAt).toBeGreaterThan(0);
      expect(original.updatedAt).toBeGreaterThan(0);
      expect(await revision()).toBe(1);

      // One generated allowlist/fingerprint, two independent database scopes.
      const a = await import(`${directory}/typescript/edits/Archive.ts`);
      expect(a.Archive.manifest).toBe(g.Main.manifest);
      expect(Object.keys(manifest.replacementContracts).sort()).toEqual(['Archive', '_default']);
      const archiveSchema = schemaMetadataByNamespace.Archive;
      expect(Object.keys(archiveSchema.tables)).toEqual(['archiveEntries']);
      expect(Object.keys(schema.tables).sort()).toEqual(['audits', 'issues']);
      archiveDb = createClient({ url: `file:${directory}/${server}-archive.db` });
      await databases.Archive.ensureDatabase(archiveDb);
      await loadSchemaFromDatabase('archive', archiveDb);
      const archiveFence = { ...fence, databaseId: 'archive', namespace: 'Archive', databaseEpoch: (await archiveDb.execute('select database_epoch from _pyre_sync')).rows[0].database_epoch };
      const archiveExecute = (kind, request) => server === 'rust' ? rust(kind, request, 'archive')
        : kind === 'batch' ? runBatchWithSync(archiveDb, manifest, archiveFence, request, {})
        : catchupReplacement(archiveDb, manifest, archiveFence, request, {});
      const archiveWorker = context.Elm.Main.init({ flags: { schema: archiveSchema, server: { baseUrl: '', catchupPath: '' }, sync: { autoStart: false } } });
      const archivePublications = [], archiveReads = [], archiveQueries = [], archiveFailures = [];
      const archiveEntities = new EntityStreamService(archiveSchema);
      const archiveQueryClient = new QueryClientService();
      archiveQueryClient.attachPorts(archiveWorker);
      archiveRuntime = new LocalEditsRuntime({ fence: archiveFence, minimumSafeRevision: 0, operations: a.operations,
        prepare: async request => ({ dispatch: async () => {
          const result = await archiveExecute('batch', request);
          if (result.kind !== 'success') throw new Error(JSON.stringify(result));
          return result.response;
        } }), replacement: async request => {
          const result = await archiveExecute('replacement', request);
          if (result.kind !== 'success') console.error('archive replacement', result);
          expect(result.kind).toBe('success');
          archiveReads.push(result.response);
          return result.response;
        },
      }, { send: message => archiveWorker.ports.receiveQueryManagerMessage.send(message),
        install: (publication, changes) => {
          const notifyQuery = archiveQueryClient.installPublication(changes);
          const notifyEntities = archiveEntities.installVisible(publication.tables, 'archive');
          archivePublications.push(publication);
          return () => { notifyQuery(); notifyEntities(); };
        }, persist: async () => {} });
      archiveWorker.ports.queryManagerOut.subscribe(message => { if (message.type === 'localEdits') archiveRuntime.receiveEnvelope(message); });
      const { meta: readArchive } = await import(`${directory}/typescript/core/queries/metadata/readArchive.ts`);
      archiveQueryClient.registerQuery({ queryId: 'archive', querySource: readArchive.queryShape, input: {} }, update => archiveQueries.push(update.result));
      archiveRuntime.start();
      archiveRuntime.onEditFailure(value => archiveFailures.push(value));
      try { await until(() => archivePublications.some(p => !p.invalid)); }
      catch (error) { console.error(JSON.stringify({ archiveSchema, archiveReads, archivePublications, archiveFailures })); throw error; }
      expect(await archiveRuntime.bind(a.Archive).submit(a.ArchiveEntry.create({ id, title: 'archive ts', state: 'Stored' })).confirmed).toMatchObject({ kind: 'confirmed', result: { id } });
      expect(rows('issues').find(row => row.id === id).title).toBe('original');
      const archiveContext = vm.createContext({ setTimeout, clearTimeout, console });
      vm.runInContext(readFileSync(`${directory}/archive.js`, 'utf8'), archiveContext);
      const archiveApp = archiveContext.Elm.ArchiveConformance.init();
      const archiveObserved = [];
      archiveApp.ports.observed.subscribe(value => archiveObserved.push(value));
      archiveBridge = elmLocalEdits(databaseId => databaseId === 'archive' ? { runtime: archiveRuntime, operations: a.operations } : undefined, event => archiveApp.ports.incoming.send(event));
      archiveApp.ports.effectOut.subscribe(effect => archiveBridge.forward(effect));
      await until(() => archiveObserved.length === 1);
      expect(archiveObserved).toEqual([id]);
      expect(archiveQueries.at(-1)).toEqual({ archiveEntry: [{ id, title: 'archive elm' }] });
      expect(archiveEntities.getVisibleTables().archiveEntries[0].title).toBe('archive elm');
      expect(rows('issues').find(row => row.id === id).title).toBe('original');
      expect(await revision()).toBe(1);
      expect(archiveReads.every(read => Object.keys(read.tables).join() === 'archiveEntries')).toBe(true);
      expect(await submit(a.ArchiveEntry.update(id, { title: 'wrong scope' }))).toMatchObject({ kind: 'rejected' });
      const archiveOperation = a.ArchiveEntry.update(id, { title: 'wrong scope' })[planKey].operations[0];
      expect((await execute('batch', { ...fence, version: 1, requestId: 'cross-operation', sequence: 100, operations: [{ operation: archiveOperation.definition.id, input: archiveOperation.input }] })).kind).toBe('error');
      const mainOperation = g.Issue.update(id, { title: 'wrong scope' })[planKey].operations[0];
      expect((await archiveExecute('batch', { ...archiveFence, version: 1, requestId: 'cross-operation', sequence: 100, operations: [{ operation: mainOperation.definition.id, input: mainOperation.input }] })).kind).toBe('error');
      expect((await archiveExecute('replacement', { ...fence, version: 1, requestId: 'cross-replacement', target: 0 })).kind).toBe('error');
      expect((await execute('replacement', { ...archiveFence, version: 1, requestId: 'cross-replacement', target: 0 })).kind).toBe('error');
      expect(await databases.Archive.ensureDatabase(archiveDb)).toBe('up-to-date');

      const uppercase = 'ABCDEFAB-CDEF-0123-4567-ABCDEFABCDEF';
      expect(await submit(g.Issue.create({ id: uppercase, title: 'case preserved', owner: 'me', watchers: [uppercase], dueAt: '2026-01-01T00:00:00Z' })))
        .toMatchObject({ kind: 'confirmed', result: { id: uppercase } });
      expect(rows('issues').find(row => row.id === uppercase)).toMatchObject({ id: uppercase, watchers: [uppercase], dueAt: 1767225600 });
      expect(await submit(g.Issue.delete(uppercase))).toMatchObject({ kind: 'confirmed' });
      const baselineRevision = await revision();

      // Both client builders and server transaction validation must reject, not
      // strip protected or malformed values and accidentally commit a prefix.
      const beforeInvalid = writes.length;
      for (const patch of [{ owner: 'other' }, { title: null }, { payload: { items: ['bad'] } }, { watchers: ['not-a-uuid'] }, { assignee: 'not-a-uuid' }, {}]) {
        expect(await submit(g.Issue.update(id, patch))).toMatchObject({ kind: 'rejected' });
      }
      expect(writes).toHaveLength(beforeInvalid);
      const operation = g.Issue.update(id, { title: 'valid' })[planKey].operations[0].definition.id;
      for (const input of [{ id, owner: 'other', title: 'bad' }, { id, title: null }, { id, payload: { items: ['bad'] } }, { id, watchers: ['bad'] }, { id, assignee: 'bad' }]) {
        const request = { ...fence, version: 1, requestId: 'raw-invalid', sequence: 100,
          operations: [{ operation, input: { id, title: 'prefix' } }, { operation, input }] };
        expect((await execute('batch', request)).kind).toBe('error');
        expect(await revision()).toBe(baselineRevision);
      }
      const createOperation = g.Issue.create({ id, title: 'valid', owner: 'me' })[planKey].operations[0].definition.id;
      for (const invalidId of ['symbolic', id.replaceAll('-', ''), `{${id}}`, `${id}\n`, id.replace('4', 'g')]) {
        expect(() => g.Issue.create({ id, title: 'valid', owner: 'me' })[planKey].operations[0].definition.decodeResult({ id: invalidId })).toThrow();
        expect(await submit(g.Issue.create({ id: invalidId, title: 'invalid', owner: 'me' }))).toMatchObject({ kind: 'rejected' });
        expect((await execute('batch', { ...fence, version: 1, requestId: 'invalid-uuid', sequence: 101,
          operations: [{ operation: createOperation, input: { id: invalidId, title: 'invalid', owner: 'me' } }] })).kind).toBe('error');
        expect(await revision()).toBe(baselineRevision);
      }

      // Strict permission/missing cardinality rolls back earlier writes and
      // replacement never leaves a rejected UUID create as a ghost.
      for (const failure of [g.Issue.create({ id: crypto.randomUUID(), title: 'forbidden', owner: 'other' }),
        g.Issue.update(crypto.randomUUID(), { title: 'missing' })]) {
        const ghost = crypto.randomUUID();
        const beforeRollback = publications.length;
        expect(await submit(g.batch([g.Issue.create({ id: ghost, title: 'prefix', owner: 'me' }), failure])))
          .toMatchObject({ kind: 'rejected', code: 'TargetNotWritable' });
        expect(rows('issues').some(row => row.id === ghost)).toBe(false);
        expect(publications.slice(beforeRollback).some(p => p.tables.issues?.some(row => row.id === ghost))).toBe(false);
        expect(Number((await db.execute({ sql: 'select count(*) as n from issues where id = ?', args: [ghost] })).rows[0].n)).toBe(0);
        expect(await revision()).toBe(baselineRevision);
      }

      // The actual generated Elm application emits an effect and decodes the
      // authoritative integer/named result through Pyre.update and Pyre.outcome.
      const elmContext = vm.createContext({ setTimeout, clearTimeout, console });
      vm.runInContext(readFileSync(`${directory}/test.js`, 'utf8'), elmContext);
      const app = elmContext.Elm.Test.init();
      const observed = [], events = [];
      app.ports.observed.subscribe(value => observed.push(value));
      bridge = elmLocalEdits(dbId => dbId === 'one' ? { runtime, operations: g.operations } : undefined,
        event => { events.push(event); app.ports.incoming.send(event); });
      app.ports.effectOut.subscribe(effect => { expect(bridge.forward(effect)).toBe(true); bridge.forward(effect); });
      await until(() => observed.length === 1);
      expect(observed[0].auditId).toBe(1);
      expect(observed[0].timestamps).toHaveLength(1);
      expect(observed[0].timestamps[0]).toBeGreaterThan(0);
      expect(rows('issues').find(row => row.id === id)).toMatchObject({ title: 'elm', assignee: null });
      expect(rows('audits').map(row => row.id).sort()).toEqual([1, 2]);
      expect(events.filter(e => e.state === 'confirmed')).toHaveLength(1);

      const completed = [];
      app.ports.completed.subscribe(value => completed.push(value));
      const elmAction = async (action, state = 'confirmed') => {
        const before = completed.length;
        app.ports.perform.send(action);
        await until(() => completed.length > before);
        expect(completed.at(-1)).toEqual({ action, state });
      };
      const elmId = '00000000-0000-4000-8000-000000000010';
      await elmAction('create');
      expect(rows('issues').find(row => row.id === elmId).assignee).toBe(id);
      const validWrites = writes.length;
      for (const action of ['invalidUuid', 'invalidStructured', 'emptyUpdate']) await elmAction(action, 'rejected');
      expect(writes).toHaveLength(validWrites);
      await elmAction('nullable');
      expect(rows('issues').find(row => row.id === elmId)).toMatchObject({ assignee: null, payload: { items: [3, null] }, watchers: [uppercase], dueAt: 1767225600 });
      await elmAction('delete');
      expect(rows('issues').some(row => row.id === elmId)).toBe(false);
      const elmRevision = await revision();
      await elmAction('rollback', 'rejected');
      expect(rows('issues').some(row => row.id === elmId)).toBe(false);
      expect(Number((await db.execute({ sql: 'select count(*) as n from issues where id = ?', args: [elmId] })).rows[0].n)).toBe(0);
      expect(await revision()).toBe(elmRevision);
      const elmWrites = writes.length, elmPublications = publications.length;
      await elmAction('empty');
      expect(writes).toHaveLength(elmWrites);
      expect(publications).toHaveLength(elmPublications);
      expect(await revision()).toBe(elmRevision);

      // No temporary integer ID, and a later submission uses the real ID.
      const auditReceipt = database.submit(g.Audit.create({ message: 'integer' }));
      expect(rows('audits').map(row => row.id).sort()).toEqual([1, 2]);
      const audit = await auditReceipt.confirmed;
      expect(audit).toMatchObject({ kind: 'confirmed', result: { id: 3 } });
      expect(await submit(g.Audit.update(audit.result.id, { message: 'updated' }))).toMatchObject({ kind: 'confirmed' });
      expect(rows('audits').find(row => row.id === 3).message).toBe('updated');
      await db.execute("create trigger normalize_title after update of title on issues when new.title != lower(new.title) begin update issues set title = lower(new.title) where id = new.id; end");
      expect(await submit(g.Issue.update(id, { title: 'SERVER NORMALIZED' }))).toMatchObject({ kind: 'confirmed' });
      expect(rows('issues').find(row => row.id === id).title).toBe('server normalized');
      expect(await submit(g.Issue.update(id, { payload: null, choice: null }))).toMatchObject({ kind: 'confirmed' });
      expect(rows('issues').find(row => row.id === id)).toMatchObject({ payload: null, choice: null });
      expect(await submit(g.batch([g.Issue.delete(related), g.Issue.delete(id), g.Audit.delete(audit.result.id)]))).toMatchObject({ kind: 'confirmed' });
      expect(rows('issues')).toEqual([]);
      expect(archiveEntities.getVisibleTables().archiveEntries[0]).toMatchObject({ id, title: 'archive elm' });
      expect(Number((await archiveDb.execute('select server_revision from _pyre_sync')).rows[0].server_revision)).toBe(2);
      expect(queryResults.at(-1)).toEqual({ issue: [] });
      expect(entityBatches.some(batch => batch.changes.some(change => change.op === 'remove' && change.id === id))).toBe(true);
      expect(readerMismatches).toEqual([]);
      expect(queryResults.length).toBeGreaterThan(2);
      expect(rows('audits').some(row => row.id === 3)).toBe(false);

      const named = await submit(g.Commands.namedAudit({ message: 'typescript named result' }));
      expect(named.kind).toBe('confirmed');
      expect(named.result.audit[0].message).toBe('typescript named result');
      expect(named.result.audit[0].updatedAt).toBeInstanceOf(Date);
      expect(Number.isSafeInteger(named.result.audit[0].id)).toBe(true);

      const current = await revision();
      const last = writes.at(-1);
      for (const change of [{ databaseId: 'two' }, { namespace: 'Archive' }, { databaseEpoch: 'stale' }, { manifest: 'stale' }, { instance: 'old' }, { authGeneration: 0 }]) {
        expect((await execute('batch', { ...last, ...change })).kind).toBe('error');
        expect((await execute('replacement', { ...fence, version: 1, requestId: 'fenced', target: current, ...change })).kind).toBe('error');
        expect(await revision()).toBe(current);
      }
      expect(publications.at(-1).coveredRevision).toBe(current);
      expect(replacements.every(r => r.kind === 'success' && r.response.complete && r.response.scope === 'database')).toBe(true);
      expect(replacements.length).toBeGreaterThan(1);
      expect(failures.filter(f => f.certainty === 'unknown')).toEqual([]);

      // Legacy malformed stored identities are rejected, never silently repaired.
      await db.execute("insert into issues (id, title, owner) values ('legacy-key', 'legacy', 'me')");
      const badStored = await execute('replacement', { ...fence, version: 1, requestId: 'legacy', target: current });
      expect(badStored.kind).toBe('error');
      expect((await db.execute("select id from issues where id = 'legacy-key'")).rows[0].id).toBe('legacy-key');
    } finally { await runtime.dispose(); await archiveRuntime?.dispose(); archiveBridge?.dispose(); archiveDb?.close(); bridge?.dispose(); manager.detach(); queries.detach(); entities.clear(); db.close(); }
  }, 30_000,
);
