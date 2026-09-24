// @ts-nocheck
import { expect, test } from 'bun:test';

import { buildSSEUrl, SSEManager } from './sse';
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

test('SSEManager keeps ephemeral messages out of Elm ports while durable messages are unchanged', () => {
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
    const elm = [];
    const states = [];
    const manager = new SSEManager({
      baseUrl: 'https://api.example.test', eventsPath: '/sync/events',
      databaseId: 'alpha', ephemeralWrite: true,
    }, message => internal.push(message));
    manager.setOnStateChange(state => states.push(state));
    manager.attachPorts({ ports: { receiveSSEMessage: { send: message => elm.push(message) } } });
    manager.connect();
    expect(sources[0].url).toContain('ephemeralWrite=true');
    sources[0].onmessage({ data: JSON.stringify({ type: 'connected', databaseId: 'alpha', connectionId: 'c1' }) });
    sources[0].onmessage({ data: JSON.stringify({ type: 'ephemeralSnapshot', ephemeralSnapshot: {} }) });
    sources[0].onmessage({ data: JSON.stringify({ type: 'delta', databaseId: 'alpha', data: [] }) });
    expect(internal.map(message => message.type)).toEqual(['connected', 'ephemeralSnapshot', 'delta']);
    expect(elm.map(message => message.type)).toEqual(['connected', 'delta']);
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

test('WebSocket lifecycle matches SSE and keeps ephemeral messages internal', () => {
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
    const elm = [];
    const manager = new WebSocketManager({
      baseUrl: 'https://api.example.test', eventsPath: '/events', databaseId: 'alpha', ephemeralWrite: true,
    }, message => internal.push(message));
    manager.setOnStateChange(state => states.push(state));
    manager.attachPorts({ ports: { receiveWebSocketMessage: { send: message => elm.push(message) } } });
    manager.connect();
    expect(sockets[0].url).toContain('ephemeralWrite=true');
    sockets[0].onmessage({ data: JSON.stringify({ type: 'connected', connectionId: 'c1' }) });
    sockets[0].onmessage({ data: JSON.stringify({ type: 'ephemeralSnapshot' }) });
    sockets[0].onmessage({ data: JSON.stringify({ type: 'delta' }) });
    sockets[0].onclose();
    expect(states).toEqual(['connecting', 'disconnected']);
    expect(internal.map(message => message.type)).toEqual(['connected', 'ephemeralSnapshot', 'delta']);
    expect(elm.map(message => message.type)).toEqual(['connected', 'delta']);
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
