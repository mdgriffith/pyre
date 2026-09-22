import { operation, PyreClient } from '../src-ts/index';
import { IndexedDBStorage } from '../src-ts/service/indexeddb';

const who = location.pathname.slice(1) || 'a';
const schema = { tables: { notes: { name: 'notes', links: {}, indices: [{ field: 'noteKey', primary: true, unique: true }] } }, queryFieldToTable: { notes: 'notes' } };
const client = await PyreClient.create({ schema, cacheNamespace: who, server: { baseUrl: `${location.origin}/${who}`, endpoints: { events: '/events' } } });
const batches: any[] = [];
const results: any[] = [];
let rows: any[] = [];
let connected = false;
client.onSyncState((state) => { connected = state.status === 'live'; });
await client.onEntityChanges('proof', { tables: [{ tableName: 'notes' }] }, (batch) => batches.push(batch));
await client.run('proof', { operation: 'query', queryShape: { notes: { noteKey: true, id: true, title: true, updatedAt: true } } }, {}, (result: any) => { rows = result.notes; });
await client.syncDatabase('proof');
const storage = new IndexedDBStorage(client.getInternalIndexedDbName('proof'), { notes: 'noteKey' });
Object.assign(window, {
  proof: {
    batches, results,
    get rows() { return rows; },
    get connected() { return connected; },
    async batch(inputs: Array<{ id: string; title: string }>) {
      const edits = inputs.map(input => operation({ operation: 'update', id: 'edit', optimistic: {
        queryField: 'notes', where: { field: 'noteKey', input: 'id' }, set: [{ field: 'title', input: 'title' }],
      } }, input));
      const result = await client.submit('proof', edits);
      results.push(result);
      return result;
    },
    edit(title: string) {
      return client.run('proof', { operation: 'update', id: 'edit', optimistic: {
        queryField: 'notes', where: { field: 'noteKey', input: 'id' }, set: [{ field: 'title', input: 'title' }],
      } }, { id: '00000000-0000-7000-8000-000000000001', title }, (result) => results.push(result));
    },
    async late() {
      const received: any[] = [];
      const off = await client.onEntityChanges('proof', { tables: [{ tableName: 'notes' }] }, (batch) => received.push(batch));
      off();
      return received;
    },
    persisted: () => storage.getAllRows('notes'),
    async verifyRemovalPersistence() {
      const cache = new IndexedDBStorage('pyre-removal-proof', { notes: 'noteKey' });
      const id = '00000000-0000-7000-8000-000000000003';
      const row = [{ table_name: 'notes', headers: ['noteKey', 'id', 'title'], rows: [[id, 'ordinary', 'Old']] }];
      await cache.putAuthoritativeDelta(row, 1);
      await cache.putAuthoritativeDelta([{ table_name: 'notes', headers: ['noteKey', '_pyre_removed'], rows: [[id, true]] }], 3);
      const reloaded = new IndexedDBStorage('pyre-removal-proof', { notes: 'noteKey' });
      await reloaded.putAuthoritativeDelta(row, 2);
      return { rows: await reloaded.getAllRows('notes'), stamps: await reloaded.getRowRevisions() };
    },
    async verifyIdentityMigration() {
      const name = 'pyre-uuid-upgrade-proof';
      await new Promise<void>((resolve, reject) => {
        const request = indexedDB.open(name, 2);
        request.onupgradeneeded = () => {
          const db = request.result;
          db.createObjectStore('tables', { keyPath: ['tableName', 'id'] });
          db.createObjectStore('syncCursor');
          db.createObjectStore('meta');
        };
        request.onerror = () => reject(request.error);
        request.onsuccess = () => {
          const db = request.result;
          const tx = db.transaction(['tables', 'syncCursor', 'meta'], 'readwrite');
          tx.objectStore('tables').put({ tableName: 'notes', id: 1, title: 'Legacy' });
          tx.objectStore('syncCursor').put({ tables: { notes: { last_seen_primary_key: 1, last_seen_updated_at: 99, permission_hash: 'old' } } }, 'cursor');
          tx.objectStore('meta').put(99, 'lastAppliedServerRevision');
          tx.objectStore('meta').put({ '["notes",1]': 99 }, 'rowRevisions');
          tx.oncomplete = () => { db.close(); resolve(); };
          tx.onerror = () => reject(tx.error);
        };
      });
      const migrated = new IndexedDBStorage(name);
      const before = { rows: await migrated.getAllTables(), revision: await migrated.getServerRevision(), stamps: await migrated.getRowRevisions(), cursor: await migrated.getSyncCursor() };
      const id = '00000000-0000-7000-8000-000000000001';
      const group = (title: string) => [{ table_name: 'notes', headers: ['title', 'id'], rows: [[title, id]] }];
      await migrated.putAuthoritativeDelta(group('Current'), 2);
      await migrated.putAuthoritativeDelta(group('Stale'), 1);
      return { before, rows: await migrated.getAllRows('notes'), stamps: await migrated.getRowRevisions() };
    },
  },
});
