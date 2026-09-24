import type { LiveSyncMessage } from './sse';
import { resolveEndpointUrl, type DatabaseId } from '../routing';

export interface WebSocketConfig {
  baseUrl: string;
  eventsPath: string;
  databaseId?: DatabaseId;
  reconnectDelayMs?: number;
  ephemeralWrite?: boolean;
}

export type LiveTransportState = 'connecting' | 'disconnected';

import type { ElmApp } from '../types';

export class WebSocketManager {
  private socket: WebSocket | null = null;
  private config: WebSocketConfig;
  private shouldReconnect = true;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private onMessage: ((message: LiveSyncMessage) => void) | null = null;
  private elmApp: ElmApp | null = null;
  private debugLog: (...args: unknown[]) => void;
  private onStateChange: ((state: LiveTransportState) => void) | null = null;
  private state: LiveTransportState = 'disconnected';

  constructor(
    config: WebSocketConfig,
    onMessage?: (message: LiveSyncMessage) => void,
    debugLog?: (...args: unknown[]) => void
  ) {
    if (config.reconnectDelayMs !== undefined
      && (!Number.isFinite(config.reconnectDelayMs) || config.reconnectDelayMs < 0)) {
      throw new TypeError('WebSocket reconnectDelayMs must be a finite non-negative number');
    }
    this.config = config;
    this.onMessage = onMessage ?? null;
    this.debugLog = debugLog ?? (() => {});
  }

  setOnMessage(callback: (message: LiveSyncMessage) => void): void {
    this.onMessage = callback;
  }

  setOnStateChange(callback: (state: LiveTransportState) => void): void {
    this.onStateChange = callback;
  }

  attachPorts(elmApp: ElmApp): void {
    this.elmApp = elmApp;

    if (elmApp.ports.webSocketOut) {
      elmApp.ports.webSocketOut.subscribe((message) => {
        this.debugLog('[PyreClient] port webSocketOut <-', message);
        const typedMessage = message as { type?: string };
        if (typedMessage.type === 'connectWebSocket') {
          this.connect();
        } else if (typedMessage.type === 'disconnectWebSocket') {
          this.disconnect();
        }
      });
    }
  }

  connect(): void {
    this.shouldReconnect = true;
    this.setState('connecting');
    this.openSocket();
  }

  private emitMessage(message: LiveSyncMessage): void {
    this.onMessage?.(message);
    if (!message.type.startsWith('ephemeral')) {
      this.elmApp?.ports.receiveWebSocketMessage?.send(message);
      this.debugLog('[PyreClient] port receiveWebSocketMessage ->', message);
    }
  }

  private openSocket(): void {
    const wsUrl = this.buildWebSocketUrl();
    const socket = new WebSocket(wsUrl);
    this.socket = socket;

    socket.onopen = () => {
      if (this.socket !== socket) return;
    };

    socket.onmessage = (event) => {
      if (this.socket !== socket) return;
      if (typeof event.data !== 'string') {
        return;
      }
      try {
        const message = JSON.parse(event.data) as LiveSyncMessage;
        if (message.type === 'connected') this.setState('connecting');
        this.emitMessage(message);
      } catch (error) {
        console.error('[PyreClient] Failed to parse WebSocket message:', error);
        const errorMessage = {
          type: 'error',
          error: 'Failed to parse WebSocket message',
        };
        this.emitMessage(errorMessage);
      }
    };

    socket.onerror = () => {
      if (this.socket !== socket) return;
      const errorMessage = {
        type: 'error',
        error: 'WebSocket connection error',
      };
      this.emitMessage(errorMessage);
    };

    socket.onclose = () => {
      if (this.socket !== socket) return;
      this.socket = null;
      this.setState('disconnected');
      if (!this.shouldReconnect) {
        return;
      }
      if (this.reconnectTimer !== null) {
        return;
      }
      const delay = this.config.reconnectDelayMs ?? 1000;
      this.reconnectTimer = globalThis.setTimeout(() => {
        this.reconnectTimer = null;
        if (this.shouldReconnect) {
          this.setState('connecting');
          this.openSocket();
        }
      }, delay);
    };
  }

  private buildWebSocketUrl(): string {
    const url = new URL(resolveEndpointUrl(this.config.baseUrl, this.config.eventsPath, {
      databaseId: this.config.databaseId,
      ephemeralWrite: this.config.ephemeralWrite === undefined ? undefined : String(this.config.ephemeralWrite),
    }));
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
    return url.toString();
  }

  disconnect(): void {
    this.shouldReconnect = false;
    if (this.reconnectTimer !== null) {
      globalThis.clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.socket) {
      const socket = this.socket;
      this.socket = null;
      socket.close();
    }
    this.setState('disconnected');
  }

  private setState(state: LiveTransportState): void {
    if (this.state === state) return;
    this.state = state;
    this.onStateChange?.(state);
  }
}

export function buildWebSocketUrl(config: WebSocketConfig): string {
  const url = new URL(resolveEndpointUrl(config.baseUrl, config.eventsPath, {
    databaseId: config.databaseId,
    ephemeralWrite: config.ephemeralWrite === undefined ? undefined : String(config.ephemeralWrite),
  }));
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  return url.toString();
}
