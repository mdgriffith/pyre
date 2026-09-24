import type { SchemaMetadata } from '@pyre/core';

export type {
  FilterPlaceholder,
  FilterValue,
  GeneratedQueryShape,
  QueryField,
  QueryVariableReference,
  QueryShape,
  SchemaMetadata,
  RejectedQueryShape,
  SortClause,
  SortDirection,
  WhereClause,
} from '@pyre/core';

export interface ServerEndpoints {
  catchup: string;
  events: string;
  query: string;
  ephemeralConnection?: string;
  ephemeralShared?: string;
  ephemeralLease?: string;
  ephemeralResnapshot?: string;
}

export type ServerHeaders = Record<string, string> | (() => Record<string, string> | Promise<Record<string, string>>);

export interface ServerConfig {
  baseUrl: string;
  endpoints?: Partial<ServerEndpoints>;
  headers?: ServerHeaders;
  credentials?: RequestCredentials;
  withCredentials?: boolean;
  liveSyncTransport?: LiveSyncTransport;
  /** Advertise write intent to the ephemeral live subscription. Defaults to true. */
  ephemeralWrite?: boolean;
  /** Minimum time between requests for each ephemeral patch kind. Defaults to 50ms. */
  ephemeralMaxUpdateCadenceMs?: number;
  /** Time between independent ephemeral lease renewals. Defaults to 10 seconds. */
  ephemeralLeaseCadenceMs?: number;
}

export interface SyncProgress {
  table?: string;
  tablesSynced: number;
  totalTables?: number;
  complete: boolean;
  error?: string;
}

export type SyncStatus = 'not_started' | 'catching_up' | 'live';

export type TableSyncStatus = 'waiting' | 'catching_up' | 'live';

export interface SyncState {
  status: SyncStatus;
  tables: Record<string, TableSyncStatus>;
  error?: string;
}

export type LiveSyncTransport = 'sse' | 'websocket';

export interface ElmPorts {
  visibleStateOut?: {
    subscribe: (callback: (message: { source: import('./service/entity-stream').EntityChangeBatchSource; data: import('./service/entity-stream').ServerTableGroup[]; snapshot: import('./service/entity-stream').ServerTableGroup[] }) => void) => void;
  };
  indexedDbOut?: {
    subscribe: (callback: (message: unknown) => void) => void;
  };
  sseOut?: {
    subscribe: (callback: (message: unknown) => void) => void;
  };
  webSocketOut?: {
    subscribe: (callback: (message: unknown) => void) => void;
  };
  queryManagerOut?: {
    subscribe: (callback: (message: unknown) => void) => void;
  };
  queryClientOut?: {
    subscribe: (callback: (message: unknown) => void) => void;
  };
  errorOut?: {
    subscribe: (callback: (message: string) => void) => void;
  };
  syncStateOut?: {
    subscribe: (callback: (message: unknown) => void) => void;
  };
  debugOut?: {
    subscribe: (callback: (message: unknown) => void) => void;
  };
  receiveIndexedDbMessage?: {
    send: (message: unknown) => void;
  };
  receiveSSEMessage?: {
    send: (message: unknown) => void;
  };
  receiveWebSocketMessage?: {
    send: (message: unknown) => void;
  };
  receiveQueryManagerMessage?: {
    send: (message: unknown) => void;
  };
  receiveQueryClientMessage?: {
    send: (message: unknown) => void;
  };
  receiveSyncControlMessage?: {
    send: (message: unknown) => void;
  };
}

export interface ElmApp {
  ports: ElmPorts;
}

export interface ElmFlags {
  schema: SchemaMetadata;
  server: {
    baseUrl: string;
    catchupPath: string;
    headers: Array<[string, string]>;
    credentials: RequestCredentials;
    withCredentials: boolean;
  };
  liveSync: {
    transport: LiveSyncTransport;
  };
  sync?: {
    autoStart: boolean;
  };
}

export interface ElmModule {
  Main: {
    init: (config: { flags: ElmFlags }) => ElmApp;
  };
}
