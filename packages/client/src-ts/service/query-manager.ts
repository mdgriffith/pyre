import type { ElmApp } from '../types';
import { applyQueryDelta } from './query-delta';
import type { LocalEditsRuntime } from './local-edits';
import type { QueryUpdate } from './query-client';

export interface QueryRegistration {
  queryId: string;
  query: unknown;
  input: unknown;
  callbackPort: string;
}

export interface MutationResult {
  ok: boolean;
  value?: unknown;
  error?: string;
}

type QueryResultCallback = (result: unknown) => void;
type MutationResultCallback = (result: MutationResult) => void;

interface QueryState {
  queryId: string;
  callbackPort: string;
  result: unknown;
  revision: number;
}

export class QueryManagerService {
  private elmApp: ElmApp | null = null;
  private debugLog: (...args: unknown[]) => void;
  private queryCallbacks: Map<string, QueryResultCallback> = new Map();
  private callbackPortToQueryId: Map<string, string> = new Map();
  private queryStates: Map<string, QueryState> = new Map();
  private mutationCallbacks: Map<string, MutationResultCallback> = new Map();
  private localEdits?: LocalEditsRuntime;
  private portListener?: (message: unknown) => void;

  setLocalEdits(runtime: LocalEditsRuntime): void { this.localEdits = runtime; }

  sendLocalEdits(message: unknown): void {
    if (!this.elmApp?.ports.receiveQueryManagerMessage) throw new Error('Local edit worker bridge unavailable');
    this.elmApp.ports.receiveQueryManagerMessage.send(message);
  }

  installPublication(updates: QueryUpdate[]): () => void {
    const callbacks: Array<() => void> = [];
    for (const update of updates) {
      const state = this.queryStates.get(update.queryId);
      if (!state || update.revision <= state.revision) continue;
      state.result = update.result;
      state.revision = update.revision;
      const callback = this.queryCallbacks.get(state.callbackPort);
      if (callback) callbacks.push(() => {
        try { callback(update.result); } catch (error) { console.error('[PyreClient] Query listener failed', error); }
      });
    }
    return () => callbacks.forEach(callback => callback());
  }

  constructor(debugLog?: (...args: unknown[]) => void) {
    this.debugLog = debugLog ?? (() => {});
  }

  attachPorts(elmApp: ElmApp): void {
    this.elmApp = elmApp;

    if (elmApp.ports.queryManagerOut) {
      this.portListener = (message) => {
        this.debugLog('[PyreClient] port queryManagerOut <-', message);
        this.handleMessage(message as { type?: string }).catch((error) => {
          console.error('[PyreClient] Query manager handler failed:', error);
        });
      };
      elmApp.ports.queryManagerOut.subscribe(this.portListener);
    }
  }

  detach(): void {
    if (this.portListener) this.elmApp?.ports.queryManagerOut?.unsubscribe?.(this.portListener);
    this.elmApp = null;
    this.localEdits = undefined;
    this.queryCallbacks.clear();
    this.queryStates.clear();
    this.callbackPortToQueryId.clear();
    this.mutationCallbacks.clear();
  }

  registerQuery(registration: QueryRegistration, callback: QueryResultCallback): void {
    this.queryCallbacks.set(registration.callbackPort, callback);
    this.callbackPortToQueryId.set(registration.callbackPort, registration.queryId);
    this.queryStates.set(registration.queryId, {
      queryId: registration.queryId,
      callbackPort: registration.callbackPort,
      result: {},
      revision: 0,
    });
    const registerMessage = {
      type: 'registerQuery',
      queryId: registration.queryId,
      query: registration.query,
      input: registration.input,
      callbackPort: registration.callbackPort,
    };
    this.elmApp?.ports.receiveQueryManagerMessage?.send(registerMessage);
    this.debugLog('[PyreClient] port receiveQueryManagerMessage ->', registerMessage);
  }

  updateQueryInput(queryId: string, input: unknown, query?: unknown): void {
    const updateMessage = {
      type: 'updateQueryInput',
      queryId,
      query,
      input,
    };
    this.elmApp?.ports.receiveQueryManagerMessage?.send(updateMessage);
    this.debugLog('[PyreClient] port receiveQueryManagerMessage ->', updateMessage);
  }

  unregisterQuery(queryId: string, callbackPort: string): void {
    this.queryCallbacks.delete(callbackPort);
    this.callbackPortToQueryId.delete(callbackPort);
    this.queryStates.delete(queryId);
    const unregisterMessage = {
      type: 'unregisterQuery',
      queryId,
    };
    this.elmApp?.ports.receiveQueryManagerMessage?.send(unregisterMessage);
    this.debugLog('[PyreClient] port receiveQueryManagerMessage ->', unregisterMessage);
  }

  sendMutation(
    requestId: string,
    mutationId: string,
    baseUrl: string,
    input: unknown,
    optimistic?: unknown,
    callback?: MutationResultCallback,
    headers?: Record<string, string>,
    credentials?: RequestCredentials,
    withCredentials?: boolean
  ): void {
    if (callback) {
      this.mutationCallbacks.set(requestId, callback);
    }

    const headerPairs = headers
      ? Object.entries(headers)
      : undefined;

    const mutationMessage = {
      type: 'sendMutation',
      requestId,
      mutationId,
      baseUrl,
      input,
      optimistic,
      headers: headerPairs,
      credentials: credentials ?? (withCredentials === true ? 'include' : 'same-origin'),
      withCredentials: credentials === 'include' || withCredentials === true,
    };
    this.elmApp?.ports.receiveQueryManagerMessage?.send(mutationMessage);
    this.debugLog('[PyreClient] port receiveQueryManagerMessage ->', mutationMessage);
  }

  private async handleMessage(message: { type?: string }): Promise<void> {
    if (message.type === 'localEdits') {
      this.localEdits?.receiveEnvelope(message as Parameters<LocalEditsRuntime['receiveEnvelope']>[0]);
      return;
    }
    if (message.type === 'queryResult') {
      const typedMessage = message as { callbackPort?: string; result?: unknown };
      if (!typedMessage.callbackPort) {
        return;
      }
      const queryId = this.callbackPortToQueryId.get(typedMessage.callbackPort);
      if (queryId) {
        const state = this.queryStates.get(queryId);
        if (state) {
          state.result = typedMessage.result ?? {};
          state.revision += 1;
        }
      }
      const callback = this.queryCallbacks.get(typedMessage.callbackPort);
      if (callback) {
        callback(typedMessage.result);
      }
      return;
    }

    if (message.type === 'queryFull') {
      const typedMessage = message as { queryId?: string; revision?: number; result?: unknown };
      if (!typedMessage.queryId || typedMessage.revision === undefined) {
        return;
      }
      const state = this.queryStates.get(typedMessage.queryId);
      if (!state) {
        return;
      }
      if (typedMessage.revision <= state.revision) {
        return;
      }
      state.result = typedMessage.result ?? {};
      state.revision = typedMessage.revision;
      const callback = this.queryCallbacks.get(state.callbackPort);
      if (callback) {
        callback(state.result);
      }
      return;
    }

    if (message.type === 'queryDelta') {
      const typedMessage = message as { queryId?: string; revision?: number; delta?: { ops?: unknown[] } };
      if (!typedMessage.queryId || typedMessage.revision === undefined || !typedMessage.delta) {
        return;
      }
      const state = this.queryStates.get(typedMessage.queryId);
      if (!state) {
        return;
      }
      if (typedMessage.revision <= state.revision) {
        return;
      }
      const { result, errors } = applyQueryDelta(typedMessage.queryId, state.result, typedMessage.delta as any);
      if (errors.length > 0) {
        errors.forEach((error) => {
          console.warn('[PyreClient] QueryDelta op failed:', error);
        });
      }
      state.result = result;
      state.revision = typedMessage.revision;
      const callback = this.queryCallbacks.get(state.callbackPort);
      if (callback) {
        callback(state.result);
      }
      return;
    }

    if (message.type === 'mutationResult') {
      const typedMessage = message as { requestId?: string; result?: MutationResult };
      if (!typedMessage.requestId) {
        return;
      }
      const callback = this.mutationCallbacks.get(typedMessage.requestId);
      if (!callback) {
        return;
      }
      callback(typedMessage.result ?? { ok: false, error: 'Missing mutation result' });
      this.mutationCallbacks.delete(typedMessage.requestId);
    }
  }
}
