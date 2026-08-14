// @ts-nocheck
import { expect, test } from 'bun:test';

import { PyreClient } from './index';
import {
  __resetPyreDevtoolsRegistryForTests,
  getPyreDevtoolsRegistrySnapshot,
  getPyreDevtoolsInstanceSnapshot,
} from './devtools-registry';

const schema = {
  tables: {},
  queryFieldToTable: {},
};

const server = {
  baseUrl: 'https://api.example.test',
};

function fakeInternalClient(disconnects: string[], databaseId: string, starts: string[] = []) {
  const syncStateCallbacks: Array<(state: { status: string; tables: Record<string, string> }) => void> = [];
  return {
    run(receivedDatabaseId: string) {
      if (receivedDatabaseId !== databaseId) {
        throw new Error(`expected ${databaseId}, got ${receivedDatabaseId}`);
      }
      return undefined;
    },
    disconnect() {
      disconnects.push(databaseId);
    },
    startSync() {
      starts.push(databaseId);
    },
    onSyncState(callback: (state: { status: string; tables: Record<string, string> }) => void) {
      syncStateCallbacks.push(callback);
      callback({ status: 'not_started', tables: {} });
      return () => {};
    },
    onEntityChanges(_subscription: unknown, _callback: (batch: unknown) => void) {
      return () => {
        disconnects.push(`entity:${databaseId}`);
      };
    },
    onDevtoolsEvent() {
      return () => {};
    },
    emitLive() {
      syncStateCallbacks.forEach((callback) => callback({ status: 'live', tables: {} }));
    },
    async getDevtoolsSnapshot() {
      return {
        tables: {
          messages: {
            name: 'messages',
            count: databaseId.length,
            sync: 'live',
          },
        },
      };
    },
    async inspectDevtoolsTablePage(request: { offset?: number; limit?: number }) {
      return {
        rows: [{ databaseId }],
        offset: request.offset ?? 0,
        limit: request.limit ?? 100,
        hasMore: false,
      };
    },
  };
}

function fakePort() {
  let callback: ((message: unknown) => void) | null = null;
  const sent: unknown[] = [];
  return {
    sent,
    port: {
      subscribe(next: (message: unknown) => void) {
        callback = next;
      },
      unsubscribe() {
        callback = null;
      },
      send(message: unknown) {
        sent.push(message);
      },
    },
    emit(message: unknown) {
      callback?.(message);
    },
  };
}

test('PyreClient requires cacheNamespace', async () => {
  await expect(PyreClient.create({
    schema,
    server,
  })).rejects.toThrow('PyreClient.create requires cacheNamespace');
});

test('PyreClient creates one internal client per databaseId', async () => {
  const createdConfigs: any[] = [];
  const disconnects: string[] = [];
  const client = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    indexedDbName: 'pyre-client',
    createInternalClient: async (config) => {
      createdConfigs.push(config);
      return fakeInternalClient(disconnects, config.databaseId);
    },
  });

  await client.run('campaign:123', { operation: 'insert', id: 'CreateNote' }, {}, () => {});
  await client.run('campaign:456', { operation: 'insert', id: 'CreateNote' }, {}, () => {});
  await client.run('campaign:123', { operation: 'insert', id: 'CreateNote' }, {}, () => {});

  expect(createdConfigs.map((config) => config.databaseId)).toEqual([
    'campaign:123',
    'campaign:456',
  ]);
  expect(client.getInternalDatabaseIds()).toEqual(['campaign:123', 'campaign:456']);
  expect(createdConfigs.every((config) => config.autoStartSync === false)).toBe(true);

  client.disconnect();
  await Bun.sleep(0);
  expect(disconnects.sort()).toEqual(['campaign:123', 'campaign:456']);
});

test('PyreClient routes entity change subscriptions by databaseId', async () => {
  const createdConfigs: any[] = [];
  const disconnects: string[] = [];
  const client = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => {
      createdConfigs.push(config);
      return fakeInternalClient(disconnects, config.databaseId);
    },
  });

  const unsubscribe = await client.onEntityChanges(
    'campaign:123',
    { tables: [{ tableName: 'posts' }] },
    () => {}
  );
  unsubscribe();

  expect(createdConfigs.map((config) => config.databaseId)).toEqual(['campaign:123']);
  expect(disconnects).toEqual(['entity:campaign:123']);
});

test('PyreClient rejects invalid entity subscriptions before creating an internal client', async () => {
  const createdConfigs: any[] = [];
  const client = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => {
      createdConfigs.push(config);
      return fakeInternalClient([], config.databaseId);
    },
  });

  await expect(client.onEntityChanges('campaign:123', { tables: [] }, () => {})).rejects.toThrow('at least one table');
  expect(createdConfigs).toHaveLength(0);
});

test('PyreClient forwards initial entity batches from the internal client', async () => {
  const batches: unknown[] = [];
  const client = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => ({
      ...fakeInternalClient([], config.databaseId),
      onEntityChanges(_subscription: unknown, callback: (batch: unknown) => void) {
        callback({
          type: 'entity-change-batch',
          databaseId: config.databaseId,
          sequence: 1,
          source: 'indexeddb-initial',
          changes: [{ tableName: 'posts', id: 1, op: 'row', row: { id: 1 } }],
        });
        return () => {};
      },
    }),
  });

  await client.onEntityChanges('campaign:123', { tables: [{ tableName: 'posts' }] }, (batch) => {
    batches.push(batch);
  });

  expect(batches).toEqual([
    {
      type: 'entity-change-batch',
      databaseId: 'campaign:123',
      sequence: 1,
      source: 'indexeddb-initial',
      changes: [{ tableName: 'posts', id: 1, op: 'row', row: { id: 1 } }],
    },
  ]);
});

test('PyreClient derives readable separate IndexedDB names per databaseId', async () => {
  const client = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user:42',
    indexedDbName: 'pyre-client',
    createInternalClient: async (config) => fakeInternalClient([], config.databaseId),
  });

  const campaign123 = client.getInternalIndexedDbName('campaign:123');
  const campaign456 = client.getInternalIndexedDbName('campaign:456');

  expect(campaign123).toStartWith('pyre-client:user_42:campaign_123_');
  expect(campaign456).toStartWith('pyre-client:user_42:campaign_456_');
  expect(campaign123).not.toBe(campaign456);
});

test('setSyncedDatabases preserves order and dedupes databaseIds', async () => {
  const createdConfigs: any[] = [];
  const client = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => {
      createdConfigs.push(config);
      return fakeInternalClient([], config.databaseId);
    },
  });

  await client.setSyncedDatabases(['main', 'campaign:123', 'main', 'campaign:456']);

  expect(client.getSyncedDatabaseIds()).toEqual(['main', 'campaign:123', 'campaign:456']);
  expect(createdConfigs.map((config) => config.databaseId)).toEqual(['main', 'campaign:123', 'campaign:456']);
});

test('setSyncedDatabases reuses existing clients and disconnects removed databases', async () => {
  const createdConfigs: any[] = [];
  const disconnects: string[] = [];
  const client = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => {
      createdConfigs.push(config);
      return fakeInternalClient(disconnects, config.databaseId);
    },
  });

  await client.setSyncedDatabases(['main', 'campaign:123']);
  await client.setSyncedDatabases(['campaign:123', 'campaign:456']);
  await Bun.sleep(0);

  expect(client.getSyncedDatabaseIds()).toEqual(['campaign:123', 'campaign:456']);
  expect(createdConfigs.map((config) => config.databaseId)).toEqual(['main', 'campaign:123', 'campaign:456']);
  expect(disconnects).toEqual(['main']);
  expect(client.getInternalDatabaseIds()).toEqual(['campaign:123', 'campaign:456']);
});

test('syncDatabase and unsyncDatabase update active set incrementally', async () => {
  const disconnects: string[] = [];
  const client = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => fakeInternalClient(disconnects, config.databaseId),
  });

  await client.syncDatabase('main');
  await client.syncDatabase('campaign:123');
  await client.syncDatabase('main');
  await client.unsyncDatabase('main');
  await Bun.sleep(0);

  expect(client.getSyncedDatabaseIds()).toEqual(['campaign:123']);
  expect(disconnects).toEqual(['main']);
});

test('setSyncedDatabases rejects missing databaseIds', async () => {
  const client = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => fakeInternalClient([], config.databaseId),
  });

  await expect(client.setSyncedDatabases(['main', ''])).rejects.toThrow('databaseId is required');
});

test('setSyncedDatabases starts sync one database at a time in order', async () => {
  const starts: string[] = [];
  const clients = new Map<string, ReturnType<typeof fakeInternalClient>>();
  const client = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => {
      const internalClient = fakeInternalClient([], config.databaseId, starts);
      clients.set(config.databaseId, internalClient);
      return internalClient;
    },
  });

  await client.setSyncedDatabases(['main', 'campaign:123', 'campaign:456']);
  await Bun.sleep(0);

  expect(starts).toEqual(['main']);

  clients.get('main')?.emitLive();
  await Bun.sleep(0);
  expect(starts).toEqual(['main', 'campaign:123']);

  clients.get('campaign:123')?.emitLive();
  await Bun.sleep(0);
  expect(starts).toEqual(['main', 'campaign:123', 'campaign:456']);
});

test('setSyncedDatabases does not start removed pending databases', async () => {
  const starts: string[] = [];
  const clients = new Map<string, ReturnType<typeof fakeInternalClient>>();
  const client = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => {
      const internalClient = fakeInternalClient([], config.databaseId, starts);
      clients.set(config.databaseId, internalClient);
      return internalClient;
    },
  });

  await client.setSyncedDatabases(['main', 'campaign:123']);
  await Bun.sleep(0);
  await client.setSyncedDatabases(['main']);
  clients.get('main')?.emitLive();
  await Bun.sleep(0);

  expect(starts).toEqual(['main']);
});

test('removing active database disconnects it and starts next eligible database', async () => {
  const starts: string[] = [];
  const disconnects: string[] = [];
  const client = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => fakeInternalClient(disconnects, config.databaseId, starts),
  });

  await client.setSyncedDatabases(['main', 'campaign:123']);
  await Bun.sleep(0);
  await client.setSyncedDatabases(['campaign:123']);
  await Bun.sleep(0);

  expect(starts).toEqual(['main', 'campaign:123']);
  expect(disconnects).toEqual(['main']);
});

test('stale live events from removed clients are ignored', async () => {
  const starts: string[] = [];
  const clients = new Map<string, ReturnType<typeof fakeInternalClient>>();
  const client = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => {
      const internalClient = fakeInternalClient([], config.databaseId, starts);
      clients.set(config.databaseId, internalClient);
      return internalClient;
    },
  });

  await client.setSyncedDatabases(['main', 'campaign:123', 'campaign:456']);
  await Bun.sleep(0);
  await client.setSyncedDatabases(['campaign:456']);
  await Bun.sleep(0);
  clients.get('main')?.emitLive();
  await Bun.sleep(0);

  expect(starts).toEqual(['main', 'campaign:456']);
});

test('reordering pending databases changes the next sync priority', async () => {
  const starts: string[] = [];
  const clients = new Map<string, ReturnType<typeof fakeInternalClient>>();
  const client = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => {
      const internalClient = fakeInternalClient([], config.databaseId, starts);
      clients.set(config.databaseId, internalClient);
      return internalClient;
    },
  });

  await client.setSyncedDatabases(['main', 'campaign:123', 'campaign:456']);
  await Bun.sleep(0);
  await client.setSyncedDatabases(['main', 'campaign:456', 'campaign:123']);
  clients.get('main')?.emitLive();
  await Bun.sleep(0);

  expect(starts).toEqual(['main', 'campaign:456']);
});

test('Elm bridge routes register messages by databaseId', async () => {
  const receivedRuns: any[] = [];
  const outbound = fakePort();
  const results = fakePort();
  await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => ({
      ...fakeInternalClient([], config.databaseId),
      run(databaseId: string, queryModule: any, input: unknown, callback: (result: unknown) => void) {
        receivedRuns.push({ databaseId, queryModule, input });
        callback({ rows: [databaseId] });
        return {
          update() {},
          unsubscribe() {},
        };
      },
    }),
    elm: {
      app: {
        ports: {
          pyreStoreOut: outbound.port,
          pyre_receiveQueryDelta: results.port,
        },
      },
    },
  });

  outbound.emit({
    type: 'register',
    databaseId: 'campaign:123',
    queryId: 'q1',
    queryName: 'CampaignNotes',
    querySource: { note: {} },
    queryInput: { limit: 10 },
  });
  await Bun.sleep(0);

  expect(receivedRuns).toEqual([
    {
      databaseId: 'campaign:123',
      queryModule: { operation: 'query', queryShape: { note: {} } },
      input: { limit: 10 },
    },
  ]);
  expect(results.sent).toEqual([
    {
      type: 'full',
      queryId: 'q1',
      queryName: 'CampaignNotes',
      revision: expect.any(Number),
      result: { rows: ['campaign:123'] },
    },
  ]);
});

test('Elm bridge routes mutation messages by databaseId', async () => {
  const receivedRuns: any[] = [];
  const outbound = fakePort();
  const results = fakePort();
  await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => ({
      ...fakeInternalClient([], config.databaseId),
      run(databaseId: string, queryModule: any, input: unknown, callback: (result: unknown) => void) {
        receivedRuns.push({ databaseId, queryModule, input });
        callback({ kind: 'success' });
      },
    }),
    elm: {
      app: {
        ports: {
          pyreStoreOut: outbound.port,
          pyre_receiveMutationResult: results.port,
        },
      },
    },
  });

  outbound.emit({
    type: 'mutate',
    databaseId: 'campaign:123',
    requestId: 'm1',
    mutationId: 'CreateNote',
    mutationName: 'CreateNote',
    mutationInput: { body: 'Hello' },
  });
  await Bun.sleep(0);

  expect(receivedRuns).toEqual([
    {
      databaseId: 'campaign:123',
      queryModule: { operation: 'mutation', id: 'CreateNote' },
      input: { body: 'Hello' },
    },
  ]);
  expect(results.sent).toEqual([
    {
      type: 'mutation-result',
      requestId: 'm1',
      mutationId: 'CreateNote',
      mutationName: 'CreateNote',
      result: { kind: 'success' },
    },
  ]);
});

test('Elm bridge routes entity stream registrations and batches by streamId', async () => {
  const receivedSubscriptions: any[] = [];
  const unsubscribed: string[] = [];
  const outbound = fakePort();
  const entityChanges = fakePort();
  await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => ({
      ...fakeInternalClient([], config.databaseId),
      onEntityChanges(subscription: unknown, callback: (batch: unknown) => void) {
        receivedSubscriptions.push({ databaseId: config.databaseId, subscription });
        callback({
          type: 'entity-change-batch',
          databaseId: config.databaseId,
          sequence: 1,
          source: 'indexeddb-initial',
          changes: [{ tableName: 'posts', id: 1, op: 'row', row: { id: 1, author_id: 10 } }],
        });
        return () => {
          unsubscribed.push(config.databaseId);
        };
      },
    }),
    elm: {
      app: {
        ports: {
          pyreStoreOut: outbound.port,
          pyre_receiveEntityChanges: entityChanges.port,
        },
      },
    },
  });

  outbound.emit({
    type: 'register-entity-stream',
    databaseId: 'campaign:123',
    streamId: 'visible-posts',
    tables: [{ tableName: 'posts', where: { id: { $in: [1, 2] } } }],
  });
  await Bun.sleep(0);

  expect(receivedSubscriptions).toEqual([
    {
      databaseId: 'campaign:123',
      subscription: { tables: [{ tableName: 'posts', where: { id: { $in: [1, 2] } } }] },
    },
  ]);
  expect(entityChanges.sent).toEqual([
    {
      type: 'entity-change-batch',
      streamId: 'visible-posts',
      databaseId: 'campaign:123',
      sequence: 1,
      source: 'indexeddb-initial',
      changes: [{ tableName: 'posts', id: 1, op: 'row', row: { id: 1, author_id: 10 } }],
    },
  ]);

  outbound.emit({ type: 'unregister-entity-stream', streamId: 'visible-posts' });
  await Bun.sleep(0);

  expect(unsubscribed).toEqual(['campaign:123']);
});

test('Elm bridge treats exact duplicate entity stream registrations as a no-op', async () => {
  const receivedSubscriptions: unknown[] = [];
  const unsubscribed: string[] = [];
  const outbound = fakePort();
  await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => ({
      ...fakeInternalClient([], config.databaseId),
      onEntityChanges(subscription: unknown) {
        receivedSubscriptions.push(subscription);
        return () => {
          unsubscribed.push(config.databaseId);
        };
      },
    }),
    elm: {
      app: {
        ports: {
          pyreStoreOut: outbound.port,
        },
      },
    },
  });

  const registration = {
    type: 'register-entity-stream',
    databaseId: 'campaign:123',
    streamId: 'visible-posts',
    tables: [{ tableName: 'posts' }],
  };
  outbound.emit(registration);
  await Bun.sleep(0);
  outbound.emit(registration);
  await Bun.sleep(0);

  expect(receivedSubscriptions).toEqual([{ tables: [{ tableName: 'posts' }] }]);
  expect(unsubscribed).toEqual([]);
});

test('Elm bridge replaces a changed entity stream registration with the same streamId', async () => {
  const unsubscribed: string[] = [];
  const outbound = fakePort();
  await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => ({
      ...fakeInternalClient([], config.databaseId),
      onEntityChanges() {
        return () => {
          unsubscribed.push(config.databaseId);
        };
      },
    }),
    elm: {
      app: {
        ports: {
          pyreStoreOut: outbound.port,
        },
      },
    },
  });

  outbound.emit({
    type: 'register-entity-stream',
    databaseId: 'campaign:123',
    streamId: 'visible-posts',
    tables: [{ tableName: 'posts' }],
  });
  await Bun.sleep(0);
  outbound.emit({
    type: 'register-entity-stream',
    databaseId: 'campaign:456',
    streamId: 'visible-posts',
    tables: [{ tableName: 'posts' }],
  });
  await Bun.sleep(0);

  expect(unsubscribed).toEqual(['campaign:123']);
});

test('Elm bridge reports invalid entity stream registrations', async () => {
  const errors: Array<{ message: string; phase: string }> = [];
  const outbound = fakePort();
  await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => fakeInternalClient([], config.databaseId),
    elm: {
      app: {
        ports: {
          pyreStoreOut: outbound.port,
        },
      },
      onError(error, context) {
        errors.push({ message: error.message, phase: context.phase });
      },
    },
  });

  outbound.emit({
    type: 'register-entity-stream',
    databaseId: 'campaign:123',
    streamId: 'visible-posts',
    tables: [{ tableName: 'posts', where: { id: { $gt: 1 } } }],
  });
  await Bun.sleep(0);

  expect(errors).toEqual([
    { message: 'Entity subscription table posts where.id uses unsupported operator $gt', phase: 'incoming-message' },
  ]);
});

test('devtools registry tracks public clients and disambiguates duplicate namespaces', async () => {
  __resetPyreDevtoolsRegistryForTests();
  const first = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => fakeInternalClient([], config.databaseId),
  });
  const second = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => fakeInternalClient([], config.databaseId),
  });

  expect((await getPyreDevtoolsRegistrySnapshot()).instances.map((instance) => instance.label)).toEqual(['user_42', 'user_42 (2)']);

  first.disconnect();
  expect((await getPyreDevtoolsRegistrySnapshot()).instances.map((instance) => instance.label)).toEqual(['user_42']);
  second.disconnect();
  expect((await getPyreDevtoolsRegistrySnapshot()).instances).toEqual([]);
});

test('devtools snapshots keep known databases after unsync and classify scheduler state', async () => {
  __resetPyreDevtoolsRegistryForTests();
  const starts: string[] = [];
  const clients = new Map<string, ReturnType<typeof fakeInternalClient>>();
  const client = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => {
      const internalClient = fakeInternalClient([], config.databaseId, starts);
      clients.set(config.databaseId, internalClient);
      return internalClient;
    },
  });

  await client.setSyncedDatabases(['main', 'campaign:123']);
  const instanceId = (await getPyreDevtoolsRegistrySnapshot()).instances[0].instanceId;
  let snapshot = await getPyreDevtoolsInstanceSnapshot(instanceId);
  expect(snapshot?.databases.map((database) => ({ id: database.databaseId, lifecycle: database.lifecycle, flagged: database.flaggedForSync }))).toEqual([
    { id: 'main', lifecycle: 'syncing', flagged: true },
    { id: 'campaign:123', lifecycle: 'queued', flagged: true },
  ]);

  await client.unsyncDatabase('main');
  snapshot = await getPyreDevtoolsInstanceSnapshot(instanceId);
  expect(snapshot?.databases.map((database) => database.databaseId)).toEqual(['main', 'campaign:123']);
  expect(snapshot?.databases.find((database) => database.databaseId === 'main')?.lifecycle).toBe('unsynced');
  expect(snapshot?.databases.find((database) => database.databaseId === 'campaign:123')?.lifecycle).toBe('syncing');

  clients.get('campaign:123')?.emitLive();
  await Bun.sleep(0);
  snapshot = await getPyreDevtoolsInstanceSnapshot(instanceId);
  expect(snapshot?.databases.find((database) => database.databaseId === 'campaign:123')?.lifecycle).toBe('live');
});

test('devtools mutation events include database metadata and retain newest events', async () => {
  __resetPyreDevtoolsRegistryForTests();
  const client = await PyreClient.create({
    schema,
    server,
    cacheNamespace: 'user_42',
    createInternalClient: async (config) => ({
      ...fakeInternalClient([], config.databaseId),
      run(databaseId: string, _queryModule: any, input: unknown, callback: (result: unknown) => void) {
        callback({ ok: input !== 'fail', value: input, error: input === 'fail' ? 'nope' : undefined });
      },
    }),
  });
  const instanceId = (await getPyreDevtoolsRegistrySnapshot()).instances[0].instanceId;

  for (let index = 0; index < 205; index += 1) {
    await client.run('main', { operation: 'mutation', id: `Mutation${index}` }, index, () => {});
  }
  await client.run('secondary', { operation: 'mutation', id: 'FailMutation' }, 'fail', () => {});

  const snapshot = await getPyreDevtoolsInstanceSnapshot(instanceId);
  expect(snapshot?.events).toHaveLength(200);
  expect(snapshot?.events[0]).toMatchObject({
    actor: 'APP',
    operation: 'mutation.failed',
    level: 'error',
    summary: '[pyre] APP mutation.failed secondary mutation=FailMutation duration=0ms error=nope',
    type: '[pyre] APP mutation.failed secondary mutation=FailMutation duration=0ms error=nope',
  });
  expect(snapshot?.events[0].payload).toMatchObject({
    instanceId,
    databaseId: 'secondary',
    mutationId: 'FailMutation',
    input: 'fail',
    error: 'nope',
  });
  expect(snapshot?.events.some((event) => event.operation === 'mutation.started' && (event.payload as any).mutationId === 'Mutation0')).toBe(false);
});
