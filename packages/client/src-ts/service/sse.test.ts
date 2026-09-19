// @ts-nocheck
import { afterEach, expect, test } from 'bun:test';
import { SSEManager } from './sse';

const originalEventSource = globalThis.EventSource;

afterEach(() => {
  globalThis.EventSource = originalEventSource;
});

test('SSE reconnect clears the stale connection capability', () => {
  const sources = [];
  class MockEventSource {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSED = 2;
    readyState = MockEventSource.CONNECTING;
    onopen = null;
    onmessage = null;
    onerror = null;
    constructor() { sources.push(this); }
    close() { this.readyState = MockEventSource.CLOSED; }
  }
  globalThis.EventSource = MockEventSource;
  const messages = [];
  const manager = new SSEManager({ baseUrl: 'https://example.test', eventsPath: '/events' });
  manager.setOnMessage(message => messages.push(message));

  manager.connect();
  const source = sources[0];
  source.readyState = MockEventSource.OPEN;
  source.onopen();
  source.onmessage({ data: JSON.stringify({ type: 'connected', connectionId: 'capability' }) });
  source.readyState = MockEventSource.CONNECTING;
  source.onerror();

  expect(messages).toEqual([
    { type: 'connected', connectionId: 'capability' },
    { type: 'disconnected' },
  ]);
});
