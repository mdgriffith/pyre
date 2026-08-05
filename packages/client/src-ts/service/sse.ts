import type { ElmApp } from '../types';
import { resolveEndpointUrl, type DatabaseId } from '../routing';

export interface SSEConfig {
  baseUrl: string;
  eventsPath: string;
  credentials?: RequestCredentials;
  withCredentials?: boolean;
  databaseId?: DatabaseId;
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
    this.attemptConnect();
  }

  private emitMessage(message: LiveSyncMessage): void {
    this.onMessage?.(message);
    this.elmApp?.ports.receiveSSEMessage?.send(message);
    this.debugLog('[PyreClient] port receiveSSEMessage ->', message);
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
      this.debugLog('[PyreClient] SSE EventSource constructed', {
        sseUrl,
        withCredentials: shouldIncludeCredentials(this.config),
      });

      eventSource.onopen = () => {
        this.eventSource = eventSource;
        this.debugLog('[PyreClient] SSE connection opened', { sseUrl });
      };

      eventSource.onmessage = (event: MessageEvent) => {
        try {
          const message = JSON.parse(event.data) as LiveSyncMessage;
          if (message.type === 'connected' && message.connectionId) {
            this.connectionId = message.connectionId;
            this.debugLog('[PyreClient] SSE connected', { connectionId: message.connectionId });
          }
          this.emitMessage(message);
        } catch (error) {
          console.error('Failed to parse SSE message:', error);
        }
      };

      eventSource.onerror = () => {
        const state = eventSource.readyState;
        this.debugLog('[PyreClient] SSE connection state changed', {
          readyState: state,
          connectionId: this.connectionId,
          shouldReconnect: this.shouldReconnect,
        });

        if (state === EventSource.CLOSED) {
          console.warn('[PyreClient] SSE connection closed');
          if (this.shouldReconnect) {
            this.debugLog('[PyreClient] SSE waiting for EventSource auto-reconnect');
          }
        } else if (state === EventSource.CONNECTING && !this.connectionId) {
          this.debugLog('[PyreClient] SSE failed before session established');
          const errorMessage = {
            type: 'error',
            error: 'SSE connection failed',
          };
          this.emitMessage(errorMessage);
        }
      };
    } catch (error) {
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
  }
}

export function buildSSEUrl(config: SSEConfig): string {
  return resolveEndpointUrl(config.baseUrl, config.eventsPath, {
    databaseId: config.databaseId,
  });
}

function shouldIncludeCredentials(config: SSEConfig): boolean {
  return config.credentials === 'include' || config.withCredentials === true;
}
