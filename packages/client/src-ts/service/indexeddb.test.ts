// @ts-nocheck
import { expect, test } from 'bun:test';

import { IndexedDbService, type SyncCursor } from './indexeddb';

test('IndexedDbService restores persisted sync cursor with initial data', async () => {
  const persistedCursor: SyncCursor = {
    tables: {
      maps: {
        last_seen_updated_at: 10,
        last_seen_primary_key: '00000000-0000-0000-0000-000000000001',
        permission_hash: 'perm-hash',
      },
    },
  };

  const sentMessages: unknown[] = [];
  let serverRevision: number | null = 7;
  let handleIndexedDbOut: ((message: unknown) => void | Promise<void>) | null = null;

  const storage = {
    init: async () => undefined,
    getAllTables: async () => ({ maps: [] }),
    getSyncCursor: async () => persistedCursor,
    getServerRevision: async () => serverRevision,
    getDatabaseEpoch: async () => 'persisted-epoch',
    putSyncCursor: async (cursor: SyncCursor) => {
      Object.assign(persistedCursor, cursor);
    },
    putServerRevision: async (revision: number) => {
      serverRevision = revision;
    },
    putRows: async () => undefined,
  };

  const service = new IndexedDbService(storage as never);
  service.attachPorts({
    ports: {
      indexedDbOut: {
        subscribe: (callback) => {
          handleIndexedDbOut = callback;
        },
      },
      receiveIndexedDbMessage: {
        send: (message) => {
          sentMessages.push(message);
        },
      },
    },
  });

  if (!handleIndexedDbOut) {
    throw new Error('indexedDbOut handler was not attached');
  }

  const indexedDbOut = handleIndexedDbOut;

  indexedDbOut({
    type: 'writeSyncCursor',
    cursor: persistedCursor,
  });
  await Bun.sleep(0);

  indexedDbOut({ type: 'requestInitialData' });
  await Bun.sleep(0);

  expect(sentMessages).toEqual([
    {
      type: 'initialData',
      data: {
        tables: { maps: [] },
        cursor: persistedCursor,
        lastAppliedServerRevision: 7,
        databaseEpoch: 'persisted-epoch',
      },
    },
  ]);
});

test('IndexedDbService forwards catchup entity deltas after cache writes', async () => {
  let handleIndexedDbOut: ((message: unknown) => void | Promise<void>) | null = null;
  const entityDeltas: unknown[] = [];
  const operations: string[] = [];
  const storage = {
    init: async () => undefined,
    getAllTables: async () => ({}),
    getSyncCursor: async () => ({ tables: {} }),
    getServerRevision: async () => null,
    getDatabaseEpoch: async () => null,
    putSyncCursor: async () => undefined,
    putServerRevision: async () => undefined,
    putRows: async () => {
      operations.push('write');
    },
  };

  const service = new IndexedDbService(storage as never, undefined, (tableGroups, source) => {
    operations.push('notify');
    entityDeltas.push({ tableGroups, source });
  });
  service.attachPorts({
    ports: {
      indexedDbOut: {
        subscribe: (callback) => {
          handleIndexedDbOut = callback;
        },
      },
    },
  });

  if (!handleIndexedDbOut) {
    throw new Error('indexedDbOut handler was not attached');
  }

  const tableGroups = [{ table_name: 'maps', headers: ['id'], rows: [[1]] }];
  handleIndexedDbOut({ type: 'writeDelta', entityStreamSource: 'live', tableGroups });
  handleIndexedDbOut({ type: 'writeDelta', entityStreamSource: 'catchup', tableGroups });
  await Bun.sleep(0);

  expect(entityDeltas).toEqual([{ tableGroups, source: 'catchup' }]);
  expect(operations).toEqual(['write', 'write', 'notify']);
});

test('IndexedDbService acknowledges an atomic database epoch reset', async () => {
  let handleIndexedDbOut: ((message: unknown) => void | Promise<void>) | null = null;
  const sentMessages: unknown[] = [];
  const resets: string[] = [];
  let callbackCount = 0;
  const storage = {
    resetForDatabaseEpoch: async (epoch: string) => {
      resets.push(epoch);
    },
  };
  const service = new IndexedDbService(storage as never, undefined, undefined, () => {
    callbackCount += 1;
  });
  service.attachPorts({
    ports: {
      indexedDbOut: { subscribe: (callback) => { handleIndexedDbOut = callback; } },
      receiveIndexedDbMessage: { send: (message) => sentMessages.push(message) },
    },
  });

  handleIndexedDbOut?.({ type: 'resetForDatabaseEpoch', databaseEpoch: 'new-epoch' });
  await Bun.sleep(0);

  expect(resets).toEqual(['new-epoch']);
  expect(callbackCount).toBe(1);
  expect(sentMessages).toEqual([
    { type: 'databaseEpochResetCompleted', databaseEpoch: 'new-epoch' },
  ]);
});
