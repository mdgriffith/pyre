import type { ElmApp } from '../types';
import { resolveEndpointUrl, type DatabaseId } from '../routing';

export interface SSEConfig {
  baseUrl: string;
  eventsPath: string;
  credentials?: RequestCredentials;
  withCredentials?: boolean;
  databaseId?: DatabaseId;
  ephemeralWrite?: boolean;
}

export interface LiveSyncMessage {
  type: string;
  databaseId?: string;
  connectionId?: string;
  serverRevision?: number;
  databaseEpoch?: string;
  data?: unknown;
  error?: string;
}

export class SSEManager {
  private eventSource: EventSource | null = null;
  private connectionId: string | null = null;
  private config: SSEConfig | null = null;
  private shouldReconnect = true;
  private onMessage: ((message: LiveSyncMessage) => void) | null = null;
  private elmApp: ElmApp | null = null;
  private debugLog: (...args: unknown[]) => void;
  private onStateChange: ((state: 'connecting' | 'disconnected') => void) | null = null;
  private state: 'connecting' | 'disconnected' = 'disconnected';

  constructor(
    config: SSEConfig,
    onMessage?: (message: LiveSyncMessage) => void,
    debugLog?: (...args: unknown[]) => void
  ) {
    this.config = config;
    this.onMessage = onMessage ?? null;
    this.debugLog = debugLog ?? (() => {});
  }

  setOnMessage(callback: (message: LiveSyncMessage) => void): void {
    this.onMessage = callback;
  }

  setOnStateChange(callback: (state: 'connecting' | 'disconnected') => void): void {
    this.onStateChange = callback;
  }

  attachPorts(elmApp: ElmApp): void {
    this.elmApp = elmApp;

    if (elmApp.ports.sseOut) {
      this.debugLog('[PyreClient] SSE port attached');
      elmApp.ports.sseOut.subscribe((message) => {
        this.debugLog('[PyreClient] port sseOut <-', message);
        const typedMessage = message as { type?: string };
        if (typedMessage.type === 'connectSSE') {
          this.connect();
        } else if (typedMessage.type === 'disconnectSSE') {
          this.disconnect();
        } else {
          this.debugLog('[PyreClient] SSE ignored unknown port message', message);
        }
      });
    } else {
      this.debugLog('[PyreClient] SSE port missing: sseOut');
    }
  }

  connect(): void {
    this.shouldReconnect = true;
    this.debugLog('[PyreClient] SSE connect requested');
    this.setState('connecting');
    if (this.eventSource) {
      this.eventSource.close();
      this.eventSource = null;
      this.connectionId = null;
    }
    this.attemptConnect();
  }

  private emitMessage(message: LiveSyncMessage): void {
    this.onMessage?.(message);
    if (!message.type.startsWith('ephemeral')) {
      this.elmApp?.ports.receiveSSEMessage?.send(message);
      this.debugLog('[PyreClient] port receiveSSEMessage ->', message);
    }
  }

  private attemptConnect(): void {
    if (!this.config) {
      this.debugLog('[PyreClient] SSE connect skipped: missing config');
      return;
    }

    try {
      const sseUrl = buildSSEUrl(this.config);
      this.debugLog('[PyreClient] SSE attempting connection', { sseUrl });
      const eventSource = new EventSource(sseUrl, {
        withCredentials: shouldIncludeCredentials(this.config),
      });
      this.eventSource = eventSource;
      this.debugLog('[PyreClient] SSE EventSource constructed', {
        sseUrl,
        withCredentials: shouldIncludeCredentials(this.config),
      });

      eventSource.onopen = () => {
        if (this.eventSource !== eventSource) return;
        this.debugLog('[PyreClient] SSE connection opened', { sseUrl });
      };

      eventSource.onmessage = (event: MessageEvent) => {
        if (this.eventSource !== eventSource) return;
        try {
          const message = JSON.parse(event.data) as LiveSyncMessage;
          if (message.type === 'connected' && message.connectionId) {
            this.setState('connecting');
            this.connectionId = message.connectionId;
            this.debugLog('[PyreClient] SSE connected', { connectionId: message.connectionId });
          }
          this.emitMessage(message);
        } catch (error) {
          console.error('Failed to parse SSE message:', error);
        }
      };

      eventSource.onerror = () => {
        if (this.eventSource !== eventSource) return;
        const state = eventSource.readyState;
        const hadConnection = this.connectionId !== null;
        const wasDisconnected = this.state === 'disconnected';
        this.debugLog('[PyreClient] SSE connection state changed', {
          readyState: state,
          connectionId: this.connectionId,
          shouldReconnect: this.shouldReconnect,
        });
        this.connectionId = null;
        this.setState('disconnected');

        if (state === EventSource.CLOSED) {
          console.warn('[PyreClient] SSE connection closed');
          if (this.shouldReconnect) {
            this.debugLog('[PyreClient] SSE waiting for EventSource auto-reconnect');
          }
        } else if (state === EventSource.CONNECTING && !hadConnection && !wasDisconnected) {
          this.debugLog('[PyreClient] SSE failed before session established');
          const errorMessage = {
            type: 'error',
            error: 'SSE connection failed',
          };
          this.emitMessage(errorMessage);
        }
      };
    } catch (error) {
      this.setState('disconnected');
      const errorMessage = {
        type: 'error',
        error: `SSE connection error: ${error}`,
      };
      this.emitMessage(errorMessage);
    }
  }

  disconnect(): void {
    this.shouldReconnect = false;
    this.debugLog('[PyreClient] SSE disconnect requested', { connectionId: this.connectionId });

    if (this.eventSource) {
      this.eventSource.close();
      this.eventSource = null;
    }

    this.connectionId = null;
    this.setState('disconnected');
  }

  private setState(state: 'connecting' | 'disconnected'): void {
    if (this.state === state) return;
    this.state = state;
    this.onStateChange?.(state);
  }
}

export function buildSSEUrl(config: SSEConfig): string {
  return resolveEndpointUrl(config.baseUrl, config.eventsPath, {
    databaseId: config.databaseId,
    ephemeralWrite: config.ephemeralWrite === undefined ? undefined : String(config.ephemeralWrite),
  });
}

function shouldIncludeCredentials(config: SSEConfig): boolean {
  return config.credentials === 'include' || config.withCredentials === true;
}
