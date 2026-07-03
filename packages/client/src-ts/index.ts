import loadElm from '../dist/engine.mjs';
import { IndexedDBStorage, IndexedDbService } from './service/indexeddb';
import {
  EntityStreamService,
  validateEntitySubscription,
  type EntityChangeBatch,
  type EntitySubscription,
  type ServerTableGroup,
} from './service/entity-stream';
import { QueryClientService } from './service/query-client';
import { QueryManagerService, type MutationResult } from './service/query-manager';
import { SSEManager, type LiveSyncMessage } from './service/sse';
import { WebSocketManager } from './service/websocket';
import {
  deriveIndexedDbName,
  requireDatabaseId,
  resolveEndpointUrl,
  type CacheNamespace,
  type DatabaseId,
} from './routing';
import {
  registerPyreDevtoolsClient,
  unregisterPyreDevtoolsClient,
} from './devtools-registry';
import type {
  ElmApp,
  LiveSyncTransport,
  SchemaMetadata,
  ServerConfig,
  ServerEndpoints,
  SyncState,
  SyncProgress,
  TableSyncStatus,
} from './types';

export type {
  LiveSyncTransport,
  ServerConfig,
  ServerEndpoints,
  ServerHeaders,
  SyncProgress,
  SyncState,
  SyncStatus,
  TableSyncStatus,
} from './types';
export type { MutationResult } from './service/query-manager';
export type {
  EntityChange,
  EntityChangeBatch,
  EntityChangeBatchSource,
  EntitySubscription,
  EntityTableSubscription,
  EntityWhere,
  EntityWhereValue,
} from './service/entity-stream';
export type { CacheNamespace, DatabaseId } from './routing';

interface PyreBridgeClient {
  run<Input = unknown>(
    databaseId: DatabaseId,
    queryModule: QueryModule<Input>,
    input: Input,
    callback: (result: unknown) => void
  ): QuerySubscription<Input> | void | Promise<QuerySubscription<Input> | void>;
}

export interface QueryModule<Input = unknown> {
  operation: 'query' | 'insert' | 'update' | 'delete' | 'mutation';
  id?: string;
  source?: unknown;
  queryShape?: unknown;
  toQueryShape?: (input: Input) => unknown;
  optimistic?: unknown | ((input: Input) => unknown);
}

export interface QuerySubscription<Input = unknown> {
  unsubscribe(): void;
  update(input: Input): void;
}

export interface RegisteredQuery<Input = unknown> {
  databaseId: DatabaseId;
  queryId: string;
  querySource: unknown;
  input: Input;
  queryName?: string;
}

export interface RegisteredQueryResult {
  queryId: string;
  queryName?: string;
  revision: number;
  result: unknown;
}

export interface PyreDevtoolsEvent {
  id: number;
  timestamp: number;
  type: string;
  payload?: unknown;
}

export type DevtoolsDatabaseLifecycle = 'not_started' | 'queued' | 'syncing' | 'live' | 'unsynced' | 'error';

export interface PyreDevtoolsTableSnapshot {
  name: string;
  count: number;
  rows?: unknown[];
  sync?: TableSyncStatus;
  cursor?: {
    last_seen_updated_at: number | null;
    permission_hash: string;
  };
}

export interface DevtoolsTableSummary {
  name: string;
  count?: number;
  sync?: TableSyncStatus;
  cursor?: {
    last_seen_updated_at: number | null;
    permission_hash: string;
  };
}

export interface DevtoolsDatabaseSummary {
  databaseId: string;
  indexedDbName: string;
  flaggedForSync: boolean;
  lifecycle: DevtoolsDatabaseLifecycle;
  syncState?: SyncState;
  tableSummaries?: DevtoolsTableSummary[];
  error?: string;
}

export interface DevtoolsInstanceSummary {
  instanceId: string;
  label: string;
  cacheNamespace?: string;
}

export interface PyreDevtoolsRegistrySnapshot {
  instances: DevtoolsInstanceSummary[];
}

export interface PyreDevtoolsInstanceSnapshot {
  instanceId: string;
  label: string;
  cacheNamespace?: string;
  schema: SchemaMetadata;
  server: PyreDevtoolsSnapshot['server'];
  aggregateSyncState: SyncState;
  syncProgress: SyncProgress | null;
  debugValues: Record<string, unknown>;
  databases: DevtoolsDatabaseSummary[];
  events: PyreDevtoolsEvent[];
}

export interface PyreDevtoolsTablePageRequest {
  instanceId: string;
  databaseId: string;
  tableName: string;
  offset?: number;
  limit?: number;
  filter?: unknown;
  sort?: unknown;
}

export interface PyreDevtoolsTablePage {
  rows: unknown[];
  offset: number;
  limit: number;
  hasMore: boolean;
}

export interface PyreDevtoolsSnapshot {
  schema: SchemaMetadata;
  indexedDbName: string;
  server: {
    baseUrl: string;
    endpoints: ServerEndpoints;
    liveSyncTransport: LiveSyncTransport;
    hasHeaders: boolean;
    credentials: RequestCredentials;
    withCredentials: boolean;
  };
  connectionId: string | null;
  syncState: SyncState;
  syncProgress: SyncProgress | null;
  debugValues: Record<string, unknown>;
  tables: Record<string, PyreDevtoolsTableSnapshot>;
}

export interface ElmBridgePort {
  subscribe?: (callback: (message: unknown) => void) => void;
  unsubscribe?: (callback: (message: unknown) => void) => void;
  send?: (message: unknown) => void;
}

export interface ElmBridgeApp {
  ports?: Record<string, ElmBridgePort | undefined>;
}

export interface ElmBridgeQueryMessage {
  type: 'register' | 'update-input' | 'unregister';
  databaseId: DatabaseId;
  queryId: string;
  queryName?: string;
  querySource?: unknown;
  queryInput?: unknown;
}

export interface ElmBridgeMutationMessage {
  type: 'mutate';
  databaseId: DatabaseId;
  requestId: string;
  mutationId: string;
  mutationName?: string;
  mutationInput?: unknown;
  optimistic?: unknown;
}

export interface ElmBridgeMutationResultMessage {
  type: 'mutation-result';
  requestId: string;
  mutationId: string;
  mutationName: string | null;
  result: MutationResult;
}

export interface ElmBridgeEntityStreamRegisterMessage {
  type: 'register-entity-stream';
  databaseId: DatabaseId;
  streamId: string;
  tables: EntitySubscription['tables'];
}

export interface ElmBridgeEntityStreamUnregisterMessage {
  type: 'unregister-entity-stream';
  streamId: string;
}

export interface ElmBridgeEntityChangeBatchMessage extends EntityChangeBatch {
  streamId: string;
}

interface EntityBridgeRegistration {
  databaseId?: DatabaseId;
  tables: EntitySubscription['tables'];
  unsubscribePromise: Promise<() => void>;
}

export type ElmBridgeIncomingMessage = ElmBridgeQueryMessage | ElmBridgeMutationMessage | ElmBridgeEntityStreamRegisterMessage | ElmBridgeEntityStreamUnregisterMessage;

export interface ElmBridgeConfig {
  app: ElmBridgeApp;
  receivePort?: string;
  queryResultPort?: string;
  entityChangesPort?: string;
  syncStatePort?: string;
  mutationResultPort?: string;
  onMutation?: (payload: {
    client: PyreBridgeClient;
    message: ElmBridgeMutationMessage;
  }) => Promise<void> | void;
  onError?: (error: Error, context: { phase: 'incoming-message' | 'mutation-handler' }) => void;
}

export interface PyreElmConfig extends ElmBridgeConfig {}

interface SingleDatabasePyreClientConfig {
  schema: SchemaMetadata;
  server: ServerConfig;
  databaseId?: DatabaseId;
  cacheNamespace?: CacheNamespace;
  autoStartSync?: boolean;
  resolvedHeaders?: Record<string, string>;
  indexedDbName?: string;
  debug?: boolean;
  onError?: (error: Error) => void;
  session?: Record<string, unknown>;
  elm?: PyreElmConfig;
}

interface SingleDatabasePyreClientCreateConfig {
  schema: SchemaMetadata;
  server?: ServerConfig;
  databaseId?: DatabaseId;
  cacheNamespace?: CacheNamespace;
  autoStartSync?: boolean;
  indexedDbName?: string;
  debug?: boolean;
  onError?: (error: Error) => void;
  session?: Record<string, unknown>;
  connect?: () => Promise<{
    server: ServerConfig;
    databaseId?: DatabaseId;
    cacheNamespace?: CacheNamespace;
    session?: Record<string, unknown>;
  }> | {
    server: ServerConfig;
    databaseId?: DatabaseId;
    cacheNamespace?: CacheNamespace;
    session?: Record<string, unknown>;
  };
  elm?: PyreElmConfig;
}

export type PyreInternalClient = Pick<SingleDatabasePyreClient, 'run' | 'disconnect' | 'getDevtoolsSnapshot' | 'inspectDevtoolsTablePage' | 'startSync' | 'onSyncState' | 'setSession' | 'onEntityChanges' | 'onDevtoolsEvent'>;

type PyreInternalClientFactory = (config: SingleDatabasePyreClientCreateConfig & {
  databaseId: DatabaseId;
  cacheNamespace: CacheNamespace;
}) => Promise<PyreInternalClient>;

export interface PyreClientConfig {
  schema: SchemaMetadata;
  server?: ServerConfig;
  cacheNamespace?: CacheNamespace;
  indexedDbName?: string;
  debug?: boolean;
  onError?: (error: Error) => void;
  session?: Record<string, unknown>;
  connect?: () => Promise<{
    server: ServerConfig;
    cacheNamespace?: CacheNamespace;
    session?: Record<string, unknown>;
  }> | {
    server: ServerConfig;
    cacheNamespace?: CacheNamespace;
    session?: Record<string, unknown>;
  };
  createInternalClient?: PyreInternalClientFactory;
  elm?: PyreElmConfig;
}

interface ResolvedPyreClientConfig {
  schema: SchemaMetadata;
  server: ServerConfig;
  cacheNamespace: CacheNamespace;
  indexedDbName?: string;
  debug?: boolean;
  onError?: (error: Error) => void;
  session?: Record<string, unknown>;
  createInternalClient: PyreInternalClientFactory;
  elm?: PyreElmConfig;
}

class SingleDatabasePyreClient {
  private elmApp: ElmApp;
  private storage: IndexedDBStorage;
  private indexedDbService: IndexedDbService;
  private sseManager: SSEManager;
  private webSocketManager: WebSocketManager;
  private connectionId: string | null = null;
  private syncStateCallbacks: Set<(state: SyncState) => void> = new Set();
  private syncProgressCallbacks: Set<(progress: SyncProgress) => void> = new Set();
  private connectionCallbacks: Set<(connectionId: string) => void> = new Set();
  private lastSyncState: SyncState;
  private lastSyncProgress: SyncProgress | null = null;
  private pendingLiveState: SyncState | null = null;
  private pendingLiveQueries: Set<string> = new Set();
  private lastAppliedServerRevision: number | null = null;
  private queryManager: QueryManagerService;
  private queryClient: QueryClientService;
  private entityStream: EntityStreamService;
  private bridgeCleanup: (() => void) | null = null;
  private debug: boolean;
  private session: Record<string, unknown>;
  private server: ServerConfig;
  private endpoints: ServerEndpoints;
  private queryCounter = 0;
  private mutationCounter = 0;
  private schema: SchemaMetadata;
  private indexedDbName: string;
  private databaseId?: DatabaseId;
  private devtoolsDebugValues: Record<string, unknown> = {};
  private devtoolsEventCounter = 0;
  private devtoolsEventCallbacks: Set<(event: PyreDevtoolsEvent) => void> = new Set();

  constructor(config: SingleDatabasePyreClientConfig) {
    const baseIndexedDbName = config.indexedDbName ?? 'pyre-client';
    const dbName = config.cacheNamespace && config.databaseId
      ? deriveIndexedDbName(baseIndexedDbName, config.cacheNamespace, config.databaseId)
      : baseIndexedDbName;
    const liveSyncTransport = config.server.liveSyncTransport ?? 'sse';
    this.debug = config.debug ?? false;
    this.server = config.server;
    this.schema = config.schema;
    this.indexedDbName = dbName;
    this.databaseId = config.databaseId;
    this.endpoints = {
      catchup: '/sync',
      events: '/sync/events',
      query: '/db',
      ...config.server.endpoints,
    };
    this.lastSyncState = createInitialSyncState(config.schema);
    this.lastSyncProgress = toSyncProgress(this.lastSyncState);
    this.session = config.session ?? {};

    const elmScope = Object.create(globalThis) as typeof globalThis & { Elm?: unknown };
    elmScope.Elm = undefined;
    let Elm: any;
    try {
      Elm = loadElm(elmScope);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      throw new Error(`[PyreClient ctor] loadElm failed: ${message}`);
    }

    try {
      const initialHeaders = config.resolvedHeaders ?? getStaticServerHeaders(config.server);
      this.elmApp = Elm.Main.init({
        flags: {
          schema: config.schema,
          server: {
            baseUrl: config.server.baseUrl,
            catchupPath: this.endpoints.catchup,
            databaseId: config.databaseId ?? null,
            headers: serverHeadersToPairs(initialHeaders),
            credentials: getServerCredentials(config.server),
            withCredentials: shouldIncludeCredentials(config.server),
          },
          liveSync: {
            transport: liveSyncTransport,
          },
          sync: {
            autoStart: config.autoStartSync ?? true,
          },
        },
      });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      throw new Error(`[PyreClient ctor] Elm.Main.init failed: ${message}`);
    }

    this.storage = new IndexedDBStorage(dbName);
    this.entityStream = new EntityStreamService();
    this.indexedDbService = new IndexedDbService(this.storage, this.logDebug, (tableGroups, source) => {
      this.entityStream.handleTableDelta(tableGroups, source, this.databaseId);
    });
    this.sseManager = new SSEManager({
      baseUrl: config.server.baseUrl,
      eventsPath: this.endpoints.events,
      credentials: config.server.credentials,
      withCredentials: config.server.withCredentials,
      databaseId: config.databaseId,
    }, undefined, this.logDebug);
    this.webSocketManager = new WebSocketManager({
      baseUrl: config.server.baseUrl,
      eventsPath: this.endpoints.events,
      databaseId: config.databaseId,
    }, undefined, this.logDebug);
    this.queryManager = new QueryManagerService(this.logDebug);
    this.queryClient = new QueryClientService(() => this.session, (payload) => {
      if (config.onError) {
        config.onError(new Error(payload.message));
      } else {
        console.error('[PyreClient] QueryClient error', payload);
      }
    }, this.logDebug);
    this.queryClient.setOnQueryResult(this.handleQueryResult);
    this.queryClient.setOnQueryUnregister(this.handleQueryUnregister);

    this.indexedDbService.attachPorts(this.elmApp);
    this.sseManager.setOnMessage(this.handleLiveSyncMessage);
    this.webSocketManager.setOnMessage(this.handleLiveSyncMessage);
    this.sseManager.attachPorts(this.elmApp);
    this.webSocketManager.attachPorts(this.elmApp);
    this.queryManager.attachPorts(this.elmApp);
    this.queryClient.attachPorts(this.elmApp);

    if (this.elmApp.ports.debugOut) {
      this.elmApp.ports.debugOut.subscribe((message) => {
        this.logDebug('[PyreClient] port debugOut <-', message);
        const eventName = debugOutEventName(message);
        if (eventName) {
          this.emitDevtoolsEvent(`elm:${eventName}`, message);
        }
      });
    } else {
      this.logDebug('[PyreClient] port debugOut unavailable');
    }

    if (this.elmApp.ports.errorOut) {
      this.elmApp.ports.errorOut.subscribe((message) => {
        this.logDebug('[PyreClient] port errorOut <-', message);
        const nextState = { ...this.lastSyncState, error: message };
        this.updateSyncState(nextState);
        const error = new Error(message);
        if (config.onError) {
          config.onError(error);
        } else {
          console.error('[PyreClient]', message);
        }
      });
    }

    if (this.elmApp.ports.syncStateOut) {
      this.elmApp.ports.syncStateOut.subscribe((message) => {
        const parsed = parseSyncStateMessage(message);
        if (!parsed) {
          return;
        }

        this.handleRawSyncState(parsed);
      });
    }
  }

  private logDebug = (...args: unknown[]): void => {
    this.emitDevtoolsEvent('debug', { args });
    if (this.debug) {
      console.log(...args);
    }
  };

  static async create(config: SingleDatabasePyreClientCreateConfig): Promise<SingleDatabasePyreClient> {
    const resolvedConfig = await resolveCreateConfig(config);
    const client = new SingleDatabasePyreClient(resolvedConfig);
    await client.init();
    if (resolvedConfig.elm) {
      client.bridgeCleanup = client.attachElmBridge(resolvedConfig.elm);
    }
    return client;
  }

  async init(): Promise<void> {
    await this.storage.init();
    this.lastAppliedServerRevision = await this.storage.getServerRevision();
  }

  startSync(): void {
    this.elmApp.ports.receiveSyncControlMessage?.send({ type: 'startSync' });
  }

  onSyncProgress(callback: (progress: SyncProgress) => void): () => void {
    this.syncProgressCallbacks.add(callback);
    if (this.lastSyncProgress) {
      callback(this.lastSyncProgress);
    }
    return () => {
      this.syncProgressCallbacks.delete(callback);
    };
  }

  onSyncState(callback: (state: SyncState) => void): () => void {
    this.syncStateCallbacks.add(callback);
    callback(this.lastSyncState);
    return () => {
      this.syncStateCallbacks.delete(callback);
    };
  }

  onConnection(callback: (connectionId: string) => void): () => void {
    this.connectionCallbacks.add(callback);
    if (this.connectionId) {
      callback(this.connectionId);
    }
    return () => {
      this.connectionCallbacks.delete(callback);
    };
  }

  async onEntityChanges(subscription: EntitySubscription, callback: (batch: EntityChangeBatch) => void): Promise<() => void> {
    validateEntitySubscription(subscription);
    const initialSequence = this.entityStream.reserveSequence();
    const pendingBatches: EntityChangeBatch[] = [];
    let initialLoaded = false;
    const startedAt = Date.now();
    const subscribedTables = subscription.tables.map((table) => ({
      tableName: table.tableName,
      whereKeys: table.where ? Object.keys(table.where).sort() : [],
    }));

    this.logDebug('[PyreClient] Entity stream initial load started', {
      databaseId: this.databaseId,
      initialSequence,
      tableCount: subscription.tables.length,
      tables: subscribedTables,
    });

    const unsubscribe = this.entityStream.subscribe(subscription, (batch) => {
      if (initialLoaded) {
        this.logDebug('[PyreClient] Entity stream forwarding batch', {
          databaseId: this.databaseId,
          initialSequence,
          source: batch.source,
          sequence: batch.sequence,
          changeCount: batch.changes.length,
          elapsedMs: Date.now() - startedAt,
        });
        callback(batch);
        return;
      }

      this.logDebug('[PyreClient] Entity stream buffering batch during initial load', {
        databaseId: this.databaseId,
        initialSequence,
        source: batch.source,
        sequence: batch.sequence,
        changeCount: batch.changes.length,
        bufferedBatchCount: pendingBatches.length + 1,
        elapsedMs: Date.now() - startedAt,
      });
      pendingBatches.push(batch);
    });

    const initialRows = await this.loadInitialEntityRows(subscription);
    const initialBatch = this.entityStream.createBatchFromRows(
      subscription,
      initialRows,
      'indexeddb-initial',
      this.databaseId,
      initialSequence
    );
    const initialTableCounts = Object.fromEntries(Array.from(initialRows.entries()).map(([tableName, rows]) => [tableName, rows.length]));
    const initialChangeCount = initialBatch?.changes.length ?? 0;

    this.logDebug('[PyreClient] Entity stream initial load finished', {
      databaseId: this.databaseId,
      initialSequence,
      elapsedMs: Date.now() - startedAt,
      tableCounts: initialTableCounts,
      initialChangeCount,
      bufferedBatchCount: pendingBatches.length,
    });

    const batchToEmit = initialBatch ?? {
      type: 'entity-change-batch',
      databaseId: this.databaseId,
      sequence: initialSequence,
      source: 'indexeddb-initial',
      changes: [],
    };

    this.logDebug('[PyreClient] Entity stream emitting initial batch', {
      databaseId: this.databaseId,
      initialSequence,
      source: batchToEmit.source,
      sequence: batchToEmit.sequence,
      changeCount: batchToEmit.changes.length,
      elapsedMs: Date.now() - startedAt,
    });
    callback(batchToEmit);

    initialLoaded = true;
    pendingBatches.forEach((batch, index) => {
      this.logDebug('[PyreClient] Entity stream draining buffered batch', {
        databaseId: this.databaseId,
        initialSequence,
        bufferedBatchIndex: index,
        bufferedBatchCount: pendingBatches.length,
        source: batch.source,
        sequence: batch.sequence,
        changeCount: batch.changes.length,
        elapsedMs: Date.now() - startedAt,
      });
      callback(batch);
    });
    return unsubscribe;
  }

  onDevtoolsEvent(callback: (event: PyreDevtoolsEvent) => void): () => void {
    this.devtoolsEventCallbacks.add(callback);
    return () => {
      this.devtoolsEventCallbacks.delete(callback);
    };
  }

  async getDevtoolsSnapshot(): Promise<PyreDevtoolsSnapshot> {
    const cursor = await this.storage.getSyncCursor();

    const tableNames = new Set([
      ...Object.keys(this.schema.tables),
      ...Object.keys(cursor.tables),
    ]);
    const snapshots: Record<string, PyreDevtoolsTableSnapshot> = {};

    await Promise.all(Array.from(tableNames).sort().map(async (tableName) => {
      snapshots[tableName] = {
        name: tableName,
        count: await this.storage.countRows(tableName),
        sync: this.lastSyncState.tables[tableName],
        cursor: cursor.tables[tableName],
      };
    }));

    return {
      schema: this.schema,
      indexedDbName: this.indexedDbName,
      server: {
        baseUrl: this.server.baseUrl,
        endpoints: this.endpoints,
        liveSyncTransport: this.server.liveSyncTransport ?? 'sse',
        hasHeaders: hasServerHeaders(this.server),
        credentials: getServerCredentials(this.server),
        withCredentials: shouldIncludeCredentials(this.server),
      },
      connectionId: this.connectionId,
      syncState: this.lastSyncState,
      syncProgress: this.lastSyncProgress,
      debugValues: { ...this.devtoolsDebugValues },
      tables: snapshots,
    };
  }

  async inspectDevtoolsTablePage(request: Omit<PyreDevtoolsTablePageRequest, 'instanceId' | 'databaseId'>): Promise<PyreDevtoolsTablePage> {
    const offset = Math.max(0, Math.floor(request.offset ?? 0));
    const limit = Math.max(1, Math.min(500, Math.floor(request.limit ?? 100)));
    const page = await this.storage.getRowsPage(request.tableName, offset, limit);
    return {
      rows: page.rows,
      offset,
      limit,
      hasMore: page.hasMore,
    };
  }

  private async loadInitialEntityRows(subscription: EntitySubscription): Promise<Map<string, Array<Record<string, unknown>>>> {
    const rowsByTable = new Map<string, Array<Record<string, unknown>>>();
    const tableNames = Array.from(new Set(subscription.tables.map((table) => table.tableName)));
    const loadStartedAt = Date.now();

    this.logDebug('[PyreClient] Entity stream IndexedDB snapshot scan started', {
      databaseId: this.databaseId,
      tableNames,
      tableCount: tableNames.length,
    });

    await Promise.all(tableNames.map(async (tableName) => {
      const tableStartedAt = Date.now();
      const rows: Array<Record<string, unknown>> = [];
      let offset = 0;
      const limit = 500;
      let pageCount = 0;

      while (true) {
        const page = await this.storage.getRowsPage(tableName, offset, limit);
        pageCount += 1;
        page.rows.forEach((row) => {
          if (isRecord(row)) {
            rows.push(row);
          }
        });

        if (!page.hasMore) {
          break;
        }

        offset += limit;
      }

      if (rows.length > 0) {
        rowsByTable.set(tableName, rows);
      }

      this.logDebug('[PyreClient] Entity stream IndexedDB snapshot table loaded', {
        databaseId: this.databaseId,
        tableName,
        rowCount: rows.length,
        pageCount,
        elapsedMs: Date.now() - tableStartedAt,
      });
    }));

    this.logDebug('[PyreClient] Entity stream IndexedDB snapshot scan finished', {
      databaseId: this.databaseId,
      tableCount: tableNames.length,
      totalRowCount: Array.from(rowsByTable.values()).reduce((sum, rows) => sum + rows.length, 0),
      elapsedMs: Date.now() - loadStartedAt,
    });

    return rowsByTable;
  }


  setDevtoolsDebugValue(name: string, value: unknown): void {
    if (value === undefined) {
      delete this.devtoolsDebugValues[name];
    } else {
      this.devtoolsDebugValues[name] = value;
    }
    this.emitDevtoolsEvent('debug:value', { name, value });
  }

  getConnectionId(): string | null {
    return this.connectionId;
  }

  setSession(session: Record<string, unknown> | null): void {
    this.session = session ?? {};
    this.emitDevtoolsEvent('session:update', { keys: Object.keys(this.session) });
    this.queryClient.refreshAllQueries();
  }

  run<Input = unknown>(
    databaseId: DatabaseId,
    queryModule: QueryModule<Input>,
    input: Input,
    callback: (result: unknown) => void
  ): QuerySubscription<Input> | void {
    const targetDatabaseId = requireDatabaseId(databaseId);

    if (queryModule.operation === 'query') {
      return this.runQuery(targetDatabaseId, queryModule, input, callback);
    }

    this.runMutation(targetDatabaseId, queryModule, input, callback);
    return;
  }

  disconnect(): void {
    this.bridgeCleanup?.();
    this.bridgeCleanup = null;
    this.sseManager.disconnect();
    this.webSocketManager.disconnect();
    this.connectionId = null;
    this.pendingLiveState = null;
    this.pendingLiveQueries.clear();
    const resetState = {
      ...this.lastSyncState,
      status: 'not_started' as const,
      tables: Object.fromEntries(
        Object.keys(this.lastSyncState.tables).map((table) => [table, 'waiting' as const])
      ),
    };
    this.updateSyncState(resetState);
  }

  private handleLiveSyncMessage = (message: LiveSyncMessage): void => {
    this.emitDevtoolsEvent(`sync:${message.type}`, message);

    if (message.type === 'delta' && this.shouldAcceptLiveDelta(message)) {
      const tableGroups = message.data as ServerTableGroup[];
      this.logDebug('[PyreClient] Live sync delta accepted', {
        databaseId: this.databaseId,
        source: this.lastSyncState.status === 'live' ? 'live' : 'catchup',
        serverRevision: message.serverRevision,
        tableGroupCount: tableGroups.length,
        rowCount: tableGroups.reduce((sum, group) => sum + group.rows.length, 0),
      });
      this.noteAppliedServerRevision(message.serverRevision);
      this.entityStream.handleTableDelta(
        tableGroups,
        this.lastSyncState.status === 'live' ? 'live' : 'catchup',
        this.databaseId
      );
    }

    if (message.type === 'connected') {
      const connectionId = message.connectionId;
      if (connectionId) {
        this.connectionId = connectionId;
        this.connectionCallbacks.forEach((callback) => {
          callback(connectionId);
        });
      }
    }

    if (message.type === 'syncProgress') {
      const nextState = {
        ...this.lastSyncState,
        status: 'catching_up' as const,
      };
      this.handleRawSyncState(nextState);
    }

    if ((message.type === 'syncRequired' || message.type === 'catchupRequired') && this.shouldAcceptLiveControlMessage(message)) {
      const nextState = {
        ...this.lastSyncState,
        status: 'catching_up' as const,
      };
      this.handleRawSyncState(nextState);
    }

    if (message.type === 'syncComplete') {
      const liveTables: Record<string, TableSyncStatus> = {};
      Object.keys(this.lastSyncState.tables).forEach((tableName) => {
        liveTables[tableName] = 'live';
      });
      const nextState = {
        ...this.lastSyncState,
        status: 'live' as const,
        tables: liveTables,
      };
      this.handleRawSyncState(nextState);
    }
  };

  private async notifyEntityStreamFromOptimisticMutation(optimistic: unknown, input: unknown): Promise<void> {
    const metadata = parseOptimisticMutation(optimistic);
    if (!metadata) {
      return;
    }

    try {
      const tableGroups = await this.buildOptimisticTableGroups(metadata, input);
      this.entityStream.handleTableDelta(tableGroups, 'optimistic', this.databaseId);
    } catch (error) {
      console.error('[PyreClient] Failed to notify optimistic entity stream:', error);
    }
  }

  private async buildOptimisticTableGroups(
    optimistic: OptimisticMutationMetadata,
    input: unknown
  ): Promise<ServerTableGroup[]> {
    if (!isRecord(input)) {
      return [];
    }

    const whereValue = input[optimistic.where.input];
    if (whereValue === undefined) {
      return [];
    }

    const tableName = this.schema.queryFieldToTable?.[optimistic.queryField] ?? optimistic.queryField;
    const setValues = Object.fromEntries(
      optimistic.set
        .filter((field) => input[field.input] !== undefined)
        .map((field) => [field.field, input[field.input]])
    );

    if (Object.keys(setValues).length === 0) {
      return [];
    }

    const rows = await this.storage.getAllRows(tableName);
    const matchingRows = rows
      .filter(isRecord)
      .filter((row) => row[optimistic.where.field] === whereValue)
      .map((row) => ({ ...row, ...setValues }));
    const optimisticRows = matchingRows.length > 0
      ? matchingRows
      : [{ [optimistic.where.field]: whereValue, ...setValues }];

    return tableGroupsFromRows(tableName, optimisticRows);
  }

  private notifyEntityStreamFromMutationResult(result: unknown): void {
    const envelope = mutationResultEnvelope(result);
    const serverRevision = extractServerRevision(envelope);
    if (this.isStaleServerRevision(serverRevision)) {
      return;
    }

    const syncDelta = extractMutationSyncDelta(envelope);
    if (!syncDelta) {
      return;
    }

    if (this.databaseId && syncDelta.databaseId !== undefined && syncDelta.databaseId !== this.databaseId) {
      return;
    }

    this.entityStream.handleTableDelta(syncDelta.data, 'mutation-response', this.databaseId);
  }

  private shouldAcceptLiveDelta(message: LiveSyncMessage): boolean {
    if (!Array.isArray(message.data)) {
      return false;
    }

    if (this.isStaleServerRevision(message.serverRevision)) {
      return false;
    }

    if (!this.databaseId) {
      return true;
    }

    return message.databaseId === this.databaseId;
  }

  private shouldAcceptLiveControlMessage(message: LiveSyncMessage): boolean {
    if (this.isStaleServerRevision(message.serverRevision)) {
      return false;
    }

    if (!this.databaseId) {
      return true;
    }

    return message.databaseId === this.databaseId;
  }

  private isStaleServerRevision(serverRevision: number | undefined): boolean {
    return typeof serverRevision === 'number'
      && this.lastAppliedServerRevision !== null
      && serverRevision <= this.lastAppliedServerRevision;
  }

  private noteAppliedServerRevision(serverRevision: number | undefined): void {
    if (typeof serverRevision !== 'number') {
      return;
    }

    this.lastAppliedServerRevision = Math.max(this.lastAppliedServerRevision ?? 0, serverRevision);
    void this.storage.putServerRevision(this.lastAppliedServerRevision).catch((error) => {
      console.error('[PyreClient] Failed to persist server revision:', error);
    });
  }

  private handleRawSyncState(state: SyncState): void {
    if (state.status !== 'live') {
      this.pendingLiveState = null;
      this.pendingLiveQueries.clear();
      this.updateSyncState(state);
      return;
    }

    const registeredQueryIds = this.queryClient.getRegisteredQueryIds();
    if (registeredQueryIds.length === 0) {
      this.pendingLiveState = null;
      this.pendingLiveQueries.clear();
      this.updateSyncState(state);
      return;
    }

    this.pendingLiveState = state;
    this.pendingLiveQueries = new Set(registeredQueryIds);
    registeredQueryIds.forEach((queryId) => {
      this.queryClient.refreshQuery(queryId);
    });
  }

  private handleQueryResult = (queryId: string): void => {
    if (!this.pendingLiveState) {
      return;
    }

    this.pendingLiveQueries.delete(queryId);
    if (this.pendingLiveQueries.size === 0) {
      const nextState = this.pendingLiveState;
      this.pendingLiveState = null;
      this.updateSyncState(nextState);
    }
  };

  private handleQueryUnregister = (queryId: string): void => {
    if (!this.pendingLiveState) {
      return;
    }

    this.pendingLiveQueries.delete(queryId);
    if (this.pendingLiveQueries.size === 0) {
      const nextState = this.pendingLiveState;
      this.pendingLiveState = null;
      this.updateSyncState(nextState);
    }
  };

  private updateSyncState(state: SyncState): void {
    this.lastSyncState = state;
    this.emitDevtoolsEvent('sync:state', state);
    this.syncStateCallbacks.forEach((callback) => {
      callback(state);
    });

    const progress = toSyncProgress(state);
    this.lastSyncProgress = progress;
    this.syncProgressCallbacks.forEach((callback) => {
      callback(progress);
    });
  }

  async deleteDatabase(): Promise<void> {
    await this.storage.deleteDatabase();
    this.emitDevtoolsEvent('indexeddb:delete', { indexedDbName: this.indexedDbName });
  }

  registerQuery<Input = unknown>(
    registration: RegisteredQuery<Input>,
    callback: (result: RegisteredQueryResult) => void
  ): QuerySubscription<Input> {
    const normalizedInput = (registration.input ?? {}) as Input;
    this.emitDevtoolsEvent('query:register', {
      queryId: registration.queryId,
      queryName: registration.queryName,
      input: normalizedInput,
      querySource: registration.querySource,
    });
    this.queryClient.registerQuery(
      {
        queryId: registration.queryId,
        querySource: registration.querySource,
        input: normalizedInput,
      },
      (update) => {
        this.emitDevtoolsEvent('query:result', {
          queryId: update.queryId,
          queryName: registration.queryName,
          revision: update.revision,
          result: update.result,
        });
        callback({
          queryId: update.queryId,
          queryName: registration.queryName,
          revision: update.revision,
          result: update.result,
        });
      }
    );

    return {
      unsubscribe: () => {
        this.emitDevtoolsEvent('query:unregister', { queryId: registration.queryId, queryName: registration.queryName });
        this.queryClient.unregisterQuery(registration.queryId);
      },
      update: (updatedInput: Input) => {
        this.emitDevtoolsEvent('query:update-input', { queryId: registration.queryId, queryName: registration.queryName, input: updatedInput });
        this.queryClient.updateQueryInput(registration.queryId, updatedInput);
      },
    };
  }

  attachElmBridge(config: ElmBridgeConfig): () => void {
    this.bridgeCleanup?.();
    const registrations = new Map<string, QuerySubscription<unknown>>();
    const entityRegistrations = new Map<string, EntityBridgeRegistration>();
    const receivePort = getElmBridgePort(config.app, config.receivePort ?? 'pyreStoreOut');
    const queryResultPort = getElmBridgePort(config.app, config.queryResultPort ?? 'pyre_receiveQueryDelta');
    const entityChangesPort = getElmBridgePort(config.app, config.entityChangesPort ?? 'pyre_receiveEntityChanges');
    const syncStatePort = config.syncStatePort
      ? getElmBridgePort(config.app, config.syncStatePort)
      : getElmBridgePort(config.app, 'pyre_receiveSyncState')
      ;
    const mutationResultPort = config.mutationResultPort
      ? getElmBridgePort(config.app, config.mutationResultPort)
      : getElmBridgePort(config.app, 'pyre_receiveMutationResult')
      ;

    const onSyncStateUnsubscribe = syncStatePort?.send
      ? this.onSyncState((syncState) => {
        syncStatePort.send?.({
          status: syncState.status,
          tables: syncState.tables,
          error: syncState.error ? { message: syncState.error } : null,
        });
      })
      : () => {};

    const unsubscribeAll = () => {
      registrations.forEach((subscription) => {
        subscription.unsubscribe();
      });
      registrations.clear();
      entityRegistrations.forEach((registration) => {
        void registration.unsubscribePromise.then((unsubscribe) => unsubscribe());
      });
      entityRegistrations.clear();
      onSyncStateUnsubscribe();
      receivePort?.unsubscribe?.(handleIncoming);
      if (this.bridgeCleanup === unsubscribeAll) {
        this.bridgeCleanup = null;
      }
    };

    const handleIncoming = (incoming: unknown) => {
      void (async () => {
        try {
          const message = parseElmBridgeIncomingMessage(incoming);

          if (message.type === 'mutate') {
            if (config.onMutation) {
              try {
                await config.onMutation({ client: this, message });
              } catch (error) {
                reportElmBridgeError(config, error, 'mutation-handler');
              }
            } else {
              this.runBridgeMutation(message, mutationResultPort);
            }
            return;
          }

          if (message.type === 'register-entity-stream') {
            this.logDebug('[PyreClient] Elm bridge register entity stream', {
              databaseId: message.databaseId,
              streamId: message.streamId,
              tableCount: message.tables.length,
              tables: message.tables.map((table) => ({
                tableName: table.tableName,
                whereKeys: table.where ? Object.keys(table.where).sort() : [],
              })),
            });
            const existingRegistration = entityRegistrations.get(message.streamId);
            if (existingRegistration && entityBridgeRegistrationMatches(existingRegistration, message)) {
              return;
            }

            if (existingRegistration) {
              void existingRegistration.unsubscribePromise.then((unsubscribe) => unsubscribe());
            }

            const registration: EntityBridgeRegistration = {
              databaseId: message.databaseId,
              tables: message.tables,
              unsubscribePromise: Promise.resolve(() => {}),
            };
            entityRegistrations.set(message.streamId, registration);
            const unsubscribePromise = this.onEntityChanges(
              { tables: message.tables },
              (batch) => {
                if (entityRegistrations.get(message.streamId) !== registration) {
                  return;
                }

                this.logDebug('[PyreClient] Elm bridge forward entity stream batch', {
                  databaseId: message.databaseId,
                  streamId: message.streamId,
                  source: batch.source,
                  sequence: batch.sequence,
                  changeCount: batch.changes.length,
                });
                entityChangesPort?.send?.({
                  ...batch,
                  streamId: message.streamId,
                } satisfies ElmBridgeEntityChangeBatchMessage);
              }
            );
            registration.unsubscribePromise = unsubscribePromise;
            return;
          }

          if (message.type === 'unregister-entity-stream') {
            const registration = entityRegistrations.get(message.streamId);
            if (!registration) {
              return;
            }

            const unsubscribe = await registration.unsubscribePromise;
            unsubscribe();
            if (entityRegistrations.get(message.streamId) === registration) {
              entityRegistrations.delete(message.streamId);
            }
            return;
          }

          if (message.type === 'register') {
            const queryName = asNonEmptyString(message.queryName, 'register message queryName');
            const querySource = asQueryShape(message.querySource);

            registrations.get(message.queryId)?.unsubscribe();

            const subscription = this.registerQuery(
              {
                databaseId: message.databaseId,
                queryId: message.queryId,
                queryName,
                querySource,
                input: message.queryInput,
              },
              ({ queryId, queryName: resultQueryName, revision, result }) => {
                queryResultPort?.send?.({
                  type: 'full',
                  queryId,
                  queryName: resultQueryName ?? queryName,
                  revision,
                  result,
                });
              }
            );

            registrations.set(message.queryId, subscription as QuerySubscription<unknown>);
            return;
          }

          if (message.type === 'update-input') {
            const registration = registrations.get(message.queryId);
            if (!registration) {
              throw new Error(`update-input for unknown query id: ${message.queryId}`);
            }

            registration.update(message.queryInput);
            return;
          }

          const registration = registrations.get(message.queryId);
          if (!registration) {
            return;
          }

          registration.unsubscribe();
          registrations.delete(message.queryId);
        } catch (error) {
          reportElmBridgeError(config, error, 'incoming-message');
        }
      })();
    };

    receivePort?.subscribe?.(handleIncoming);
    this.bridgeCleanup = unsubscribeAll;
    return unsubscribeAll;
  }

  private runQuery<Input>(
    databaseId: DatabaseId,
    queryModule: QueryModule<Input>,
    input: Input,
    callback: (result: unknown) => void
  ): QuerySubscription<Input> {
    const normalizedInput = (input ?? {}) as Input;
    const queryShape = queryModule.toQueryShape
      ? queryModule.toQueryShape(normalizedInput)
      : queryModule.queryShape;

    if (!queryShape) {
      throw new Error('Query module is missing queryShape');
    }

    const queryId = `query_${this.queryCounter}_${Date.now()}`;
    this.queryCounter += 1;

    return this.registerQuery(
      {
        databaseId,
        queryId,
        querySource: queryShape,
        input: normalizedInput,
      },
      (update) => {
        callback(update.result);
      }
    );
  }

  private runMutation<Input>(
    databaseId: DatabaseId,
    queryModule: QueryModule<Input>,
    input: Input,
    callback: (result: unknown) => void
  ): void {
    const mutationId = queryModule.id;
    if (!mutationId) {
      throw new Error('Mutation module is missing id');
    }

    const requestId = `mutation_${this.mutationCounter}_${Date.now()}`;
    this.mutationCounter += 1;
    const payload = input ?? {};
    const optimistic = typeof queryModule.optimistic === 'function'
      ? queryModule.optimistic(payload as Input)
      : queryModule.optimistic;
    const baseUrl = resolveEndpointUrl(this.server.baseUrl, this.endpoints.query, {
      databaseId,
      sync: 'true',
      ...(this.connectionId ? { connectionId: this.connectionId } : {}),
    });
    this.emitDevtoolsEvent('mutation:request', { requestId, mutationId, databaseId, input: payload, optimistic });
    void (async () => {
      await this.notifyEntityStreamFromOptimisticMutation(optimistic, payload);
      this.queryManager.sendMutation(
        requestId,
        mutationId,
        baseUrl,
        payload,
        optimistic,
        (result) => {
          this.notifyEntityStreamFromMutationResult(result);
          this.noteAppliedServerRevision(extractServerRevision(mutationResultEnvelope(result)));
          const appResult = unwrapMutationResultEnvelope(result);
          this.emitDevtoolsEvent('mutation:result', { requestId, mutationId, result });
          callback(appResult);
        },
        await resolveServerHeaders(this.server),
        getServerCredentials(this.server),
        this.server.withCredentials === true
      );
    })();
  }

  private runBridgeMutation(
    message: ElmBridgeMutationMessage,
    mutationResultPort?: ElmBridgePort
  ): void {
    const baseUrl = resolveEndpointUrl(this.server.baseUrl, this.endpoints.query, {
      databaseId: message.databaseId,
      sync: 'true',
      ...(this.connectionId ? { connectionId: this.connectionId } : {}),
    });
    this.emitDevtoolsEvent('mutation:request', {
      requestId: message.requestId,
      mutationId: message.mutationId,
      mutationName: message.mutationName,
      databaseId: message.databaseId,
      input: message.mutationInput ?? {},
      optimistic: message.optimistic,
    });
    void (async () => {
      await this.notifyEntityStreamFromOptimisticMutation(message.optimistic, message.mutationInput ?? {});
      this.queryManager.sendMutation(
        message.requestId,
        message.mutationId,
        baseUrl,
        message.mutationInput ?? {},
        message.optimistic,
        (result) => {
          this.notifyEntityStreamFromMutationResult(result);
          this.noteAppliedServerRevision(extractServerRevision(mutationResultEnvelope(result)));
          const appResult = unwrapMutationResultEnvelope(result);
          this.emitDevtoolsEvent('mutation:result', {
            requestId: message.requestId,
            mutationId: message.mutationId,
            mutationName: message.mutationName,
            result: appResult,
          });
          mutationResultPort?.send?.({
            type: 'mutation-result',
            requestId: message.requestId,
            mutationId: message.mutationId,
            mutationName: message.mutationName ?? null,
            result: appResult as MutationResult,
          } satisfies ElmBridgeMutationResultMessage);
        },
        await resolveServerHeaders(this.server),
        getServerCredentials(this.server),
        this.server.withCredentials === true
      );
    })();
  }

  private emitDevtoolsEvent(type: string, payload?: unknown): void {
    if (this.devtoolsEventCallbacks.size === 0) {
      return;
    }

    const event = {
      id: this.devtoolsEventCounter,
      timestamp: Date.now(),
      type,
      payload,
    } satisfies PyreDevtoolsEvent;
    this.devtoolsEventCounter += 1;

    this.devtoolsEventCallbacks.forEach((callback) => {
      callback(event);
    });
  }
}

export class PyreClient {
  private static readonly devtoolsEventLimit = 200;
  private config: ResolvedPyreClientConfig;
  private clients: Map<DatabaseId, Promise<PyreInternalClient>> = new Map();
  private clientGenerations: Map<DatabaseId, number> = new Map();
  private knownDatabaseIds: DatabaseId[] = [];
  private syncedDatabaseIds: DatabaseId[] = [];
  private syncingDatabaseId: DatabaseId | null = null;
  private completedSyncDatabaseIds: Set<DatabaseId> = new Set();
  private syncStateCallbacks: Set<(state: SyncState) => void> = new Set();
  private latestSyncStates: Map<DatabaseId, SyncState> = new Map();
  private internalDevtoolsUnsubscribers: Map<DatabaseId, () => void> = new Map();
  private bridgeCleanup: (() => void) | null = null;
  private devtoolsDebugValues: Record<string, unknown> = {};
  private devtoolsEventCounter = 0;
  private devtoolsEvents: PyreDevtoolsEvent[] = [];
  private devtoolsEventCallbacks: Set<(event: PyreDevtoolsEvent) => void> = new Set();
  private instanceId: string;

  private constructor(config: ResolvedPyreClientConfig) {
    this.config = config;
    this.instanceId = registerPyreDevtoolsClient(this);
  }

  static async create(config: PyreClientConfig): Promise<PyreClient> {
    const resolved = await resolveMultiCreateConfig(config);
    const client = new PyreClient(resolved);
    try {
      if (resolved.elm) {
        client.attachElmBridge(resolved.elm);
      }
    } catch (error) {
      client.disconnect();
      throw error;
    }
    return client;
  }

  run<Input = unknown>(
    databaseId: DatabaseId,
    queryModule: QueryModule<Input>,
    input: Input,
    callback: (result: unknown) => void
  ): Promise<QuerySubscription<Input> | void> {
    this.markKnownDatabase(databaseId);
    if (queryModule.operation !== 'query') {
      return this.runPublicMutation(databaseId, queryModule, input, callback);
    }
    return this.getOrCreateClient(databaseId).then((client) => (
      client.run(databaseId, queryModule, input, callback)
    ));
  }

  async getOrCreateClient(databaseId: DatabaseId): Promise<PyreInternalClient> {
    const targetDatabaseId = requireDatabaseId(databaseId);
    this.markKnownDatabase(targetDatabaseId);
    const existing = this.clients.get(targetDatabaseId);
    if (existing) {
      return existing;
    }

    const generation = (this.clientGenerations.get(targetDatabaseId) ?? 0) + 1;
    this.clientGenerations.set(targetDatabaseId, generation);

    const created = this.config.createInternalClient(this.internalClientConfig(targetDatabaseId));
    this.clients.set(targetDatabaseId, created);
    void created.then((client) => {
      this.watchInternalClient(targetDatabaseId, generation, client);
    });
    return created;
  }

  async setSyncedDatabases(databaseIds: DatabaseId[]): Promise<void> {
    const nextDatabaseIds = dedupeDatabaseIds(databaseIds);
    nextDatabaseIds.forEach((databaseId) => this.markKnownDatabase(databaseId));
    const nextSet = new Set(nextDatabaseIds);

    const removedDatabaseIds = this.syncedDatabaseIds.filter((databaseId) => !nextSet.has(databaseId));
    removedDatabaseIds.forEach((databaseId) => {
      this.completedSyncDatabaseIds.delete(databaseId);
      if (this.syncingDatabaseId === databaseId) {
        this.syncingDatabaseId = null;
      }
      this.disconnectInternalClient(databaseId);
    });

    this.syncedDatabaseIds = nextDatabaseIds;
    this.emitDevtoolsEvent('sync.databases', {
      instanceId: this.instanceId,
      databaseIds: [...this.syncedDatabaseIds],
    });

    for (const databaseId of nextDatabaseIds) {
      await this.getOrCreateClient(databaseId);
    }

    this.startNextSync();
  }

  async syncDatabase(databaseId: DatabaseId): Promise<void> {
    const targetDatabaseId = requireDatabaseId(databaseId);
    this.markKnownDatabase(targetDatabaseId);
    if (this.syncedDatabaseIds.includes(targetDatabaseId)) {
      return;
    }

    await this.setSyncedDatabases([...this.syncedDatabaseIds, targetDatabaseId]);
  }

  async unsyncDatabase(databaseId: DatabaseId): Promise<void> {
    const targetDatabaseId = requireDatabaseId(databaseId);
    this.markKnownDatabase(targetDatabaseId);
    await this.setSyncedDatabases(this.syncedDatabaseIds.filter((id) => id !== targetDatabaseId));
  }

  getInternalIndexedDbName(databaseId: DatabaseId): string {
    return deriveIndexedDbName(
      this.config.indexedDbName ?? 'pyre-client',
      this.config.cacheNamespace,
      databaseId
    );
  }

  getInternalDatabaseIds(): DatabaseId[] {
    return Array.from(this.clients.keys());
  }

  getKnownDatabaseIds(): DatabaseId[] {
    return [...this.knownDatabaseIds];
  }

  getSyncedDatabaseIds(): DatabaseId[] {
    return [...this.syncedDatabaseIds];
  }

  onSyncState(callback: (state: SyncState) => void): () => void {
    this.syncStateCallbacks.add(callback);
    callback(this.aggregateSyncState());
    return () => {
      this.syncStateCallbacks.delete(callback);
    };
  }

  onSyncProgress(callback: (progress: SyncProgress) => void): () => void {
    return this.onSyncState((state) => {
      callback(toSyncProgress(state));
    });
  }

  onDevtoolsEvent(callback: (event: PyreDevtoolsEvent) => void): () => void {
    this.devtoolsEventCallbacks.add(callback);
    return () => {
      this.devtoolsEventCallbacks.delete(callback);
    };
  }

  async onEntityChanges(
    databaseId: DatabaseId,
    subscription: EntitySubscription,
    callback: (batch: EntityChangeBatch) => void
  ): Promise<() => void> {
    const targetDatabaseId = requireDatabaseId(databaseId);
    validateEntitySubscription(subscription);
    this.markKnownDatabase(targetDatabaseId);
    const client = await this.getOrCreateClient(targetDatabaseId);
    return client.onEntityChanges(subscription, callback);
  }

  async getDevtoolsSnapshot(): Promise<PyreDevtoolsSnapshot> {
    const selectedDatabaseId = this.knownDatabaseIds[0];
    const firstClient = selectedDatabaseId ? await this.clients.get(selectedDatabaseId) : undefined;
    if (firstClient) {
      const snapshot = await firstClient.getDevtoolsSnapshot();
      return {
        ...snapshot,
        indexedDbName: this.syncedDatabaseIds.length === 1
          ? snapshot.indexedDbName
          : this.config.indexedDbName ?? 'pyre-client',
        syncState: this.aggregateSyncState(),
        syncProgress: toSyncProgress(this.aggregateSyncState()),
        debugValues: { ...snapshot.debugValues, ...this.devtoolsDebugValues },
      };
    }

    const endpoints = {
      catchup: '/sync',
      events: '/sync/events',
      query: '/db',
      ...this.config.server.endpoints,
    };

    return {
      schema: this.config.schema,
      indexedDbName: this.config.indexedDbName ?? 'pyre-client',
      server: {
        baseUrl: this.config.server.baseUrl,
        endpoints,
        liveSyncTransport: this.config.server.liveSyncTransport ?? 'sse',
        hasHeaders: hasServerHeaders(this.config.server),
        credentials: getServerCredentials(this.config.server),
        withCredentials: shouldIncludeCredentials(this.config.server),
      },
      connectionId: null,
      syncState: this.aggregateSyncState(),
      syncProgress: toSyncProgress(this.aggregateSyncState()),
      debugValues: { ...this.devtoolsDebugValues },
      tables: {},
    };
  }

  async getDevtoolsInstanceSnapshot(): Promise<PyreDevtoolsInstanceSnapshot> {
    const aggregateSyncState = this.aggregateSyncState();
    const databases = await Promise.all(this.knownDatabaseIds.map(async (databaseId) => {
      const client = await this.clients.get(databaseId);
      const syncState = this.latestSyncStates.get(databaseId);
      let tableSummaries: DevtoolsTableSummary[] | undefined;
      if (client) {
        const snapshot = await client.getDevtoolsSnapshot();
        tableSummaries = Object.values(snapshot.tables).map((table) => ({
          name: table.name,
          count: table.count,
          sync: table.sync,
          cursor: table.cursor,
        }));
      }

      return {
        databaseId,
        indexedDbName: this.getInternalIndexedDbName(databaseId),
        flaggedForSync: this.syncedDatabaseIds.includes(databaseId),
        lifecycle: this.classifyDatabaseLifecycle(databaseId),
        syncState,
        tableSummaries,
        error: syncState?.error,
      } satisfies DevtoolsDatabaseSummary;
    }));

    return {
      instanceId: this.instanceId,
      label: this.config.cacheNamespace,
      cacheNamespace: this.config.cacheNamespace,
      schema: this.config.schema,
      server: this.devtoolsServerSnapshot(),
      aggregateSyncState,
      syncProgress: toSyncProgress(aggregateSyncState),
      debugValues: { ...this.devtoolsDebugValues },
      databases,
      events: [...this.devtoolsEvents],
    };
  }

  async inspectDevtoolsTablePage(request: Omit<PyreDevtoolsTablePageRequest, 'instanceId'>): Promise<PyreDevtoolsTablePage> {
    this.markKnownDatabase(request.databaseId);
    const client = await this.getOrCreateClient(request.databaseId);
    return client.inspectDevtoolsTablePage({
      tableName: request.tableName,
      offset: request.offset,
      limit: request.limit,
      filter: request.filter,
      sort: request.sort,
    });
  }

  setDevtoolsDebugValue(name: string, value: unknown): void {
    if (value === undefined) {
      delete this.devtoolsDebugValues[name];
    } else {
      this.devtoolsDebugValues[name] = value;
    }
    this.emitDevtoolsEvent('debug:value', { name, value });
  }

  getConnectionId(): string | null {
    return null;
  }

  setSession(session: Record<string, unknown> | null): void {
    this.config.session = session ?? {};
    this.clients.forEach((clientPromise) => {
      void clientPromise.then((client) => {
        client.setSession(session);
      });
    });
  }

  attachElmBridge(config: ElmBridgeConfig): () => void {
    this.bridgeCleanup?.();
    const registrations = new Map<string, Promise<QuerySubscription<unknown> | void>>();
    const entityRegistrations = new Map<string, EntityBridgeRegistration>();
    const receivePort = getElmBridgePort(config.app, config.receivePort ?? 'pyreStoreOut');
    const queryResultPort = getElmBridgePort(config.app, config.queryResultPort ?? 'pyre_receiveQueryDelta');
    const entityChangesPort = getElmBridgePort(config.app, config.entityChangesPort ?? 'pyre_receiveEntityChanges');
    const syncStatePort = config.syncStatePort
      ? getElmBridgePort(config.app, config.syncStatePort)
      : getElmBridgePort(config.app, 'pyre_receiveSyncState')
      ;
    const mutationResultPort = config.mutationResultPort
      ? getElmBridgePort(config.app, config.mutationResultPort)
      : getElmBridgePort(config.app, 'pyre_receiveMutationResult')
      ;

    const onSyncStateUnsubscribe = syncStatePort?.send
      ? this.onSyncState((syncState) => {
        syncStatePort.send?.({
          status: syncState.status,
          tables: syncState.tables,
          error: syncState.error ? { message: syncState.error } : null,
        });
      })
      : () => {};

    const unsubscribeAll = () => {
      registrations.forEach((subscriptionPromise) => {
        void subscriptionPromise.then((subscription) => {
          subscription?.unsubscribe();
        });
      });
      registrations.clear();
      entityRegistrations.forEach((registration) => {
        void registration.unsubscribePromise.then((unsubscribe) => unsubscribe());
      });
      entityRegistrations.clear();
      onSyncStateUnsubscribe();
      receivePort?.unsubscribe?.(handleIncoming);
      if (this.bridgeCleanup === unsubscribeAll) {
        this.bridgeCleanup = null;
      }
    };

    const handleIncoming = (incoming: unknown) => {
      void (async () => {
        try {
          const message = parseElmBridgeIncomingMessage(incoming);

          if (message.type === 'mutate') {
            if (config.onMutation) {
              this.markKnownDatabase(message.databaseId);
              this.emitDevtoolsEvent('mutation.custom_dispatched', {
                instanceId: this.instanceId,
                databaseId: message.databaseId,
                mutationId: message.mutationId,
                mutationName: message.mutationName,
                input: message.mutationInput ?? {},
              });
              try {
                await config.onMutation({ client: this, message });
              } catch (error) {
                reportElmBridgeError(config, error, 'mutation-handler');
              }
            } else {
              await this.run(
                message.databaseId,
                { operation: 'mutation', id: message.mutationId, optimistic: message.optimistic },
                message.mutationInput ?? {},
                (result) => {
                  mutationResultPort?.send?.({
                    type: 'mutation-result',
                    requestId: message.requestId,
                    mutationId: message.mutationId,
                    mutationName: message.mutationName ?? null,
                    result: result as MutationResult,
                  } satisfies ElmBridgeMutationResultMessage);
                }
              );
            }
            return;
          }

          if (message.type === 'register-entity-stream') {
            this.logDebug('[PyreClient] Elm bridge register entity stream', {
              databaseId: message.databaseId,
              streamId: message.streamId,
              tableCount: message.tables.length,
              tables: message.tables.map((table) => ({
                tableName: table.tableName,
                whereKeys: table.where ? Object.keys(table.where).sort() : [],
              })),
            });
            const existingRegistration = entityRegistrations.get(message.streamId);
            if (existingRegistration && entityBridgeRegistrationMatches(existingRegistration, message)) {
              return;
            }

            if (existingRegistration) {
              void existingRegistration.unsubscribePromise.then((unsubscribe) => unsubscribe());
            }

            const registration: EntityBridgeRegistration = {
              databaseId: message.databaseId,
              tables: message.tables,
              unsubscribePromise: Promise.resolve(() => {}),
            };
            entityRegistrations.set(message.streamId, registration);
            const unsubscribePromise = this.onEntityChanges(
              message.databaseId,
              { tables: message.tables },
              (batch) => {
                if (entityRegistrations.get(message.streamId) !== registration) {
                  return;
                }

                this.logDebug('[PyreClient] Elm bridge forward entity stream batch', {
                  databaseId: message.databaseId,
                  streamId: message.streamId,
                  source: batch.source,
                  sequence: batch.sequence,
                  changeCount: batch.changes.length,
                });
                entityChangesPort?.send?.({
                  ...batch,
                  streamId: message.streamId,
                } satisfies ElmBridgeEntityChangeBatchMessage);
              }
            );
            registration.unsubscribePromise = unsubscribePromise;
            return;
          }

          if (message.type === 'unregister-entity-stream') {
            const registration = entityRegistrations.get(message.streamId);
            if (!registration) {
              return;
            }

            const unsubscribe = await registration.unsubscribePromise;
            unsubscribe();
            if (entityRegistrations.get(message.streamId) === registration) {
              entityRegistrations.delete(message.streamId);
            }
            return;
          }

          if (message.type === 'register') {
            const queryName = asNonEmptyString(message.queryName, 'register message queryName');
            const querySource = asQueryShape(message.querySource);

            const existingRegistration = registrations.get(message.queryId);
            if (existingRegistration) {
              void existingRegistration.then((subscription) => subscription?.unsubscribe());
            }

            const subscriptionPromise = Promise.resolve(this.run(
              message.databaseId,
              {
                operation: 'query',
                queryShape: querySource,
              },
              message.queryInput ?? {},
              (result) => {
                queryResultPort?.send?.({
                  type: 'full',
                  queryId: message.queryId,
                  queryName,
                  revision: Date.now(),
                  result,
                });
              }
            ));

            registrations.set(message.queryId, subscriptionPromise);
            return;
          }

          if (message.type === 'update-input') {
            const registration = registrations.get(message.queryId);
            if (!registration) {
              throw new Error(`update-input for unknown query id: ${message.queryId}`);
            }

            const subscription = await registration;
            subscription?.update(message.queryInput);
            return;
          }

          const registration = registrations.get(message.queryId);
          if (!registration) {
            return;
          }

          const subscription = await registration;
          subscription?.unsubscribe();
          registrations.delete(message.queryId);
        } catch (error) {
          reportElmBridgeError(config, error, 'incoming-message');
        }
      })();
    };

    receivePort?.subscribe?.(handleIncoming);
    this.bridgeCleanup = unsubscribeAll;
    return unsubscribeAll;
  }

  disconnect(): void {
    unregisterPyreDevtoolsClient(this.instanceId);
    this.bridgeCleanup?.();
    this.bridgeCleanup = null;
    this.clients.forEach((clientPromise) => {
      void clientPromise.then((client) => {
        client.disconnect();
      });
    });
    this.clients.clear();
    this.clientGenerations.clear();
    this.knownDatabaseIds = [];
    this.syncedDatabaseIds = [];
    this.syncingDatabaseId = null;
    this.completedSyncDatabaseIds.clear();
    this.latestSyncStates.clear();
    this.internalDevtoolsUnsubscribers.forEach((unsubscribe) => unsubscribe());
    this.internalDevtoolsUnsubscribers.clear();
    this.emitSyncState();
  }

  private startNextSync(): void {
    if (this.syncingDatabaseId) {
      return;
    }

    const nextDatabaseId = this.syncedDatabaseIds.find((databaseId) => !this.completedSyncDatabaseIds.has(databaseId));
    if (!nextDatabaseId) {
      return;
    }

    const clientPromise = this.clients.get(nextDatabaseId);
    if (!clientPromise) {
      return;
    }

    this.syncingDatabaseId = nextDatabaseId;
    this.emitDevtoolsEvent('sync.scheduler', {
      instanceId: this.instanceId,
      syncingDatabaseId: this.syncingDatabaseId,
      queuedDatabaseIds: this.syncedDatabaseIds.filter((databaseId) => databaseId !== this.syncingDatabaseId && !this.completedSyncDatabaseIds.has(databaseId)),
    });
    void clientPromise.then((client) => {
      if (this.syncingDatabaseId !== nextDatabaseId || !this.syncedDatabaseIds.includes(nextDatabaseId)) {
        return;
      }

      client.startSync();
    });
  }

  private async runPublicMutation<Input>(
    databaseId: DatabaseId,
    queryModule: QueryModule<Input>,
    input: Input,
    callback: (result: unknown) => void
  ): Promise<void> {
    const targetDatabaseId = requireDatabaseId(databaseId);
    const mutationId = queryModule.id;
    if (!mutationId) {
      throw new Error('Mutation module is missing id');
    }

    const startedAt = Date.now();
    const eventPayload = {
      instanceId: this.instanceId,
      databaseId: targetDatabaseId,
      mutationId,
      mutationName: mutationId,
      input: input ?? {},
    };
    this.emitDevtoolsEvent('mutation.started', eventPayload);
    const client = await this.getOrCreateClient(targetDatabaseId);
    client.run(targetDatabaseId, queryModule, input, (result) => {
      const elapsedMs = Date.now() - startedAt;
      const mutationResult = result as MutationResult | undefined;
      if (mutationResult && mutationResult.ok === false) {
        this.emitDevtoolsEvent('mutation.failed', {
          ...eventPayload,
          error: mutationResult.error ?? 'Mutation failed',
          result,
          elapsedMs,
        });
      } else {
        this.emitDevtoolsEvent('mutation.completed', {
          ...eventPayload,
          result,
          elapsedMs,
        });
      }
      callback(result);
    });
  }

  private watchInternalClient(databaseId: DatabaseId, generation: number, client: PyreInternalClient): void {
    this.markKnownDatabase(databaseId);
    this.internalDevtoolsUnsubscribers.get(databaseId)?.();
    const unsubscribeDevtools = client.onDevtoolsEvent((event) => {
      if (this.clientGenerations.get(databaseId) !== generation) {
        return;
      }

      this.emitDevtoolsEvent(internalDevtoolsEventType(event), {
        databaseId,
        event,
      });
    });
    this.internalDevtoolsUnsubscribers.set(databaseId, unsubscribeDevtools);

    client.onSyncState((state) => {
      if (this.clientGenerations.get(databaseId) !== generation) {
        return;
      }

      this.latestSyncStates.set(databaseId, state);
      this.emitDevtoolsEvent('sync.database_state', {
        instanceId: this.instanceId,
        databaseId,
        syncState: state,
      });
      this.emitSyncState();

      if (!this.syncedDatabaseIds.includes(databaseId)) {
        return;
      }

      if (state.status !== 'live') {
        return;
      }

      this.completedSyncDatabaseIds.add(databaseId);
      if (this.syncingDatabaseId === databaseId) {
        this.syncingDatabaseId = null;
      }

      this.startNextSync();
    });
  }

  private disconnectInternalClient(databaseId: DatabaseId): void {
    const clientPromise = this.clients.get(databaseId);
    this.clients.delete(databaseId);
    this.latestSyncStates.delete(databaseId);
    this.internalDevtoolsUnsubscribers.get(databaseId)?.();
    this.internalDevtoolsUnsubscribers.delete(databaseId);
    this.emitSyncState();
    this.clientGenerations.set(databaseId, (this.clientGenerations.get(databaseId) ?? 0) + 1);
    if (!clientPromise) {
      return;
    }

    void clientPromise.then((client) => {
      client.disconnect();
    });
  }

  private internalClientConfig(databaseId: DatabaseId): SingleDatabasePyreClientCreateConfig & {
    databaseId: DatabaseId;
    cacheNamespace: CacheNamespace;
  } {
    return {
      schema: this.config.schema,
      server: this.config.server,
      databaseId,
      cacheNamespace: this.config.cacheNamespace,
      autoStartSync: false,
      indexedDbName: this.config.indexedDbName,
      debug: this.config.debug,
      onError: this.config.onError,
      session: this.config.session,
    };
  }

  private emitSyncState(): void {
    if (this.syncStateCallbacks.size === 0) {
      return;
    }

    const state = this.aggregateSyncState();
    this.syncStateCallbacks.forEach((callback) => {
      callback(state);
    });
  }

  private logDebug(...args: unknown[]): void {
    this.emitDevtoolsEvent('debug', { args });
    if (this.config.debug) {
      console.log(...args);
    }
  }

  private emitDevtoolsEvent(type: string, payload?: unknown): void {
    const event = {
      id: this.devtoolsEventCounter,
      timestamp: Date.now(),
      type,
      payload,
    } satisfies PyreDevtoolsEvent;
    this.devtoolsEventCounter += 1;
    this.devtoolsEvents = [event, ...this.devtoolsEvents].slice(0, PyreClient.devtoolsEventLimit);

    this.devtoolsEventCallbacks.forEach((callback) => {
      callback(event);
    });
  }

  private markKnownDatabase(databaseId: DatabaseId): void {
    const targetDatabaseId = requireDatabaseId(databaseId);
    if (!this.knownDatabaseIds.includes(targetDatabaseId)) {
      this.knownDatabaseIds.push(targetDatabaseId);
      this.emitDevtoolsEvent('database.known', {
        instanceId: this.instanceId,
        databaseId: targetDatabaseId,
      });
    }
  }

  private classifyDatabaseLifecycle(databaseId: DatabaseId): DevtoolsDatabaseLifecycle {
    const syncState = this.latestSyncStates.get(databaseId);
    if (syncState?.error) {
      return 'error';
    }
    if (!this.syncedDatabaseIds.includes(databaseId)) {
      return 'unsynced';
    }
    if (this.syncingDatabaseId === databaseId) {
      return 'syncing';
    }
    if (this.completedSyncDatabaseIds.has(databaseId)) {
      return 'live';
    }
    if (this.syncedDatabaseIds.includes(databaseId)) {
      return 'queued';
    }
    return 'not_started';
  }

  private devtoolsServerSnapshot(): PyreDevtoolsSnapshot['server'] {
    const endpoints = {
      catchup: '/sync',
      events: '/sync/events',
      query: '/db',
      ...this.config.server.endpoints,
    };
    return {
      baseUrl: this.config.server.baseUrl,
      endpoints,
      liveSyncTransport: this.config.server.liveSyncTransport ?? 'sse',
      hasHeaders: hasServerHeaders(this.config.server),
      credentials: getServerCredentials(this.config.server),
      withCredentials: shouldIncludeCredentials(this.config.server),
    };
  }

  private aggregateSyncState(): SyncState {
    const activeStates = this.syncedDatabaseIds
      .map((databaseId) => this.latestSyncStates.get(databaseId))
      .filter((state): state is SyncState => Boolean(state));

    if (activeStates.length === 0) {
      return createInitialSyncState(this.config.schema);
    }

    const tables: Record<string, TableSyncStatus> = {};
    Object.keys(this.config.schema.tables).forEach((tableName) => {
      const tableStates = activeStates.map((state) => state.tables[tableName] ?? 'waiting');
      if (tableStates.every((status) => status === 'live')) {
        tables[tableName] = 'live';
      } else if (tableStates.some((status) => status === 'catching_up')) {
        tables[tableName] = 'catching_up';
      } else {
        tables[tableName] = 'waiting';
      }
    });

    const errorState = activeStates.find((state) => state.error);
    return {
      status: activeStates.every((state) => state.status === 'live') ? 'live' : 'catching_up',
      tables,
      error: errorState?.error,
    };
  }
}

function dedupeDatabaseIds(databaseIds: DatabaseId[]): DatabaseId[] {
  const seen = new Set<DatabaseId>();
  const result: DatabaseId[] = [];

  databaseIds.forEach((databaseId) => {
    const targetDatabaseId = requireDatabaseId(databaseId);
    if (seen.has(targetDatabaseId)) {
      return;
    }

    seen.add(targetDatabaseId);
    result.push(targetDatabaseId);
  });

  return result;
}

function createInitialSyncState(schema: SchemaMetadata): SyncState {
  const tables: Record<string, TableSyncStatus> = {};
  Object.entries(schema.tables)
    .filter(([, table]) => table.sync !== 'query-only')
    .forEach(([tableName]) => {
      tables[tableName] = 'waiting';
    });

  return {
    status: 'not_started',
    tables,
  };
}

function getServerCredentials(server: ServerConfig): RequestCredentials {
  if (server.credentials) {
    return server.credentials;
  }

  return server.withCredentials === true ? 'include' : 'same-origin';
}

function shouldIncludeCredentials(server: ServerConfig): boolean {
  return getServerCredentials(server) === 'include';
}

function getStaticServerHeaders(server: ServerConfig): Record<string, string> {
  if (!server.headers || typeof server.headers === 'function') {
    return {};
  }

  return server.headers;
}

async function resolveServerHeaders(server: ServerConfig): Promise<Record<string, string>> {
  if (!server.headers) {
    return {};
  }

  if (typeof server.headers === 'function') {
    return server.headers();
  }

  return server.headers;
}

function serverHeadersToPairs(headers: Record<string, string>): Array<[string, string]> {
  return Object.entries(headers);
}

function hasServerHeaders(server: ServerConfig): boolean {
  return Boolean(server.headers);
}

interface OptimisticMutationMetadata {
  queryField: string;
  where: {
    field: string;
    input: string;
  };
  set: Array<{
    field: string;
    input: string;
  }>;
}

function parseOptimisticMutation(value: unknown): OptimisticMutationMetadata | null {
  if (!isRecord(value) || typeof value.queryField !== 'string' || !isRecord(value.where) || !Array.isArray(value.set)) {
    return null;
  }

  if (typeof value.where.field !== 'string' || typeof value.where.input !== 'string') {
    return null;
  }

  const set = value.set.filter((field): field is { field: string; input: string } => (
    isRecord(field) && typeof field.field === 'string' && typeof field.input === 'string'
  ));
  if (set.length === 0) {
    return null;
  }

  return {
    queryField: value.queryField,
    where: {
      field: value.where.field,
      input: value.where.input,
    },
    set,
  };
}

function tableGroupsFromRows(tableName: string, rows: Array<Record<string, unknown>>): ServerTableGroup[] {
  if (rows.length === 0) {
    return [];
  }

  const headers = rows.reduce<string[]>((acc, row) => {
    Object.keys(row).forEach((field) => {
      if (!acc.includes(field)) {
        acc.push(field);
      }
    });
    return acc;
  }, []);

  return [{
    table_name: tableName,
    headers,
    rows: rows.map((row) => headers.map((header) => row[header] ?? null)),
  }];
}

function mutationResultEnvelope(result: unknown): unknown {
  if (!isRecord(result) || result.ok !== true || !('value' in result)) {
    return result;
  }

  return result.value;
}

function extractServerRevision(result: unknown): number | undefined {
  if (!result || typeof result !== 'object') {
    return undefined;
  }

  const serverRevision = (result as { serverRevision?: unknown }).serverRevision;
  return typeof serverRevision === 'number' ? serverRevision : undefined;
}

function extractMutationSyncDelta(result: unknown): { databaseId?: string; data: ServerTableGroup[] } | null {
  if (!isRecord(result) || !isRecord(result.sync) || result.sync.type !== 'delta' || !Array.isArray(result.sync.data)) {
    return null;
  }

  const databaseId = typeof result.sync.databaseId === 'string'
    ? result.sync.databaseId
    : undefined;

  return {
    databaseId,
    data: result.sync.data.filter(isServerTableGroup),
  };
}

function isServerTableGroup(value: unknown): value is ServerTableGroup {
  return isRecord(value)
    && typeof value.table_name === 'string'
    && Array.isArray(value.headers)
    && value.headers.every((header) => typeof header === 'string')
    && Array.isArray(value.rows);
}

function unwrapMutationResultEnvelope(result: unknown): unknown {
  if (!result || typeof result !== 'object') {
    return result;
  }

  if (!('serverRevision' in result) || !('result' in result)) {
    return result;
  }

  return (result as { result?: unknown }).result;
}

function parseSyncStateMessage(message: unknown): SyncState | null {
  if (!message || typeof message !== 'object') {
    return null;
  }

  const candidate = message as {
    status?: unknown;
    tables?: unknown;
  };

  if (
    candidate.status !== 'not_started'
    && candidate.status !== 'catching_up'
    && candidate.status !== 'live'
  ) {
    return null;
  }

  if (!candidate.tables || typeof candidate.tables !== 'object') {
    return null;
  }

  const tableStatuses: Record<string, TableSyncStatus> = {};
  Object.entries(candidate.tables as Record<string, unknown>).forEach(([tableName, tableStatus]) => {
    if (tableStatus === 'waiting' || tableStatus === 'catching_up' || tableStatus === 'live') {
      tableStatuses[tableName] = tableStatus;
    }
  });

  return {
    status: candidate.status,
    tables: tableStatuses,
  };
}

function getElmBridgePort(app: ElmBridgeApp, portName: string): ElmBridgePort | undefined {
  return app.ports?.[portName];
}

async function resolveCreateConfig(config: SingleDatabasePyreClientCreateConfig): Promise<SingleDatabasePyreClientConfig> {
  if ((config.server || config.session) && config.connect) {
    throw new Error('Provide either server/session or connect, not both');
  }

  const connected = config.connect ? await config.connect() : null;
  const server = connected?.server ?? config.server;

  if (!server) {
    throw new Error('SingleDatabasePyreClient.create requires server or connect');
  }

  const session = connected?.session ?? config.session;
  const databaseId = connected?.databaseId ?? config.databaseId;
  const cacheNamespace = connected?.cacheNamespace ?? config.cacheNamespace;
  const resolvedHeaders = await resolveServerHeaders(server);

  return {
    schema: config.schema,
    server,
    databaseId,
    cacheNamespace,
    autoStartSync: config.autoStartSync,
    resolvedHeaders,
    indexedDbName: config.indexedDbName,
    onError: config.onError,
    session,
    elm: config.elm,
  };
}

async function resolveMultiCreateConfig(config: PyreClientConfig): Promise<ResolvedPyreClientConfig> {
  if ((config.server || config.session || config.cacheNamespace) && config.connect) {
    throw new Error('Provide either server/session/cacheNamespace or connect, not both');
  }

  const connected = config.connect ? await config.connect() : null;
  const server = connected?.server ?? config.server;

  if (!server) {
    throw new Error('PyreClient.create requires server or connect');
  }

  const cacheNamespace = connected?.cacheNamespace ?? config.cacheNamespace;
  if (!cacheNamespace) {
    throw new Error('PyreClient.create requires cacheNamespace');
  }

  return {
    schema: config.schema,
    server,
    cacheNamespace,
    indexedDbName: config.indexedDbName,
    debug: config.debug,
    onError: config.onError,
    session: connected?.session ?? config.session,
    createInternalClient: config.createInternalClient ?? ((internalConfig) => SingleDatabasePyreClient.create(internalConfig)),
    elm: config.elm,
  };
}

function reportElmBridgeError(
  config: ElmBridgeConfig,
  error: unknown,
  phase: 'incoming-message' | 'mutation-handler'
): void {
  const normalized = error instanceof Error ? error : new Error(String(error));
  if (config.onError) {
    config.onError(normalized, { phase });
    return;
  }

  console.error('[PyreClient] Elm bridge error', phase, normalized);
}

function entityBridgeRegistrationMatches(
  registration: EntityBridgeRegistration,
  message: ElmBridgeEntityStreamRegisterMessage
): boolean {
  return registration.databaseId === message.databaseId
    && entitySubscriptionTablesEqual(registration.tables, message.tables);
}

function entitySubscriptionTablesEqual(
  left: EntitySubscription['tables'],
  right: EntitySubscription['tables']
): boolean {
  return left.length === right.length
    && left.every((leftTable, index) => {
      const rightTable = right[index];
      return leftTable.tableName === rightTable.tableName
        && entityValuesEqual(leftTable.where ?? null, rightTable.where ?? null);
    });
}

function entityValuesEqual(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) {
    return true;
  }

  if (Array.isArray(left) || Array.isArray(right)) {
    return Array.isArray(left)
      && Array.isArray(right)
      && left.length === right.length
      && left.every((value, index) => entityValuesEqual(value, right[index]));
  }

  if (isRecord(left) || isRecord(right)) {
    if (!isRecord(left) || !isRecord(right)) {
      return false;
    }

    const leftKeys = Object.keys(left).sort();
    const rightKeys = Object.keys(right).sort();
    return leftKeys.length === rightKeys.length
      && leftKeys.every((key, index) => key === rightKeys[index] && entityValuesEqual(left[key], right[key]));
  }

  return false;
}

function parseElmBridgeIncomingMessage(message: unknown): ElmBridgeIncomingMessage {
  const raw = asObject(message, 'Pyre bridge message');
  const type = raw.type;

  if (type === 'mutate') {
    return {
      type: 'mutate',
      databaseId: requireDatabaseId(raw.databaseId, 'mutate message databaseId'),
      requestId: asNonEmptyString(raw.requestId, 'mutate message requestId'),
      mutationId: asNonEmptyString(raw.mutationId, 'mutate message mutationId'),
      mutationName: typeof raw.mutationName === 'string' && raw.mutationName.trim() !== '' ? raw.mutationName : undefined,
      mutationInput: raw.mutationInput,
      optimistic: raw.optimistic,
    };
  }

  if (type === 'register-entity-stream') {
    const message = {
      type: 'register-entity-stream' as const,
      databaseId: requireDatabaseId(raw.databaseId, 'entity stream register databaseId'),
      streamId: asNonEmptyString(raw.streamId, 'entity stream register streamId'),
      tables: raw.tables,
    };
    validateEntitySubscription({ tables: message.tables as EntitySubscription['tables'] });
    return message as ElmBridgeEntityStreamRegisterMessage;
  }

  if (type === 'unregister-entity-stream') {
    return {
      type: 'unregister-entity-stream',
      streamId: asNonEmptyString(raw.streamId, 'entity stream unregister streamId'),
    };
  }

  if (type !== 'register' && type !== 'update-input' && type !== 'unregister') {
    throw new Error(`Unsupported Pyre bridge message type: ${String(type)}`);
  }

  return {
    type,
    databaseId: requireDatabaseId(raw.databaseId, 'Pyre bridge message databaseId'),
    queryId: asNonEmptyString(raw.queryId, 'Pyre bridge message queryId'),
    queryName: typeof raw.queryName === 'string' && raw.queryName.trim() !== '' ? raw.queryName : undefined,
    querySource: raw.querySource,
    queryInput: raw.queryInput,
  };
}

function debugOutEventName(message: unknown): string | null {
  if (!message || typeof message !== 'object' || Array.isArray(message)) {
    return null;
  }

  const event = (message as { event?: unknown }).event;
  return typeof event === 'string' && event.trim() !== ''
    ? event.trim()
    : null;
}

function internalDevtoolsEventType(event: PyreDevtoolsEvent): string {
  if (event.type !== 'debug') {
    return `internal.${event.type}`;
  }

  const args = (event.payload as { args?: unknown[] } | undefined)?.args;
  const label = Array.isArray(args) && typeof args[0] === 'string'
    ? args[0]
    : 'debug';

  return `internal.debug.${slugEventLabel(label)}`;
}

function slugEventLabel(label: string): string {
  const withoutPrefix = label.replace(/^\[PyreClient\]\s*/, '');
  const slug = withoutPrefix
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '.')
    .replace(/^\.+|\.+$/g, '')
    .slice(0, 80);

  return slug || 'debug';
}

function asObject(value: unknown, label: string): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }

  return value as Record<string, unknown>;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function asNonEmptyString(value: unknown, label: string): string {
  if (typeof value !== 'string' || value.trim() === '') {
    throw new Error(`${label} must be non-empty string`);
  }

  return value;
}

function asQueryShape(value: unknown): Record<string, unknown> {
  return asObject(value, 'Pyre bridge querySource');
}

function toSyncProgress(state: SyncState): SyncProgress {
  const tableStatuses = Object.values(state.tables);
  const totalTables = tableStatuses.length;
  const tablesSynced = tableStatuses.filter((status) => status === 'live').length;
  const activeTable = Object.entries(state.tables).find(([, status]) => status === 'catching_up')?.[0];

  return {
    table: activeTable,
    tablesSynced,
    totalTables,
    complete: state.status === 'live',
    error: state.error,
  };
}
