// @ts-nocheck
import { expect, test } from 'bun:test';

import { buildSSEUrl, redactEphemeralCapability, SSEManager } from './sse';
import { buildWebSocketUrl, WebSocketManager } from './websocket';

test('buildSSEUrl includes databaseId and preserves base path for existing configs', () => {
  expect(buildSSEUrl({
    baseUrl: 'https://api.example.test/pyre',
    eventsPath: '/sync/events',
    databaseId: 'campaign:123',
  })).toBe('https://api.example.test/pyre/sync/events?databaseId=campaign%3A123');
});

test('buildSSEUrl includes ephemeral write intent', () => {
  expect(buildSSEUrl({
    baseUrl: 'https://api.example.test/pyre',
    eventsPath: '/sync/events',
    databaseId: 'campaign:123',
    ephemeralWrite: false,
  })).toBe('https://api.example.test/pyre/sync/events?databaseId=campaign%3A123&ephemeralWrite=false');
});

test('buildWebSocketUrl includes databaseId and switches protocol', () => {
  expect(buildWebSocketUrl({
    baseUrl: 'https://api.example.test/pyre',
    eventsPath: '/sync/events',
    databaseId: 'campaign:123',
  })).toBe('wss://api.example.test/pyre/sync/events?databaseId=campaign%3A123');
});

test('buildWebSocketUrl includes ephemeral write intent', () => {
  expect(buildWebSocketUrl({
    baseUrl: 'http://api.example.test/pyre', eventsPath: '/sync/events',
    databaseId: 'campaign:123', ephemeralWrite: false,
  })).toBe('ws://api.example.test/pyre/sync/events?databaseId=campaign%3A123&ephemeralWrite=false');
});

test('capability redaction preserves durable fields without mutating the privileged message', () => {
  const message = {
    type: 'connected', databaseId: 'alpha', connectionId: 'c1', databaseEpoch: 'db-1',
    ephemeralEpoch: 'ephemeral-1', ephemeralCapability: 'secret', serverRevision: 7,
  };

  expect(redactEphemeralCapability(message)).toEqual({
    type: 'connected', databaseId: 'alpha', connectionId: 'c1', databaseEpoch: 'db-1',
    ephemeralEpoch: 'ephemeral-1', serverRevision: 7,
  });
  expect(message.ephemeralCapability).toBe('secret');
});

test('SSE delivers capability only to ephemeral state and redacts Elm, debug, and public callbacks', () => {
  const OriginalEventSource = globalThis.EventSource;
  const sources = [];
  class FakeEventSource {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSED = 2;
    readyState = FakeEventSource.OPEN;
    onopen = null;
    onmessage = null;
    onerror = null;
    constructor(url, init) {
      this.url = url;
      this.init = init;
      sources.push(this);
    }
    close() {
      this.readyState = FakeEventSource.CLOSED;
    }
  }
  globalThis.EventSource = FakeEventSource;
  try {
    const internal = [];
    const ephemeral = [];
    const elm = [];
    const debug = [];
    const states = [];
    const manager = new SSEManager({
      baseUrl: 'https://api.example.test', eventsPath: '/sync/events',
      databaseId: 'alpha', ephemeralWrite: true,
    }, message => internal.push(message), (...args) => debug.push(args));
    manager.setOnEphemeralMessage(message => ephemeral.push(message));
    manager.setOnStateChange(state => states.push(state));
    manager.attachPorts({ ports: { receiveSSEMessage: { send: message => elm.push(message) } } });
    manager.connect();
    expect(sources[0].url).toContain('ephemeralWrite=true');
    const connected = {
      type: 'connected', databaseId: 'alpha', connectionId: 'c1', databaseEpoch: 'db-1',
      ephemeralEpoch: 'ephemeral-1', ephemeralCapability: 'sse-secret', serverRevision: 7,
    };
    sources[0].onmessage({ data: JSON.stringify(connected) });
    sources[0].onmessage({ data: JSON.stringify({ type: 'ephemeralSnapshot', ephemeralSnapshot: {} }) });
    sources[0].onmessage({ data: JSON.stringify({ type: 'delta', databaseId: 'alpha', data: [] }) });
    expect(internal.map(message => message.type)).toEqual(['connected', 'ephemeralSnapshot', 'delta']);
    expect(elm.map(message => message.type)).toEqual(['connected', 'delta']);
    expect(internal[0]).toEqual({
      type: 'connected', databaseId: 'alpha', connectionId: 'c1', databaseEpoch: 'db-1',
      ephemeralEpoch: 'ephemeral-1', serverRevision: 7,
    });
    expect(elm[0]).toEqual(internal[0]);
    expect(ephemeral[0]).toEqual(connected);
    expect(JSON.stringify(debug)).not.toContain('sse-secret');
    sources[0].readyState = FakeEventSource.CONNECTING;
    sources[0].onerror();
    expect(states).toEqual(['connecting', 'disconnected']);
    expect(elm.map(message => message.type)).toEqual(['connected', 'delta']);
    manager.disconnect();
    expect(states).toEqual(['connecting', 'disconnected']);
  } finally {
    globalThis.EventSource = OriginalEventSource;
  }
});

test('SSE auto-reconnect emits one disconnect and fences a fresh connected session', () => {
  const OriginalEventSource = globalThis.EventSource;
  const sources = [];
  class FakeEventSource {
    static CONNECTING = 0; static OPEN = 1; static CLOSED = 2;
    readyState = FakeEventSource.OPEN;
    constructor(url) { this.url = url; sources.push(this); }
    close() { this.readyState = FakeEventSource.CLOSED; }
  }
  globalThis.EventSource = FakeEventSource;
  try {
    const states = [];
    const messages = [];
    const manager = new SSEManager({ baseUrl: 'https://api.example.test', eventsPath: '/events' }, message => messages.push(message));
    manager.setOnStateChange(state => states.push(state));
    manager.connect();
    sources[0].onmessage({ data: JSON.stringify({ type: 'connected', connectionId: 'old' }) });
    sources[0].readyState = FakeEventSource.CONNECTING;
    sources[0].onerror();
    sources[0].onerror();
    sources[0].onmessage({ data: JSON.stringify({ type: 'connected', connectionId: 'fresh' }) });
    expect(states).toEqual(['connecting', 'disconnected', 'connecting']);
    expect(messages.map(message => message.connectionId)).toEqual(['old', 'fresh']);
  } finally {
    globalThis.EventSource = OriginalEventSource;
  }
});

test('repeated SSE connect closes the prior source and fences its callbacks', () => {
  const OriginalEventSource = globalThis.EventSource;
  const sources = [];
  class FakeEventSource {
    static CONNECTING = 0; static OPEN = 1; static CLOSED = 2;
    readyState = FakeEventSource.OPEN;
    constructor() { sources.push(this); }
    close() { this.readyState = FakeEventSource.CLOSED; }
  }
  globalThis.EventSource = FakeEventSource;
  try {
    const messages = [];
    const ephemeral = [];
    const states = [];
    const manager = new SSEManager({ baseUrl: 'https://api.example.test', eventsPath: '/events' }, message => messages.push(message));
    manager.setOnEphemeralMessage(message => ephemeral.push(message));
    manager.setOnStateChange(state => states.push(state));
    manager.connect();
    manager.connect();
    expect(sources).toHaveLength(2);
    expect(sources[0].readyState).toBe(FakeEventSource.CLOSED);
    sources[0].onmessage({ data: JSON.stringify({ type: 'connected', connectionId: 'stale', ephemeralCapability: 'stale-secret' }) });
    sources[1].onmessage({ data: JSON.stringify({ type: 'connected', connectionId: 'current', ephemeralCapability: 'current-secret' }) });
    expect(messages.map(message => message.connectionId)).toEqual(['current']);
    expect(ephemeral.map(message => message.connectionId)).toEqual(['current']);
    expect(ephemeral[0].ephemeralCapability).toBe('current-secret');
    expect(states).toEqual(['connecting', 'disconnected', 'connecting']);
  } finally {
    globalThis.EventSource = OriginalEventSource;
  }
});

test('WebSocket delivers capability only to ephemeral state and redacts Elm, debug, and public callbacks', () => {
  const OriginalWebSocket = globalThis.WebSocket;
  const sockets = [];
  class FakeWebSocket {
    constructor(url) { this.url = url; sockets.push(this); }
    close() { this.onclose?.(); }
  }
  globalThis.WebSocket = FakeWebSocket;
  try {
    const states = [];
    const internal = [];
    const ephemeral = [];
    const elm = [];
    const debug = [];
    const manager = new WebSocketManager({
      baseUrl: 'https://api.example.test', eventsPath: '/events', databaseId: 'alpha', ephemeralWrite: true,
    }, message => internal.push(message), (...args) => debug.push(args));
    manager.setOnEphemeralMessage(message => ephemeral.push(message));
    manager.setOnStateChange(state => states.push(state));
    manager.attachPorts({ ports: { receiveWebSocketMessage: { send: message => elm.push(message) } } });
    manager.connect();
    expect(sockets[0].url).toContain('ephemeralWrite=true');
    const connected = {
      type: 'connected', databaseId: 'alpha', connectionId: 'c1', databaseEpoch: 'db-1',
      ephemeralEpoch: 'ephemeral-1', ephemeralCapability: 'websocket-secret', serverRevision: 7,
    };
    sockets[0].onmessage({ data: JSON.stringify(connected) });
    sockets[0].onmessage({ data: JSON.stringify({ type: 'ephemeralSnapshot' }) });
    sockets[0].onmessage({ data: JSON.stringify({ type: 'delta' }) });
    sockets[0].onclose();
    expect(states).toEqual(['connecting', 'disconnected']);
    expect(internal.map(message => message.type)).toEqual(['connected', 'ephemeralSnapshot', 'delta']);
    expect(elm.map(message => message.type)).toEqual(['connected', 'delta']);
    expect(internal[0]).toEqual({
      type: 'connected', databaseId: 'alpha', connectionId: 'c1', databaseEpoch: 'db-1',
      ephemeralEpoch: 'ephemeral-1', serverRevision: 7,
    });
    expect(elm[0]).toEqual(internal[0]);
    expect(ephemeral[0]).toEqual(connected);
    expect(JSON.stringify(debug)).not.toContain('websocket-secret');
    manager.disconnect();
    expect(states).toEqual(['connecting', 'disconnected']);
  } finally {
    globalThis.WebSocket = OriginalWebSocket;
  }
});

test('WebSocketManager validates reconnect cadence', () => {
  expect(() => new WebSocketManager({ baseUrl: 'https://api.example.test', eventsPath: '/events', reconnectDelayMs: -1 })).toThrow('finite non-negative');
  expect(() => new WebSocketManager({ baseUrl: 'https://api.example.test', eventsPath: '/events', reconnectDelayMs: Infinity })).toThrow('finite non-negative');
});

test('repeated WebSocket connect closes the prior socket and fences its callbacks', () => {
  const OriginalWebSocket = globalThis.WebSocket;
  const sockets = [];
  class FakeWebSocket {
    constructor() { sockets.push(this); }
    close() { this.closed = true; this.onclose?.(); }
  }
  globalThis.WebSocket = FakeWebSocket;
  try {
    const messages = [];
    const ephemeral = [];
    const states = [];
    const manager = new WebSocketManager({ baseUrl: 'https://api.example.test', eventsPath: '/events' }, message => messages.push(message));
    manager.setOnEphemeralMessage(message => ephemeral.push(message));
    manager.setOnStateChange(state => states.push(state));
    manager.connect();
    manager.connect();
    expect(sockets).toHaveLength(2);
    expect(sockets[0].closed).toBe(true);
    sockets[0].onmessage({ data: JSON.stringify({ type: 'connected', connectionId: 'stale', ephemeralCapability: 'stale-secret' }) });
    sockets[1].onmessage({ data: JSON.stringify({ type: 'connected', connectionId: 'current', ephemeralCapability: 'current-secret' }) });
    expect(messages.map(message => message.connectionId)).toEqual(['current']);
    expect(ephemeral.map(message => message.connectionId)).toEqual(['current']);
    expect(ephemeral[0].ephemeralCapability).toBe('current-secret');
    expect(states).toEqual(['connecting', 'disconnected', 'connecting']);
    manager.disconnect();
  } finally {
    globalThis.WebSocket = OriginalWebSocket;
  }
});
