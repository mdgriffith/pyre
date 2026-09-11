// @ts-nocheck
import { expect, test } from 'bun:test';

// Optional native-browser suite, without adding a client runtime dependency.
// PYRE_PLAYWRIGHT_MODULE=/absolute/path/to/playwright/index.mjs bun test ...
const playwrightModule = process.env.PYRE_PLAYWRIGHT_MODULE;

test.skipIf(!playwrightModule)('native IndexedDB identity persistence, reload, v2 upgrade and epoch reset', async () => {
  const { chromium } = await import(playwrightModule);
  const build = await Bun.build({ entrypoints: [new URL('./indexeddb.ts', import.meta.url).pathname], target: 'browser' });
  expect(build.success).toBe(true);
  const script = await build.outputs[0].text();
  const server = Bun.serve({ port: 0, hostname: '127.0.0.1', fetch(request) {
    return new URL(request.url).pathname === '/storage.js'
      ? new Response(script, { headers: { 'Content-Type': 'text/javascript' } })
      : new Response('<!doctype html><title>Identity persistence test</title>', { headers: { 'Content-Type': 'text/html' } });
  } });
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage();
    const schema = { tables: {
      issues: { name: 'issues', primaryKey: { name: 'issueKey', kind: 'uuid' }, links: {}, indices: [] },
      other: { name: 'other', primaryKey: { name: 'key', kind: 'uuid' }, links: {}, indices: [] },
      audits: { name: 'audits', primaryKey: { name: 'sequence', kind: 'int' }, links: {}, indices: [] },
    }, queryFieldToTable: {} };
    const uuid = '12345678-1234-1234-1234-123456789abc';
    const row = { issueKey: uuid, id: 'ordinary id', tableName: 'domain value', identity: 'domain identity', updatedAt: 2, title: 'new', parent: uuid };
    await page.goto(server.url.href);
    const result = await page.evaluate(async ({ schema, row, uuid }) => {
      const { IndexedDBStorage } = await import('/storage.js');
      const storage = new IndexedDBStorage('main', schema);
      await storage.putRows('issues', [{ ...row, updatedAt: 1, title: 'original' }]);
      await storage.putRows('issues', [row]);
      const stale = await storage.putRows('issues', [{ ...row, title: 'stale', updatedAt: 1 }]);
      await storage.putRows('other', [{ key: uuid }]);
      await storage.putRows('audits', [{ sequence: 1, updatedAt: 2 }, { sequence: -2 }]);
      const errors = [];
      for (const invalid of ['1', 1.5, null, Number.MAX_SAFE_INTEGER + 1]) {
        try { await storage.putRows('audits', [{ sequence: 99 }, { sequence: invalid }]); }
        catch (error) { errors.push(error.message); }
      }
      for (const invalid of ['1', 1, null, 'not-a-uuid']) {
        try { await storage.putRows('issues', [{ issueKey: invalid }]); }
        catch (error) { errors.push(error.message); }
      }
      const otherDb = new IndexedDBStorage('secondary', schema);
      await otherDb.putRows('issues', [{ ...row, title: 'other database' }]);
      await storage.putSyncCursor({ tables: { issues: { last_seen_updated_at: 2, last_seen_primary_key: uuid, permission_hash: 'p' } } });
      await storage.putServerRevision(7);
      await storage.putDatabaseEpoch('e1');
      return { stale, errors, rows: await storage.getAllRows('issues'), count: await storage.countRows('audits'), page: await storage.getRowsPage('audits', 0, 1) };
    }, { schema, row, uuid });
    expect(result.stale.skippedOlder).toBe(1);
    expect(result.errors).toHaveLength(8);
    expect(result.rows).toEqual([row]);
    expect(result.count).toBe(2);
    expect(result.page).toEqual({ rows: [{ sequence: -2 }], hasMore: true });

    await page.reload();
    const reloaded = await page.evaluate(async ({ schema }) => {
      const { IndexedDBStorage } = await import('/storage.js');
      const storage = new IndexedDBStorage('main', schema);
      const otherDb = new IndexedDBStorage('secondary', schema);
      return { tables: await storage.getAllTables(), other: await otherDb.getAllRows('issues'), cursor: await storage.getSyncCursor(), revision: await storage.getServerRevision(), epoch: await storage.getDatabaseEpoch() };
    }, { schema });
    expect(reloaded.tables.issues).toEqual([row]);
    expect(reloaded.tables.other).toEqual([{ key: uuid }]);
    expect(reloaded.tables.audits).toEqual([{ sequence: -2 }, { sequence: 1, updatedAt: 2 }]);
    expect(reloaded.other[0].title).toBe('other database');
    expect(reloaded.cursor.tables.issues.last_seen_primary_key).toBe(uuid);
    expect(reloaded.revision).toBe(7);
    expect(reloaded.epoch).toBe('e1');

    await page.reload();
    const invalidReload = await page.evaluate(async ({ schema }) => {
      const { IndexedDBStorage, IndexedDbService } = await import('/storage.js');
      const changedSchema = structuredClone(schema);
      changedSchema.tables.issues.primaryKey = { name: 'issueKey', kind: 'int' };
      const storage = new IndexedDBStorage('main', changedSchema);
      const service = new IndexedDbService(storage);
      const sent = [];
      let receive;
      service.attachPorts({ ports: {
        indexedDbOut: { subscribe: (callback) => { receive = callback; } },
        receiveIndexedDbMessage: { send: (message) => sent.push(message) },
      } });
      receive({ type: 'requestInitialData' });
      let error;
      try { await service.initialize(); } catch (failure) { error = failure.message; }
      await new Promise((resolve) => setTimeout(resolve, 0));
      return { error, sent, revision: await storage.getServerRevision(), epoch: await storage.getDatabaseEpoch(), cursor: await storage.getSyncCursor() };
    }, { schema });
    expect(invalidReload.error).toContain('Invalid int identity');
    expect(invalidReload.sent).toEqual([]);
    // Explicit failure preserves the cache, but never resumes from its progress.
    expect(invalidReload.revision).toBe(7);
    expect(invalidReload.epoch).toBe('e1');
    expect(invalidReload.cursor).toEqual(reloaded.cursor);

    const reset = await page.evaluate(async ({ schema }) => {
      const { IndexedDBStorage } = await import('/storage.js');
      const storage = new IndexedDBStorage('main', schema);
      await storage.resetForDatabaseEpoch('e2');
      return { tables: await storage.getAllTables(), cursor: await storage.getSyncCursor(), revision: await storage.getServerRevision(), epoch: await storage.getDatabaseEpoch(), other: await new IndexedDBStorage('secondary', schema).countRows('issues') };
    }, { schema });
    expect(reset).toEqual({ tables: {}, cursor: { tables: {} }, revision: null, epoch: 'e2', other: 1 });

    for (const version of [1, 2]) {
      const upgraded = await page.evaluate(async ({ schema, version, row }) => {
        const name = `legacy-${version}`;
        await new Promise((resolve, reject) => {
          const request = indexedDB.open(name, version);
          request.onupgradeneeded = () => {
            const db = request.result;
            const tables = db.createObjectStore('tables', { keyPath: ['tableName', 'id'] });
            tables.createIndex('byTable', 'tableName');
            tables.createIndex('byUpdatedAt', 'updatedAt');
            tables.put({ tableName: 'issues', id: 1, title: 'legacy' });
            db.createObjectStore('syncCursor').put({ tables: { issues: { last_seen_updated_at: 999 } } }, 'cursor');
            if (version === 2) {
              const meta = db.createObjectStore('meta');
              meta.put(999, 'lastAppliedServerRevision');
              meta.put('old-epoch', 'databaseEpoch');
            }
          };
          request.onsuccess = () => { request.result.close(); resolve(); };
          request.onerror = () => reject(request.error);
        });
        const { IndexedDBStorage } = await import('/storage.js');
        const storage = new IndexedDBStorage(name, schema);
        const before = { tables: await storage.getAllTables(), cursor: await storage.getSyncCursor(), revision: await storage.getServerRevision(), epoch: await storage.getDatabaseEpoch() };
        await storage.putRows('issues', [row]);
        return { before, after: await storage.getAllRows('issues'), version: (await storage.init()).version };
      }, { schema, version, row });
      expect(upgraded).toEqual({ before: { tables: {}, cursor: { tables: {} }, revision: null, epoch: null }, after: [row], version: 3 });
    }
  } finally {
    await browser.close();
    server.stop(true);
  }
}, 30000);
