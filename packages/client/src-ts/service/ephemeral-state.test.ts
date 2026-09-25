// @ts-nocheck
import { expect, test } from 'bun:test';

import { EphemeralStateService, EphemeralUpdateError } from './ephemeral-state';

function harness(options = {}) {
  let now = 0;
  let nextTimer = 1;
  const timers = new Map();
  const requests = [];
  const reconnects = [];
  const fetch = (url, init) => new Promise((resolve, reject) => {
    const request = {
      url,
      init,
      body: JSON.parse(init.body),
      accept(extra = {}) {
        resolve(new Response(JSON.stringify({
          type: 'ephemeralAccepted',
          operation: url.includes('connection') ? 'connectionPatch'
            : url.includes('shared') ? 'sharedPatch'
              : url.includes('resnapshot') ? 'resnapshot' : 'leaseRefresh',
          clientRequestSequence: request.body.clientRequestSequence,
          ephemeralEpoch: request.body.ephemeralEpoch,
          revision: 10,
          ...extra,
        })));
      },
      reject(error = 'denied') {
        resolve(new Response(JSON.stringify({
          type: 'ephemeralRejected',
          clientRequestSequence: request.body.clientRequestSequence,
          error,
        }), { status: 403 }));
      },
      fail(error = new Error('offline')) {
        reject(error);
      },
    };
    init.signal.addEventListener('abort', () => reject(new Error('aborted')));
    requests.push(request);
  });
  const service = new EphemeralStateService({
    databaseId: options.databaseId ?? 'alpha',
    baseUrl: 'https://api.example.test/root',
    endpoints: {
      connection: '/ephemeral/connection',
      shared: '/ephemeral/shared',
      lease: '/ephemeral/lease',
      resnapshot: '/ephemeral/resnapshot',
    },
    maxUpdateCadenceMs: options.maxUpdateCadenceMs ?? 20,
    leaseCadenceMs: options.leaseCadenceMs ?? 100,
    requestTimeoutMs: options.requestTimeoutMs ?? 80,
    ephemeralWrite: options.ephemeralWrite,
    resolveHeaders: options.resolveHeaders,
    requestTransportReconnect: () => reconnects.push(now),
    fetch,
    now: () => now,
    setTimeout(callback, delay) {
      const id = nextTimer++;
      timers.set(id, { at: now + delay, callback });
      return id;
    },
    clearTimeout(id) {
      timers.delete(id);
    },
  });

  function advance(milliseconds) {
    const target = now + milliseconds;
    while (true) {
      const due = [...timers.entries()]
        .filter(([, timer]) => timer.at <= target)
        .sort((left, right) => left[1].at - right[1].at)[0];
      if (!due) break;
      timers.delete(due[0]);
      now = due[1].at;
      due[1].callback();
    }
    now = target;
  }

  function connect(connectionId = 'connection-1', epoch = 'epoch-1', revision = 1, values = {}) {
    service.handleMessage({
      type: 'connected', databaseId: options.databaseId ?? 'alpha', connectionId,
      ephemeralEpoch: epoch, ephemeralCapability: `capability-${connectionId}`,
    });
    service.handleMessage({
      type: 'ephemeralSnapshot',
      ephemeralSnapshot: {
        databaseId: options.databaseId ?? 'alpha', epoch, revision,
        shared: values.shared ?? { count: 0 },
        connections: values.connections ?? { [connectionId]: { cursor: null } },
      },
    });
  }

  return { service, requests, reconnects, advance, connect, timers };
}

const tick = async () => {
  for (let index = 0; index < 8; index += 1) await Promise.resolve();
};

test('invokes configured host timers with a valid global receiver instead of the service', () => {
  const receivers = [];
  const service = new EphemeralStateService({
    databaseId: 'alpha',
    baseUrl: 'https://api.example.test',
    endpoints: {
      connection: '/ephemeral/connection', shared: '/ephemeral/shared',
      lease: '/ephemeral/lease', resnapshot: '/ephemeral/resnapshot',
    },
    setTimeout: function () { receivers.push(this); return 1; },
    clearTimeout: function () { receivers.push(this); },
  });
  service.handleMessage({
    type: 'connected', databaseId: 'alpha', connectionId: 'one',
    ephemeralEpoch: 'epoch', ephemeralCapability: 'capability-one',
  });
  service.handleMessage({
    type: 'ephemeralSnapshot',
    ephemeralSnapshot: { databaseId: 'alpha', epoch: 'epoch', revision: 1, shared: {}, connections: {} },
  });
  service.dispose();
  expect(receivers).toEqual([globalThis, globalThis]);
});

test('snapshot and ordered changes replace complete entries and process removals', () => {
  const { service, connect } = harness();
  connect('mine', 'epoch-1', 4, { connections: { mine: { nested: { x: 1 }, old: true }, peer: { value: 1 } } });
  service.handleMessage({
    type: 'ephemeralChanges',
    ephemeralChanges: {
      databaseId: 'alpha', epoch: 'epoch-1', revision: 5,
      connections: { mine: { nested: { y: 2 } } }, removedConnections: ['peer'],
    },
  });
  expect(service.getSnapshot().authoritative.connections).toEqual({ mine: { nested: { y: 2 } } });

  service.handleMessage({
    type: 'ephemeralChanges',
    ephemeralChanges: { databaseId: 'alpha', epoch: 'epoch-1', revision: 4, shared: { count: 99 } },
  });
  service.handleMessage({
    type: 'ephemeralChanges',
    ephemeralChanges: { databaseId: 'alpha', epoch: 'old', revision: 100, shared: { count: 99 } },
  });
  expect(service.getSnapshot().authoritative.shared).toEqual({ count: 0 });
  expect(service.getSnapshot().authoritative.revision).toBe(5);
});

test('binds every HTTP request to the private capability from its connected message', async () => {
  const { service, requests, advance, connect } = harness();
  connect('mine');
  service.handleMessage({
    type: 'ephemeralSnapshot',
    ephemeralCapability: 'forged-snapshot-capability',
    ephemeralSnapshot: {
      databaseId: 'alpha', epoch: 'epoch-1', revision: 2,
      shared: { count: 0 }, connections: { mine: { cursor: null } },
    },
  });
  service.handleMessage({
    type: 'ephemeralChanges',
    ephemeralCapability: 'forged-change-capability',
    ephemeralChanges: { databaseId: 'alpha', epoch: 'epoch-1', revision: 3, shared: { count: 1 } },
  });

  const update = service.updateConnection({ cursor: 1 });
  advance(0);
  await tick();
  expect(requests[0].body).toMatchObject({
    connectionId: 'mine',
    ephemeralCapability: 'capability-mine',
  });
  requests[0].accept();
  await update;
});

test('does not establish an ephemeral identity without a participation capability', () => {
  const { service } = harness();
  service.handleMessage({
    type: 'connected', databaseId: 'alpha', connectionId: 'mine', ephemeralEpoch: 'epoch-1',
  });
  service.handleMessage({
    type: 'ephemeralSnapshot',
    ephemeralSnapshot: { databaseId: 'alpha', epoch: 'epoch-1', revision: 1, connections: {} },
  });
  expect(service.getSnapshot().authoritative).toMatchObject({
    connectionId: null,
    freshness: { status: 'disconnected', stale: true },
  });
});

test('coalesces top-level fields, collapses repeated writes, and replaces nested values', async () => {
  const { service, requests, advance, connect } = harness();
  connect();
  const first = service.updateConnection({ cursor: { x: 1 }, status: 'typing' });
  const second = service.updateConnection({ cursor: { y: 2 } });
  advance(0);
  await tick();
  expect(requests).toHaveLength(1);
  expect(requests[0].body.patch).toEqual({ cursor: { y: 2 }, status: 'typing' });
  requests[0].accept();
  await expect(first).resolves.toMatchObject({ status: 'accepted' });
  await expect(second).resolves.toMatchObject({ status: 'accepted' });
  expect(service.getSnapshot().desired.connection).toEqual({ cursor: { y: 2 }, status: 'typing' });
});

test('serializes each channel while keeping connection and shared requests independent', async () => {
  const { service, requests, advance, connect } = harness({ maxUpdateCadenceMs: 50 });
  connect();
  const old = service.updateConnection({ cursor: 1 });
  const shared = service.updateShared({ count: 1 });
  advance(0);
  await tick();
  expect(requests).toHaveLength(2);
  const newer = service.updateConnection({ cursor: 2 });
  advance(49);
  await tick();
  expect(requests).toHaveLength(2);
  advance(1);
  await tick();
  expect(requests).toHaveLength(2);
  requests[0].accept({ revision: 11 });
  await tick();
  advance(0);
  await tick();
  expect(requests).toHaveLength(3);
  expect(requests[2].body.patch).toEqual({ cursor: 2 });
  requests[2].accept({ revision: 12 });
  requests[1].accept({ revision: 11 });
  await Promise.all([old, newer, shared]);
  expect(service.getSnapshot().desired.connection).toEqual({ cursor: 2 });
  expect(service.getSnapshot().latestOutcome.clientRequestSequence).toBe(requests[2].body.clientRequestSequence);
});

test('an older rejected field version cannot replace a newer accepted outcome', async () => {
  const { service, requests, advance, connect } = harness({ maxUpdateCadenceMs: 10 });
  connect();
  const older = service.updateConnection({ cursor: { x: 1 } });
  advance(0);
  await tick();
  const newer = service.updateConnection({ cursor: { x: 2 } });
  advance(10);
  await tick();
  requests[0].reject('stale rejection');
  await expect(older).rejects.toMatchObject({ outcome: { status: 'rejected', error: 'stale rejection' } });
  advance(0);
  await tick();
  expect(requests).toHaveLength(2);
  requests[1].accept({ revision: 12 });
  await newer;

  const snapshot = service.getSnapshot();
  expect(snapshot.desired.connection).toEqual({ cursor: { x: 2 } });
  expect(snapshot.latestOutcome).toMatchObject({
    status: 'accepted', clientRequestSequence: requests[1].body.clientRequestSequence,
  });
});

test('an older accepted field version cannot replace a newer rejected outcome', async () => {
  const { service, requests, advance, connect } = harness({ maxUpdateCadenceMs: 10 });
  connect();
  const older = service.updateShared({ count: 1 });
  advance(0);
  await tick();
  const newer = service.updateShared({ count: 2 });
  advance(10);
  await tick();
  requests[0].accept({ revision: 11 });
  await older;
  advance(0);
  await tick();
  expect(requests).toHaveLength(2);
  requests[1].reject('new value denied');
  await expect(newer).rejects.toMatchObject({ outcome: { status: 'rejected', error: 'new value denied' } });

  const snapshot = service.getSnapshot();
  expect(snapshot.desired.shared).toEqual({ count: 2 });
  expect(snapshot.latestOutcome).toMatchObject({
    status: 'rejected', clientRequestSequence: requests[1].body.clientRequestSequence,
  });
});

test('structured rejection and unknown transport outcomes reject every covered caller observably', async () => {
  const { service, requests, advance, connect } = harness();
  connect();
  const rejectedA = service.updateShared({ count: 1 });
  const rejectedB = service.updateShared({ title: 'x' });
  advance(0);
  await tick();
  const observedA = rejectedA.then(
    () => { throw new Error('expected rejectedA to reject'); },
    error => error,
  );
  const observedB = rejectedB.then(
    () => { throw new Error('expected rejectedB to reject'); },
    error => error,
  );
  requests[0].reject('not authorized');
  const [errorA, errorB] = await Promise.all([observedA, observedB]);
  expect(errorA).toMatchObject({ outcome: { status: 'rejected', error: 'not authorized' } });
  expect(errorB).toBeInstanceOf(EphemeralUpdateError);
  expect(service.getSnapshot().latestOutcome.status).toBe('rejected');

  advance(20);
  const unknown = service.updateShared({ count: 2 });
  advance(0);
  await tick();
  const observedUnknown = unknown.then(
    () => { throw new Error('expected unknown to reject'); },
    error => error,
  );
  requests[1].fail();
  expect(await observedUnknown).toMatchObject({ outcome: { status: 'unknown' } });
  expect(service.getSnapshot().latestOutcome.status).toBe('unknown');
});

test('disconnect fences late replies; reconnect republishes Connection desired but never Shared desired', async () => {
  const { service, requests, advance, connect } = harness();
  connect('old');
  const connection = service.updateConnection({ cursor: 1 });
  const shared = service.updateShared({ count: 7 });
  advance(0);
  await tick();
  expect(requests).toHaveLength(2);
  service.transportDisconnected();
  await expect(connection).rejects.toMatchObject({ outcome: { status: 'unknown' } });
  await expect(shared).rejects.toMatchObject({ outcome: { status: 'unknown' } });
  expect(service.getSnapshot().authoritative.freshness.status).toBe('disconnected');

  connect('fresh', 'epoch-2', 1, { shared: { count: 9 }, connections: { fresh: { cursor: null } } });
  advance(0);
  await tick();
  expect(requests).toHaveLength(3);
  expect(requests[2].url).toContain('/ephemeral/connection');
  expect(requests[2].body).toMatchObject({ connectionId: 'fresh', ephemeralEpoch: 'epoch-2', patch: { cursor: 1 } });
  expect(requests.some((request, index) => index >= 2 && request.url.includes('/shared'))).toBe(false);
});

test('resync uses authenticated route and atomically installs its complete snapshot', async () => {
  let headerCalls = 0;
  const { service, requests, connect } = harness({
    resolveHeaders: async () => ({ authorization: `Bearer ${++headerCalls}` }),
  });
  connect('mine', 'epoch-1', 2);
  service.handleMessage({
    type: 'ephemeralResyncRequired',
    ephemeralResyncRequired: { databaseId: 'alpha', epoch: 'epoch-1', revision: 8 },
  });
  await tick();
  expect(service.getSnapshot().authoritative.freshness.status).toBe('resyncing');
  expect(new Headers(requests[0].init.headers).get('authorization')).toBe('Bearer 1');
  requests[0].accept({
    revision: 8,
    ephemeralSnapshot: {
      databaseId: 'alpha', epoch: 'epoch-1', revision: 8,
      shared: { count: 8 }, connections: { mine: { cursor: 8 } },
    },
  });
  await tick();
  expect(service.getSnapshot().authoritative).toMatchObject({
    shared: { count: 8 }, connections: { mine: { cursor: 8 } }, revision: 8,
    freshness: { status: 'live', stale: false },
  });
});

test('buffers changes racing with resnapshot and applies newer revisions in order with one recovered snapshot', async () => {
  const { service, requests, connect } = harness();
  const observed = [];
  connect('mine', 'epoch-1', 2, { connections: { mine: { cursor: 0 }, old: { cursor: 0 } } });
  service.subscribe(snapshot => observed.push(snapshot));
  service.handleMessage({
    type: 'ephemeralResyncRequired',
    ephemeralResyncRequired: { databaseId: 'alpha', epoch: 'epoch-1', revision: 8 },
  });
  await tick();
  service.handleMessage({
    type: 'ephemeralChanges',
    ephemeralChanges: {
      databaseId: 'alpha', epoch: 'epoch-1', revision: 10,
      shared: { count: 10 }, connections: { peer: { cursor: 10 } },
    },
  });
  service.handleMessage({
    type: 'ephemeralChanges',
    ephemeralChanges: {
      databaseId: 'alpha', epoch: 'epoch-1', revision: 9,
      connections: { mine: { cursor: 9 } }, removedConnections: ['old'],
    },
  });
  expect(service.getSnapshot().authoritative.revision).toBe(2);
  requests[0].accept({
    revision: 8,
    ephemeralSnapshot: {
      databaseId: 'alpha', epoch: 'epoch-1', revision: 8,
      shared: { count: 8 }, connections: { mine: { cursor: 8 }, old: { cursor: 8 } },
    },
  });
  await tick();
  expect(service.getSnapshot().authoritative).toMatchObject({
    revision: 10,
    shared: { count: 10 },
    connections: { mine: { cursor: 9 }, peer: { cursor: 10 } },
    freshness: { status: 'live', stale: false },
  });
  expect(observed.filter(snapshot => snapshot.authoritative.freshness.status === 'live').at(-1).authoritative.revision).toBe(10);
});

test('a second overflow during recovery triggers another authenticated resnapshot before becoming live', async () => {
  let headerCalls = 0;
  const { service, requests, connect } = harness({
    resolveHeaders: async () => ({ authorization: `Bearer ${++headerCalls}` }),
  });
  connect('mine', 'epoch-1', 2);
  service.handleMessage({
    type: 'ephemeralResyncRequired',
    ephemeralResyncRequired: { databaseId: 'alpha', epoch: 'epoch-1', revision: 8 },
  });
  await tick();
  service.handleMessage({
    type: 'ephemeralResyncRequired',
    ephemeralResyncRequired: { databaseId: 'alpha', epoch: 'epoch-1', revision: 12 },
  });

  expect(requests).toHaveLength(1);
  requests[0].accept({
    revision: 8,
    ephemeralSnapshot: {
      databaseId: 'alpha', epoch: 'epoch-1', revision: 8,
      shared: { count: 8 }, connections: { mine: { cursor: 8 } },
    },
  });
  await tick();

  expect(requests).toHaveLength(2);
  expect(new Headers(requests[0].init.headers).get('authorization')).toBe('Bearer 1');
  expect(new Headers(requests[1].init.headers).get('authorization')).toBe('Bearer 2');
  expect(service.getSnapshot().authoritative).toMatchObject({
    revision: 8,
    freshness: { status: 'resyncing', stale: true },
  });

  requests[1].accept({
    revision: 12,
    ephemeralSnapshot: {
      databaseId: 'alpha', epoch: 'epoch-1', revision: 12,
      shared: { count: 12 }, connections: { mine: { cursor: 12 } },
    },
  });
  await tick();

  expect(service.getSnapshot().authoritative).toMatchObject({
    revision: 12,
    shared: { count: 12 },
    connections: { mine: { cursor: 12 } },
    freshness: { status: 'live', stale: false },
  });
});

test('lease renewal is independent, reauthenticates, and rejection ends live state', async () => {
  let headerCalls = 0;
  const { service, requests, reconnects, advance, connect } = harness({
    leaseCadenceMs: 30,
    resolveHeaders: async () => ({ authorization: `session-${++headerCalls}` }),
  });
  connect();
  advance(30);
  await tick();
  expect(requests[0].url).toContain('/ephemeral/lease');
  expect(new Headers(requests[0].init.headers).get('authorization')).toBe('session-1');
  requests[0].accept();
  await tick();
  advance(30);
  await tick();
  expect(new Headers(requests[1].init.headers).get('authorization')).toBe('session-2');
  requests[1].reject('authority lost');
  await tick();
  expect(service.getSnapshot().authoritative.freshness).toMatchObject({ status: 'error', stale: true });
  expect(service.getSnapshot().authoritative.connectionId).toBeNull();
  expect(reconnects).toEqual([60]);
});

test('a hanging lease times out, aborts, and requests transport recovery', async () => {
  const { service, requests, reconnects, advance, connect } = harness({
    leaseCadenceMs: 20,
    requestTimeoutMs: 15,
  });
  connect();
  advance(20);
  await tick();
  expect(requests).toHaveLength(1);
  expect(requests[0].init.signal.aborted).toBe(false);
  advance(15);
  await tick();
  expect(requests[0].init.signal.aborted).toBe(true);
  expect(service.getSnapshot().authoritative.freshness).toMatchObject({ status: 'error', stale: true });
  expect(reconnects).toEqual([35]);
  service.dispose();
  advance(100);
  expect(reconnects).toEqual([35]);
});

test('dispose fences a delayed header resolution without fetch or reconnect', async () => {
  let resolveHeaders;
  const headers = new Promise(resolve => { resolveHeaders = resolve; });
  const { service, requests, reconnects, advance, connect } = harness({
    leaseCadenceMs: 20,
    requestTimeoutMs: 15,
    resolveHeaders: () => headers,
  });
  connect();
  advance(20);
  await tick();
  service.dispose();
  resolveHeaders({ authorization: 'too-late' });
  advance(15);
  await tick();
  expect(requests).toHaveLength(0);
  expect(reconnects).toHaveLength(0);
});

test('own connection removal makes state stale and requests a reconnect', () => {
  const { service, reconnects, connect } = harness();
  connect('mine', 'epoch-1', 1);
  service.handleMessage({
    type: 'ephemeralChanges',
    ephemeralChanges: {
      databaseId: 'alpha', epoch: 'epoch-1', revision: 2, removedConnections: ['mine'],
    },
  });
  expect(service.getSnapshot().authoritative.freshness).toMatchObject({ status: 'error', stale: true });
  expect(reconnects).toHaveLength(1);
});

test('read-only mode rejects before HTTP and still renews its subscription lease', async () => {
  const { service, requests, advance, connect } = harness({ ephemeralWrite: false, leaseCadenceMs: 10 });
  connect();
  await expect(service.updateConnection({ cursor: 1 })).rejects.toMatchObject({
    outcome: { status: 'rejected', clientRequestSequence: null },
  });
  expect(requests).toHaveLength(0);
  expect(service.getSnapshot().desired.connection).toEqual({});
  advance(10);
  await tick();
  expect(requests).toHaveLength(1);
  expect(requests[0].url).toContain('/ephemeral/lease');
});

test('database identity isolates messages and HTTP bodies without IndexedDB', async () => {
  const originalIndexedDb = globalThis.indexedDB;
  Object.defineProperty(globalThis, 'indexedDB', {
    configurable: true,
    get() { throw new Error('ephemeral state must not access IndexedDB'); },
  });
  try {
    const alpha = harness({ databaseId: 'alpha' });
    const beta = harness({ databaseId: 'beta' });
    alpha.connect();
    beta.connect();
    alpha.service.handleMessage({
      type: 'ephemeralChanges',
      ephemeralChanges: { databaseId: 'beta', epoch: 'epoch-1', revision: 2, shared: { count: 99 } },
    });
    expect(alpha.service.getSnapshot().authoritative.shared).toEqual({ count: 0 });
    const update = beta.service.updateShared({ count: 2 });
    beta.advance(0);
    await tick();
    expect(beta.requests[0].body.databaseId).toBe('beta');
    beta.requests[0].accept();
    await update;
  } finally {
    Object.defineProperty(globalThis, 'indexedDB', { configurable: true, value: originalIndexedDb });
  }
});

test('captures patches immutably and normalizes supported non-JSON primitives', async () => {
  const { service, requests, advance, connect } = harness();
  connect();
  const nested = { point: { x: 1 }, when: new Date('2024-01-02T03:04:05.000Z') };
  const update = service.updateConnection({ nested, bytes: new Uint8Array([1, 2]), count: 3n });
  nested.point.x = 99;
  nested.when.setUTCFullYear(2030);
  advance(0);
  await tick();
  expect(requests[0].body.patch).toEqual({
    nested: { point: { x: 1 }, when: 1704164645 },
    bytes: [1, 2], count: 3,
  });
  expect(service.getSnapshot().desired.connection).toEqual(requests[0].body.patch);
  requests[0].accept();
  await update;
});

test('rejects empty, undefined, circular, and values JSON would silently discard', async () => {
  const { service, connect } = harness();
  connect();
  const circular = {};
  circular.self = circular;
  for (const patch of [
    {}, { value: undefined }, { value: Number.NaN }, { value: () => {} },
    { values: [undefined] }, circular, { value: new Date(Number.NaN) },
    { value: BigInt(Number.MAX_SAFE_INTEGER) + 1n },
    { value: BigInt(Number.MIN_SAFE_INTEGER) - 1n },
  ]) {
    await expect(service.updateConnection(patch)).rejects.toBeInstanceOf(TypeError);
  }
  expect(service.getSnapshot().desired.connection).toEqual({});
});

test('returns isolated deep snapshots to every caller and subscriber', () => {
  const { service, connect } = harness();
  connect('mine', 'epoch-1', 1, {
    shared: { nested: { count: 1 } },
    connections: { mine: { cursor: { x: 1 } } },
  });
  const secondSubscriber = [];
  service.subscribe(snapshot => {
    snapshot.authoritative.shared.nested.count = 99;
    snapshot.authoritative.connections.mine.cursor.x = 99;
    snapshot.desired.connection.injected = true;
  });
  service.subscribe(snapshot => secondSubscriber.push(snapshot));
  expect(secondSubscriber[0].authoritative.shared.nested.count).toBe(1);
  expect(secondSubscriber[0].authoritative.connections.mine.cursor.x).toBe(1);
  expect(secondSubscriber[0].desired.connection).toEqual({});
  const first = service.getSnapshot();
  first.authoritative.shared.nested.count = 50;
  expect(service.getSnapshot().authoritative.shared.nested.count).toBe(1);
});

test('validates identity, endpoints, and cadence configuration', () => {
  const config = {
    databaseId: 'alpha', baseUrl: 'https://api.example.test',
    endpoints: { connection: '/c', shared: '/s', lease: '/l', resnapshot: '/r' },
  };
  expect(() => new EphemeralStateService({ ...config, databaseId: ' ' })).toThrow('databaseId');
  expect(() => new EphemeralStateService({ ...config, baseUrl: '/relative' })).toThrow('baseUrl');
  expect(() => new EphemeralStateService({ ...config, endpoints: { ...config.endpoints, lease: '' } })).toThrow('lease endpoint');
  for (const maxUpdateCadenceMs of [-1, Number.NaN, Infinity]) {
    expect(() => new EphemeralStateService({ ...config, maxUpdateCadenceMs })).toThrow('finite non-negative');
  }
  for (const leaseCadenceMs of [0, -1, Number.NaN, Infinity]) {
    expect(() => new EphemeralStateService({ ...config, leaseCadenceMs })).toThrow('finite positive');
  }
  for (const requestTimeoutMs of [0, -1, Number.NaN, Infinity]) {
    expect(() => new EphemeralStateService({ ...config, requestTimeoutMs })).toThrow('finite positive');
  }
  expect(() => new EphemeralStateService({ ...config, maxUpdateCadenceMs: 0, leaseCadenceMs: 1 })).not.toThrow();
});

test('does not renew a lease or become live before an ephemeral snapshot', () => {
  const { service, requests, advance, timers } = harness({ leaseCadenceMs: 10 });
  service.transportConnecting();
  service.handleMessage({
    type: 'connected', databaseId: 'alpha', connectionId: 'c1',
    ephemeralEpoch: 'epoch-1', ephemeralCapability: 'capability-c1',
  });
  advance(100);
  expect(requests).toHaveLength(0);
  expect(timers.size).toBe(0);
  expect(service.getSnapshot().authoritative.freshness).toEqual({ status: 'connecting', stale: true });
});

test('forces JSON content type and rejects accepted bodies on unsuccessful HTTP responses', async () => {
  let request;
  const service = new EphemeralStateService({
    databaseId: 'alpha', baseUrl: 'https://api.example.test',
    endpoints: { connection: '/c', shared: '/s', lease: '/l', resnapshot: '/r' },
    maxUpdateCadenceMs: 0, leaseCadenceMs: 100,
    resolveHeaders: async () => ({ 'Content-Type': 'text/plain', authorization: 'Bearer token' }),
    fetch: async (_url, init) => {
      request = init;
      const body = JSON.parse(init.body);
      return new Response(JSON.stringify({
        type: 'ephemeralAccepted', operation: 'connectionPatch',
        clientRequestSequence: body.clientRequestSequence,
        ephemeralEpoch: body.ephemeralEpoch, revision: 2,
      }), { status: 500 });
    },
  });
  service.handleMessage({
    type: 'connected', databaseId: 'alpha', connectionId: 'c1',
    ephemeralEpoch: 'e1', ephemeralCapability: 'capability-c1',
  });
  service.handleMessage({ type: 'ephemeralSnapshot', ephemeralSnapshot: {
    databaseId: 'alpha', epoch: 'e1', revision: 1, connections: {},
  } });
  await expect(service.updateConnection({ cursor: 1 })).rejects.toMatchObject({
    outcome: { status: 'unknown', error: expect.stringContaining('HTTP 500') },
  });
  expect(new Headers(request.headers).get('content-type')).toBe('application/json');
  expect(new Headers(request.headers).get('authorization')).toBe('Bearer token');
  service.dispose();
});
