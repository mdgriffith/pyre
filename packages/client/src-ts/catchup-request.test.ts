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
  queryFieldToTable: {},
};

test('Elm catchup request includes restored syncCursor on startup', async () => {
  const requestedUrls: string[] = [];
  const requestedMethods: string[] = [];
  const requestBodies: string[] = [];
  const previousXmlHttpRequest = globalThis.XMLHttpRequest;

  class MockXMLHttpRequest {
    listeners: Record<string, Array<() => void>> = {};
    status = 200;
    statusText = 'OK';
    responseURL = '';
    responseType = '';
    response = JSON.stringify({ databaseEpoch: 'test-epoch', tables: {}, has_more: false });
    timeout = 0;
    withCredentials = false;
    c = false;

    addEventListener(type: string, callback: () => void) {
      this.listeners[type] = this.listeners[type] ?? [];
      this.listeners[type].push(callback);
    }

    open(method: string, url: string) {
      this.responseURL = url;
      requestedMethods.push(method);
      requestedUrls.push(url);
    }

    setRequestHeader() {}

    send(body?: string) {
      requestBodies.push(body ?? '');
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

  try {
    const Elm = loadElm(Object.create(globalThis));
    const app = Elm.Main.init({
      flags: {
        schema,
        server: {
          baseUrl: 'http://example.test',
          catchupPath: '/sync',
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
          cursor: {
            tables: {
              maps: {
                last_seen_updated_at: null,
                permission_hash: 'perm-hash',
              },
            },
          },
        },
      });
    });

    await Bun.sleep(0);
    await Bun.sleep(0);

    expect(requestedUrls).toHaveLength(1);
    expect(requestedMethods).toEqual(['POST']);
    expect(requestedUrls[0]).toBe('http://example.test/sync');
    expect(JSON.parse(requestBodies[0])).toEqual({
      syncCursor: {
        tables: {
          maps: {
            last_seen_updated_at: null,
            permission_hash: 'perm-hash',
          },
        },
      },
    });
  } finally {
    globalThis.XMLHttpRequest = previousXmlHttpRequest;
  }
});

test('Elm catchup request includes databaseId when configured', async () => {
  const requestedUrls: string[] = [];
  const requestedMethods: string[] = [];
  const requestBodies: string[] = [];
  const previousXmlHttpRequest = globalThis.XMLHttpRequest;

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
      this.responseURL = url;
      requestedMethods.push(method);
      requestedUrls.push(url);
    }

    setRequestHeader() {}

    send(body?: string) {
      requestBodies.push(body ?? '');
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

  try {
    const Elm = loadElm(Object.create(globalThis));
    const app = Elm.Main.init({
      flags: {
        schema,
        server: {
          baseUrl: 'http://example.test',
          catchupPath: '/sync',
          databaseId: 'campaign:123',
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

    expect(requestedUrls).toHaveLength(1);
    expect(requestedMethods).toEqual(['POST']);
    expect(requestedUrls[0]).toBe('http://example.test/sync');
    expect(JSON.parse(requestBodies[0])).toEqual({
      databaseId: 'campaign:123',
      syncCursor: {
        tables: {
          maps: {
            last_seen_updated_at: null,
            permission_hash: '',
          },
        },
      },
    });
  } finally {
    globalThis.XMLHttpRequest = previousXmlHttpRequest;
  }
});

test('Elm catchup waits for startSync when autoStart is false', async () => {
  const requestedUrls: string[] = [];
  const requestedMethods: string[] = [];
  const requestBodies: string[] = [];
  const previousXmlHttpRequest = globalThis.XMLHttpRequest;

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
      this.responseURL = url;
      requestedMethods.push(method);
      requestedUrls.push(url);
    }

    setRequestHeader() {}

    send(body?: string) {
      requestBodies.push(body ?? '');
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

  try {
    const Elm = loadElm(Object.create(globalThis));
    const app = Elm.Main.init({
      flags: {
        schema,
        server: {
          baseUrl: 'http://example.test',
          catchupPath: '/sync',
          databaseId: 'campaign:123',
        },
        liveSync: {
          transport: 'sse',
        },
        sync: {
          autoStart: false,
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
    expect(requestedUrls).toHaveLength(0);

    app.ports.receiveSyncControlMessage.send({ type: 'startSync' });
    await Bun.sleep(0);
    await Bun.sleep(0);

    expect(requestedUrls).toHaveLength(1);
    expect(requestedMethods).toEqual(['POST']);
    expect(JSON.parse(requestBodies[0]).databaseId).toBe('campaign:123');
  } finally {
    globalThis.XMLHttpRequest = previousXmlHttpRequest;
  }
});

test('Elm catchup rejects missing response databaseId when configured', async () => {
  const errors: string[] = [];
  const previousXmlHttpRequest = globalThis.XMLHttpRequest;

  class MockXMLHttpRequest {
    listeners: Record<string, Array<() => void>> = {};
    status = 200;
    statusText = 'OK';
    responseURL = '';
    responseType = '';
    response = JSON.stringify({ databaseEpoch: 'test-epoch', tables: {}, has_more: false });
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

  try {
    const Elm = loadElm(Object.create(globalThis));
    const app = Elm.Main.init({
      flags: {
        schema,
        server: {
          baseUrl: 'http://example.test',
          catchupPath: '/sync',
          databaseId: 'campaign:123',
        },
        liveSync: {
          transport: 'sse',
        },
      },
    });

    app.ports.errorOut.subscribe((message) => {
      errors.push(message);
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

    expect(errors).toContain('Catchup response missing databaseId: expected campaign:123');
  } finally {
    globalThis.XMLHttpRequest = previousXmlHttpRequest;
  }
});

test('Elm catchup rejects mismatched response databaseId', async () => {
  const errors: string[] = [];
  const previousXmlHttpRequest = globalThis.XMLHttpRequest;

  class MockXMLHttpRequest {
    listeners: Record<string, Array<() => void>> = {};
    status = 200;
    statusText = 'OK';
    responseURL = '';
    responseType = '';
    response = JSON.stringify({ databaseId: 'campaign:456', databaseEpoch: 'test-epoch', tables: {}, has_more: false });
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

  try {
    const Elm = loadElm(Object.create(globalThis));
    const app = Elm.Main.init({
      flags: {
        schema,
        server: {
          baseUrl: 'http://example.test',
          catchupPath: '/sync',
          databaseId: 'campaign:123',
        },
        liveSync: {
          transport: 'sse',
        },
      },
    });

    app.ports.errorOut.subscribe((message) => {
      errors.push(message);
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

    expect(errors).toContain('Catchup response databaseId mismatch: expected campaign:123, got campaign:456');
  } finally {
    globalThis.XMLHttpRequest = previousXmlHttpRequest;
  }
});

test('Elm catchup request includes credentials and headers when configured', async () => {
  const requestCredentials: boolean[] = [];
  const requestHeaders: Record<string, string> = {};
  const previousXmlHttpRequest = globalThis.XMLHttpRequest;

  class MockXMLHttpRequest {
    listeners: Record<string, Array<() => void>> = {};
    status = 200;
    statusText = 'OK';
    responseURL = '';
    responseType = '';
    response = JSON.stringify({ databaseEpoch: 'test-epoch', tables: {}, has_more: false });
    timeout = 0;
    withCredentials = false;

    addEventListener(type: string, callback: () => void) {
      this.listeners[type] = this.listeners[type] ?? [];
      this.listeners[type].push(callback);
    }

    open(_method: string, url: string) {
      this.responseURL = url;
    }

    setRequestHeader(key: string, value: string) {
      requestHeaders[key] = value;
    }

    send() {
      requestCredentials.push(this.withCredentials);
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

  try {
    const Elm = loadElm(Object.create(globalThis));
    const app = Elm.Main.init({
      flags: {
        schema,
        server: {
          baseUrl: 'http://example.test',
          catchupPath: '/sync',
          headers: [['Authorization', 'Bearer token-1']],
          credentials: 'include',
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

    expect(requestCredentials).toEqual([true]);
    expect(requestHeaders.Authorization).toBe('Bearer token-1');
  } finally {
    globalThis.XMLHttpRequest = previousXmlHttpRequest;
  }
});

test('Elm destructively resets persisted state before retrying a changed database epoch', async () => {
  const previousXmlHttpRequest = globalThis.XMLHttpRequest;
  const requestBodies: unknown[] = [];

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
    open(_method: string, url: string) { this.responseURL = url; }
    setRequestHeader() {}
    send(body?: string) {
      requestBodies.push(JSON.parse(body ?? '{}'));
      this.response = requestBodies.length === 1
        ? JSON.stringify({
            type: 'reset',
            databaseEpoch: 'new-epoch',
            operation: 'replace',
            scope: 'database',
            reason: 'database_epoch_changed',
          })
        : JSON.stringify({ databaseEpoch: 'new-epoch', tables: {}, has_more: false });
      queueMicrotask(() => (this.listeners.load ?? []).forEach((listener) => listener()));
    }
    abort() {}
    getAllResponseHeaders() { return ''; }
  }

  globalThis.XMLHttpRequest = MockXMLHttpRequest;
  try {
    const Elm = loadElm(Object.create(globalThis));
    const app = Elm.Main.init({
      flags: {
        schema,
        server: { baseUrl: 'http://example.test', catchupPath: '/sync' },
        liveSync: { transport: 'sse' },
      },
    });
    const resetMessages: unknown[] = [];
    const debugMessages: unknown[] = [];
    app.ports.debugOut.subscribe((message) => debugMessages.push(message));
    app.ports.indexedDbOut.subscribe((message) => {
      if (message?.type === 'requestInitialData') {
        app.ports.receiveIndexedDbMessage.send({
          type: 'initialData',
          data: {
            tables: { maps: [{ id: 1, updatedAt: 9 }] },
            cursor: { tables: { maps: { last_seen_updated_at: 9, permission_hash: 'old' } } },
            lastAppliedServerRevision: 20,
            databaseEpoch: 'old-epoch',
          },
        });
      }
      if (message?.type === 'resetForDatabaseEpoch') {
        resetMessages.push(message);
        app.ports.receiveIndexedDbMessage.send({
          type: 'databaseEpochResetCompleted',
          databaseEpoch: message.databaseEpoch,
        });
      }
    });

    await Bun.sleep(0);
    await Bun.sleep(0);
    await Bun.sleep(0);

    expect(resetMessages).toEqual([
      { type: 'resetForDatabaseEpoch', databaseEpoch: 'new-epoch' },
    ]);
    expect(debugMessages).toContainEqual({
      event: 'database-epoch-change',
      fromEpoch: 'old-epoch',
      toEpoch: 'new-epoch',
    });
    expect(requestBodies).toEqual([
      {
        databaseEpoch: 'old-epoch',
        syncCursor: { tables: { maps: { last_seen_updated_at: 9, permission_hash: 'old' } } },
      },
      { databaseEpoch: 'new-epoch', syncCursor: { tables: {} } },
    ]);
  } finally {
    globalThis.XMLHttpRequest = previousXmlHttpRequest;
  }
});
