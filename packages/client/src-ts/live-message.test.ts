// @ts-nocheck
import { expect, test } from 'bun:test';

import loadElm from '../dist/engine.mjs';

const schema = {
  tables: {
    maps: {
      name: 'maps',
      links: {},
      indices: [],
    },
  },
  queryFieldToTable: { maps: 'maps' },
};

function deltaMessage(databaseId?: string) {
  return {
    type: 'delta',
    databaseId,
    data: [
      {
        table_name: 'maps',
        headers: ['id', 'name', 'updatedAt'],
        rows: [[1, 'World', 10]],
      },
    ],
  };
}

async function startSyncedElmApp() {
  const previousXmlHttpRequest = globalThis.XMLHttpRequest;
  const requests: Array<{ method: string; url: string; body?: string }> = [];
  let currentMethod = 'GET';

  class MockXMLHttpRequest {
    listeners: Record<string, Array<() => void>> = {};
    status = 200;
    statusText = 'OK';
    responseURL = '';
    responseType = '';
    response = JSON.stringify({ databaseId: 'campaign:123', databaseEpoch: 'test-epoch', tables: {}, has_more: false });
    timeout = 0;
    withCredentials = false;

    addEventListener(type: string, callback: () => void) {
      this.listeners[type] = this.listeners[type] ?? [];
      this.listeners[type].push(callback);
    }

    open(method: string, url: string) {
      currentMethod = method;
      this.responseURL = url;
    }

    setRequestHeader() {}

    send(body?: string) {
      requests.push({ method: currentMethod, url: this.responseURL, body });
      queueMicrotask(() => {
        (this.listeners.load ?? []).forEach((listener) => listener());
      });
    }

    abort() {}

    getAllResponseHeaders() {
      return '';
    }
  }

  globalThis.XMLHttpRequest = MockXMLHttpRequest;

  const Elm = loadElm(Object.create(globalThis));
  const app = Elm.Main.init({
    flags: {
      schema,
      server: {
        baseUrl: 'http://example.test',
        catchupPath: '/sync',
        databaseId: 'campaign:123',
        databaseEpoch: 'test-epoch',
      },
      liveSync: {
        transport: 'sse',
      },
    },
  });

  app.ports.indexedDbOut.subscribe((message) => {
    if (message?.type !== 'requestInitialData') {
      return;
    }

    app.ports.receiveIndexedDbMessage.send({
      type: 'initialData',
      data: {
        tables: { maps: [] },
        cursor: { tables: {} },
      },
    });
  });

  await Bun.sleep(0);
  await Bun.sleep(0);

  return { app, requests, restore: () => { globalThis.XMLHttpRequest = previousXmlHttpRequest; } };
}

test('Elm live sync rejects missing delta databaseId when configured', async () => {
  const errors: string[] = [];
  const writes: unknown[] = [];
  const { app, restore } = await startSyncedElmApp();

  try {
    app.ports.errorOut.subscribe((message) => {
      errors.push(message);
    });
    app.ports.indexedDbOut.subscribe((message) => {
      if (message?.type === 'writeDelta') {
        writes.push(message);
      }
    });

    app.ports.receiveSSEMessage.send(deltaMessage());
    await Bun.sleep(0);

    expect(errors).toContain('Live sync delta missing databaseId: expected campaign:123');
    expect(writes).toHaveLength(0);
  } finally {
    restore();
  }
});

test('Elm live sync rejects mismatched delta databaseId before writing cache', async () => {
  const errors: string[] = [];
  const writes: unknown[] = [];
  const { app, restore } = await startSyncedElmApp();

  try {
    app.ports.errorOut.subscribe((message) => {
      errors.push(message);
    });
    app.ports.indexedDbOut.subscribe((message) => {
      if (message?.type === 'writeDelta') {
        writes.push(message);
      }
    });

    app.ports.receiveSSEMessage.send(deltaMessage('campaign:456'));
    await Bun.sleep(0);

    expect(errors).toContain('Live sync delta databaseId mismatch: expected campaign:123, got campaign:456');
    expect(writes).toHaveLength(0);
  } finally {
    restore();
  }
});

test('Elm live sync accepts matching delta databaseId', async () => {
  const errors: string[] = [];
  const writes: unknown[] = [];
  const { app, restore } = await startSyncedElmApp();

  try {
    app.ports.errorOut.subscribe((message) => {
      errors.push(message);
    });
    app.ports.indexedDbOut.subscribe((message) => {
      if (message?.type === 'writeDelta') {
        writes.push(message);
      }
    });

    app.ports.receiveSSEMessage.send(deltaMessage('campaign:123'));
    await Bun.sleep(0);

    expect(errors).toHaveLength(0);
    expect(writes).toHaveLength(1);
  } finally {
    restore();
  }
});

test('Elm live syncRequired starts catchup from the current cursor', async () => {
  const { app, requests, restore } = await startSyncedElmApp();

  try {
    const requestCountAfterInitialCatchup = requests.length;

    app.ports.receiveSSEMessage.send({
      type: 'syncRequired',
      databaseId: 'campaign:123',
    });
    await Bun.sleep(0);
    await Bun.sleep(0);

    expect(requests).toHaveLength(requestCountAfterInitialCatchup + 1);
    expect(requests.at(-1)?.method).toBe('POST');
    expect(JSON.parse(requests.at(-1)?.body ?? '{}')).toEqual({
      databaseId: 'campaign:123',
      databaseEpoch: 'test-epoch',
      syncCursor: {
        tables: {
          maps: {
            last_seen_updated_at: null,
            last_seen_primary_key: null,
            permission_hash: '',
          },
        },
      },
    });
  } finally {
    restore();
  }
});

test('Elm live syncRequired ignores stale server revisions', async () => {
  const { app, requests, restore } = await startSyncedElmApp();

  try {
    app.ports.receiveSSEMessage.send({
      ...deltaMessage('campaign:123'),
      serverRevision: 7,
    });
    await Bun.sleep(0);

    const requestCountAfterInitialCatchup = requests.length;

    app.ports.receiveSSEMessage.send({
      type: 'syncRequired',
      databaseId: 'campaign:123',
      serverRevision: 7,
    });
    await Bun.sleep(0);
    await Bun.sleep(0);

    expect(requests).toHaveLength(requestCountAfterInitialCatchup);
  } finally {
    restore();
  }
});

test('Elm catchup emits entity stream catchup notifications', async () => {
  const previousXmlHttpRequest = globalThis.XMLHttpRequest;

  class MockXMLHttpRequest {
    listeners: Record<string, Array<() => void>> = {};
    status = 200;
    statusText = 'OK';
    responseURL = '';
    responseType = '';
    response = '';
    timeout = 0;
    withCredentials = false;

    addEventListener(type: string, callback: () => void) {
      this.listeners[type] = this.listeners[type] ?? [];
      this.listeners[type].push(callback);
    }

    open(_method: string, url: string) {
      this.responseURL = url;
    }

    setRequestHeader() {}

    send() {
      this.response = JSON.stringify({
        databaseId: 'campaign:123',
        databaseEpoch: 'test-epoch',
        serverRevision: 1,
        tables: {
          maps: {
            rows: [{ id: 1, name: 'Catchup Map', updatedAt: 1 }],
            permission_hash: 'allowed',
            last_seen_updated_at: 1,
          },
        },
        has_more: false,
      });
      queueMicrotask(() => (this.listeners.load ?? []).forEach((listener) => listener()));
    }

    abort() {}

    getAllResponseHeaders() {
      return '';
    }
  }

  globalThis.XMLHttpRequest = MockXMLHttpRequest;

  const Elm = loadElm(Object.create(globalThis));
  const app = Elm.Main.init({
    flags: {
      schema,
      server: {
        baseUrl: 'http://example.test',
        catchupPath: '/sync',
        databaseId: 'campaign:123',
      },
      liveSync: { transport: 'sse' },
    },
  });
  const notifications: unknown[] = [];

  try {
    app.ports.indexedDbOut.subscribe((message) => {
      if (message?.type === 'requestInitialData') {
        app.ports.receiveIndexedDbMessage.send({
          type: 'initialData',
          data: {
            tables: {},
            cursor: { tables: {} },
            lastAppliedServerRevision: null,
            databaseEpoch: null,
          },
        });
      }

      if (message?.type === 'writeDelta' && message?.entityStreamSource === 'catchup') {
        notifications.push(message);
      }
    });

    await Bun.sleep(0);
    await Bun.sleep(0);

    expect(notifications).toEqual([
      {
        type: 'writeDelta',
        entityStreamSource: 'catchup',
        tableGroups: [
          {
            table_name: 'maps',
            headers: ['id', 'name', 'updatedAt'],
            rows: [[1, 'Catchup Map', 1]],
          },
        ],
      },
    ]);
  } finally {
    globalThis.XMLHttpRequest = previousXmlHttpRequest;
  }
});

test('Elm mutation response sync preserves newer rapid optimistic state', async () => {
  const previousXmlHttpRequest = globalThis.XMLHttpRequest;
  const pendingMutations: Array<{ url: string; complete: (response: unknown) => void }> = [];

  class MockXMLHttpRequest {
    listeners: Record<string, Array<() => void>> = {};
    status = 200;
    statusText = 'OK';
    responseURL = '';
    responseType = '';
    response = '';
    timeout = 0;
    withCredentials = false;

    addEventListener(type: string, callback: () => void) {
      this.listeners[type] = this.listeners[type] ?? [];
      this.listeners[type].push(callback);
    }

    open(_method: string, url: string) {
      this.responseURL = url;
    }

    setRequestHeader() {}

    send() {
      if (this.responseURL.endsWith('/sync')) {
        this.response = JSON.stringify({ databaseId: 'campaign:123', databaseEpoch: 'test-epoch', serverRevision: 0, tables: {}, has_more: false });
        queueMicrotask(() => (this.listeners.load ?? []).forEach((listener) => listener()));
        return;
      }

      pendingMutations.push({
        url: this.responseURL,
        complete: (response: unknown) => {
          this.response = JSON.stringify(response);
          (this.listeners.load ?? []).forEach((listener) => listener());
        },
      });
    }

    abort() {}

    getAllResponseHeaders() {
      return '';
    }
  }

  globalThis.XMLHttpRequest = MockXMLHttpRequest;

  const Elm = loadElm(Object.create(globalThis));
  const app = Elm.Main.init({
    flags: {
      schema,
      server: {
        baseUrl: 'http://example.test',
        catchupPath: '/sync',
        databaseId: 'campaign:123',
      },
      liveSync: { transport: 'sse' },
    },
  });
  const queryResults: unknown[] = [];

  try {
    app.ports.queryClientOut.subscribe((message) => {
      if (message?.type === 'full') {
        queryResults.push(message.result);
      }
    });
    app.ports.indexedDbOut.subscribe((message) => {
      if (message?.type !== 'requestInitialData') {
        return;
      }

      app.ports.receiveIndexedDbMessage.send({
        type: 'initialData',
        data: {
          tables: { maps: [{ id: 1, name: 'Initial', updatedAt: 0 }] },
          cursor: { tables: {} },
          lastAppliedServerRevision: null,
        },
      });
    });

    app.ports.receiveIndexedDbMessage.send({
      type: 'initialData',
      data: {
        tables: { maps: [{ id: 1, name: 'Initial', updatedAt: 0 }] },
        cursor: { tables: {} },
        lastAppliedServerRevision: null,
      },
    });

    await Bun.sleep(0);
    await Bun.sleep(0);

    const optimistic = {
      queryField: 'maps',
      where: { field: 'id', input: 'id' },
      set: [{ field: 'name', input: 'name' }],
    };

    app.ports.receiveQueryManagerMessage.send({
      type: 'sendMutation',
      requestId: 'a',
      mutationId: 'move',
      baseUrl: 'http://example.test/db',
      input: { id: 1, name: 'A' },
      optimistic,
    });
    app.ports.receiveQueryManagerMessage.send({
      type: 'sendMutation',
      requestId: 'b',
      mutationId: 'move',
      baseUrl: 'http://example.test/db',
      input: { id: 1, name: 'B' },
      optimistic,
    });
    await Bun.sleep(0);

    expect(pendingMutations).toHaveLength(2);

    pendingMutations[1].complete({
      serverRevision: 2,
      sync: {
        type: 'delta',
        serverRevision: 2,
        databaseId: 'campaign:123',
        data: [{ table_name: 'maps', headers: ['id', 'name', 'updatedAt'], rows: [[1, 'B', 2]] }],
      },
      result: {},
    });
    await Bun.sleep(0);

    pendingMutations[0].complete({
      serverRevision: 1,
      sync: {
        type: 'delta',
        serverRevision: 1,
        databaseId: 'campaign:123',
        data: [{ table_name: 'maps', headers: ['id', 'name', 'updatedAt'], rows: [[1, 'A', 1]] }],
      },
      result: {},
    });
    await Bun.sleep(0);

    app.ports.receiveQueryClientMessage.send({
      type: 'register',
      queryId: 'maps-query',
      querySource: { maps: { id: true, name: true } },
      queryInput: {},
    });
    await Bun.sleep(0);

    const latest = queryResults.at(-1) as { maps?: Array<{ name?: string }> };
    expect(latest.maps?.[0]?.name).toBe('B');
  } finally {
    globalThis.XMLHttpRequest = previousXmlHttpRequest;
  }
});
