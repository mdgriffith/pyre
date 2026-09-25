import { PyreClient, type BoundEphemeralStateSnapshot } from '@pyre/client';

interface Connection {
  userId: number;
  cursor: string | null;
  label: string;
}

interface ConnectionPatch {
  cursor?: string | null;
  label?: string;
}

interface Shared {
  marker: string;
  count: number;
}

interface SharedPatch {
  marker?: string;
  count?: number;
}

// This is the same structural bundle emitted by generated typescript/core/state.ts.
interface StateTypes {
  connection: Connection;
  connectionPatch: ConnectionPatch;
  shared: Shared;
  sharedPatch: SharedPatch;
}

type Snapshot = BoundEphemeralStateSnapshot<StateTypes>;

const params = new URLSearchParams(location.search);
const name = params.get('name') ?? 'client';
const readOnly = params.get('readOnly') === 'true';
const server = params.get('server');
if (!server) throw new Error('Missing server URL');

// Durable existence is inspected directly by the coordinator. Keeping the browser
// schema empty makes this proof exercise only the public ephemeral client surface.
const schema = { tables: {}, queryFieldToTable: {} };
const client = await PyreClient.create({
  schema,
  cacheNamespace: `ephemeral-proof-${name}`,
  server: {
    baseUrl: server,
    ephemeralWrite: !readOnly,
    ephemeralMaxUpdateCadenceMs: 0,
    ephemeralLeaseCadenceMs: 2_000,
  },
});
let syncStatus = 'not_started';
client.onSyncState((state) => { syncStatus = state.status; });
const history: Snapshot[] = [];
await client.subscribeEphemeralState<StateTypes>('proof', (snapshot) => {
  history.push(structuredClone(snapshot));
});
await client.syncDatabase('proof');

async function scanIndexedDb(): Promise<unknown[]> {
  const databases = await indexedDB.databases();
  const values: unknown[] = [];
  for (const database of databases) {
    if (!database.name) continue;
    const opened = await new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open(database.name!);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    try {
      for (const storeName of Array.from(opened.objectStoreNames)) {
        values.push(...await new Promise<unknown[]>((resolve, reject) => {
          const transaction = opened.transaction(storeName, 'readonly');
          const request = transaction.objectStore(storeName).getAll();
          request.onsuccess = () => resolve(request.result);
          request.onerror = () => reject(request.error);
        }));
      }
    } finally {
      opened.close();
    }
  }
  return values;
}

Object.assign(window, {
  proof: {
    history,
    syncStatus: () => syncStatus,
    snapshot: () => client.getEphemeralState<StateTypes>('proof'),
    updateConnection: (patch: ConnectionPatch) => client.updateEphemeralConnection<StateTypes>('proof', patch),
    updateShared: (patch: SharedPatch) => client.updateEphemeralShared<StateTypes>('proof', patch),
    async invalidConnection() {
      try {
        await client.updateEphemeralConnection<StateTypes>('proof', { cursor: 42 } as unknown as ConnectionPatch);
        return { accepted: true };
      } catch (error) {
        return {
          accepted: false,
          name: error instanceof Error ? error.name : typeof error,
          message: error instanceof Error ? error.message : String(error),
          outcome: error && typeof error === 'object' && 'outcome' in error ? error.outcome : null,
        };
      }
    },
    async rejectedReadOnly() {
      try {
        await client.updateEphemeralConnection<StateTypes>('proof', { cursor: 'EPHEMERAL_READ_ONLY' });
        return { accepted: true };
      } catch (error) {
        return {
          accepted: false,
          name: error instanceof Error ? error.name : typeof error,
          message: error instanceof Error ? error.message : String(error),
          outcome: error && typeof error === 'object' && 'outcome' in error ? error.outcome : null,
        };
      }
    },
    scanIndexedDb,
  },
});
