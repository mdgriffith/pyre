import { operation, PyreClient } from '../src-ts/index';
import { IndexedDBStorage } from '../src-ts/service/indexeddb';

const who = location.pathname.slice(1) || 'a';
const schema = { tables: { notes: { name: 'notes', links: {}, indices: [] } }, queryFieldToTable: { notes: 'notes' } };
const client = await PyreClient.create({ schema, cacheNamespace: who, server: { baseUrl: `${location.origin}/${who}`, endpoints: { events: '/events' } } });
const batches: any[] = [];
const results: any[] = [];
let rows: any[] = [];
let connected = false;
client.onSyncState((state) => { connected = state.status === 'live'; });
await client.onEntityChanges('proof', { tables: [{ tableName: 'notes' }] }, (batch) => batches.push(batch));
await client.run('proof', { operation: 'query', queryShape: { notes: { id: true, title: true, updatedAt: true } } }, {}, (result: any) => { rows = result.notes; });
await client.syncDatabase('proof');
const storage = new IndexedDBStorage(client.getInternalIndexedDbName('proof'));
Object.assign(window, {
  proof: {
    batches, results,
    get rows() { return rows; },
    get connected() { return connected; },
    async batch(inputs: Array<{ id: number; title: string }>) {
      const edits = inputs.map(input => operation({ operation: 'update', id: 'edit', optimistic: {
        queryField: 'notes', where: { field: 'id', input: 'id' }, set: [{ field: 'title', input: 'title' }],
      } }, input));
      const result = await client.submit('proof', edits);
      results.push(result);
      return result;
    },
    edit(title: string) {
      return client.run('proof', { operation: 'update', id: 'edit', optimistic: {
        queryField: 'notes', where: { field: 'id', input: 'id' }, set: [{ field: 'title', input: 'title' }],
      } }, { id: 1, title }, (result) => results.push(result));
    },
    async late() {
      const received: any[] = [];
      const off = await client.onEntityChanges('proof', { tables: [{ tableName: 'notes' }] }, (batch) => received.push(batch));
      off();
      return received;
    },
    persisted: () => storage.getAllRows('notes'),
  },
});
