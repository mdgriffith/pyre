// @ts-nocheck
import { expect, spyOn, test } from 'bun:test';
import { PyreClient } from './index';
import { IndexedDBStorage } from './service/indexeddb';

const schema = { tables: {
  maps: { name: 'maps', primaryKey: { name: 'id', kind: 'int' }, links: {}, indices: [] },
}, queryFieldToTable: {} };
const group = { table_name: 'maps', headers: ['id'], rows: [[1]] };

test('production live path validates before revision persistence and still forwards to Elm', async () => {
  const mocks = [
    spyOn(IndexedDBStorage.prototype, 'init').mockResolvedValue(undefined),
    spyOn(IndexedDBStorage.prototype, 'getAllTables').mockResolvedValue({}),
    spyOn(IndexedDBStorage.prototype, 'getSyncCursor').mockResolvedValue({ tables: {} }),
    spyOn(IndexedDBStorage.prototype, 'getServerRevision').mockResolvedValue(7),
    spyOn(IndexedDBStorage.prototype, 'getDatabaseEpoch').mockResolvedValue('epoch'),
    spyOn(console, 'error').mockImplementation(() => {}),
  ];
  const persist = spyOn(IndexedDBStorage.prototype, 'putServerRevision').mockResolvedValue(undefined);
  const client = await PyreClient.create({ schema, server: { baseUrl: 'http://example.test' }, cacheNamespace: 'validation' });
  try {
    const internal = await client.getOrCreateClient('main');
    await new Promise((resolve) => setTimeout(resolve, 10));
    const forwarded = [];
    internal.sseManager.elmApp = { ports: { receiveSSEMessage: { send: (message) => forwarded.push(message) } } };
    const batches = [];
    for (const subscribed of [false, true]) {
      const unsubscribe = subscribed ? internal.entityStream.subscribe({ tables: [{ tableName: 'maps' }] }, (batch) => batches.push(batch)) : () => {};
      for (const data of [
        [group, { ...group, rows: [['bad']] }],
        [group, { ...group, table_name: 'unknown', rows: [] }],
        [group, null],
        [{ ...group, rows: [[]] }],
        [{ ...group, headers: ['id', 'id'], rows: [[1, 1]] }],
        [group, group],
        [{ ...group, rows: [[1], [1]] }],
      ]) {
        const message = { type: 'delta', databaseId: 'main', databaseEpoch: 'epoch', serverRevision: 99, data };
        expect(() => internal.sseManager.emitMessage(message)).not.toThrow();
        expect(forwarded.at(-1)).toBe(message);
        expect(internal.lastAppliedServerRevision).toBe(7);
        expect(persist).not.toHaveBeenCalled();
        expect(batches).toEqual([]);
      }
      unsubscribe();
    }
    internal.sseManager.emitMessage({ type: 'delta', databaseId: 'main', serverRevision: 8, data: [group] });
    expect(internal.lastAppliedServerRevision).toBe(8);
    expect(persist).toHaveBeenCalledWith(8);
  } finally {
    client.disconnect();
    persist.mockRestore();
    mocks.forEach((mock) => mock.mockRestore());
  }
});

test('invalid cache rejects production initialization without loading independent progress', async () => {
  const invalid = new Error('Invalid persisted identity for table maps');
  const revision = spyOn(IndexedDBStorage.prototype, 'getServerRevision').mockResolvedValue(99);
  const epoch = spyOn(IndexedDBStorage.prototype, 'getDatabaseEpoch').mockResolvedValue('old');
  const mocks = [revision, epoch,
    spyOn(IndexedDBStorage.prototype, 'init').mockResolvedValue(undefined),
    spyOn(IndexedDBStorage.prototype, 'getAllTables').mockRejectedValue(invalid),
    spyOn(console, 'error').mockImplementation(() => {}),
  ];
  const client = await PyreClient.create({ schema, server: { baseUrl: 'http://example.test' }, cacheNamespace: 'invalid' });
  try {
    await expect(client.getOrCreateClient('main')).rejects.toBe(invalid);
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(revision).not.toHaveBeenCalled();
    expect(epoch).not.toHaveBeenCalled();
  } finally {
    client.disconnect();
    mocks.forEach((mock) => mock.mockRestore());
  }
});

test.each(['public', 'bridge'])('legacy %s named calls never publish independent optimistic entity rows', async path => {
  const mocks = [
    spyOn(IndexedDBStorage.prototype, 'init').mockResolvedValue(undefined),
    spyOn(IndexedDBStorage.prototype, 'getAllTables').mockResolvedValue({}),
    spyOn(IndexedDBStorage.prototype, 'getAllRows').mockResolvedValue([{ id: 1, name: 'base' }]),
    spyOn(IndexedDBStorage.prototype, 'getSyncCursor').mockResolvedValue({ tables: {} }),
    spyOn(IndexedDBStorage.prototype, 'getServerRevision').mockResolvedValue(null),
    spyOn(IndexedDBStorage.prototype, 'getDatabaseEpoch').mockResolvedValue(null),
    spyOn(IndexedDBStorage.prototype, 'putServerRevision').mockResolvedValue(undefined),
  ];
  const client = await PyreClient.create({ schema, server: { baseUrl: 'http://example.test' }, cacheNamespace: 'legacy' });
  try {
    const internal = await client.getOrCreateClient('main');
    await new Promise(resolve => setTimeout(resolve, 10));
    const batches = [], outcomes = [], pending = [];
    internal.entityStream.subscribe({ tables: [{ tableName: 'maps' }] }, batch => batches.push(batch));
    mocks.push(spyOn(internal.queryManager, 'sendMutation').mockImplementation((...args) => pending.push(args[5])));
    const optimistic = { queryField: 'maps', where: { field: 'id', input: 'id' }, set: [{ field: 'name', input: 'name' }] };
    const invoke = async () => {
      if (path === 'public') await client.run('main', { operation: 'mutation', id: 'rename', optimistic }, { id: 1, name: 'local' }, result => outcomes.push(result));
      else internal.runBridgeMutation({ databaseId: 'main', requestId: 'bridge', mutationId: 'rename', mutationInput: { id: 1, name: 'local' }, optimistic }, { send: message => outcomes.push(message.result) });
      await new Promise(resolve => setTimeout(resolve, 10));
    };
    await invoke();
    expect(pending).toHaveLength(1);
    expect(batches).toEqual([]);
    pending.shift()({ ok: false, error: 'TargetNotWritable' });
    expect(batches).toEqual([]);
    expect(outcomes[0]).toEqual({ ok: false, error: 'TargetNotWritable' });
    await invoke();
    expect(batches).toEqual([]);
    pending.shift()({ ok: true, value: { serverRevision: 1, sync: { type: 'delta', databaseId: 'main', data: [{ table_name: 'maps', headers: ['id', 'name'], rows: [[1, 'server']] }] } } });
    expect(batches).toHaveLength(1);
    expect(batches[0].source).toBe('mutation-response');
    expect(batches[0].changes[0].row).toEqual({ id: 1, name: 'server' });
  } finally {
    client.disconnect();
    mocks.forEach(mock => mock.mockRestore());
  }
});
