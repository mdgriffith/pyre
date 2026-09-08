import type { ElmApp } from '../types';
import type { EntityChangeBatchSource, ServerTableGroup } from './entity-stream';

export interface TableGroup {
  table_name: string;
  headers: string[];
  rows: unknown[][];
}

export interface SyncCursorEntry {
  last_seen_delete_sequence?: number;
  last_seen_updated_at: number | null;
  last_seen_primary_key?: number | string | null;
  permission_hash: string;
}

export interface SyncCursor {
  tables: Record<string, SyncCursorEntry>;
}

export interface PutRowsResult {
  tableName: string;
  received: number;
  written: number;
  skippedOlder: number;
}

const DB_VERSION = 4;

export class IndexedDBStorage {
  private dbName: string;
  private db: IDBDatabase | null = null;
  private initPromise: Promise<IDBDatabase> | null = null;

  constructor(dbName: string) {
    this.dbName = dbName;
  }

  async init(): Promise<IDBDatabase> {
    if (this.db) {
      return this.db;
    }

    if (this.initPromise) {
      return this.initPromise;
    }

    this.initPromise = new Promise((resolve, reject) => {
      const request = indexedDB.open(this.dbName, DB_VERSION);

      request.onerror = () => {
        this.initPromise = null;
        reject(new Error(`Failed to open IndexedDB: ${request.error}`));
      };

      request.onsuccess = () => {
        this.db = request.result;
        this.initPromise = null;
        resolve(this.db);
      };

      request.onupgradeneeded = (event) => {
        const db = (event.target as IDBOpenDBRequest).result;

        if (!db.objectStoreNames.contains('tables')) {
          const tablesStore = db.createObjectStore('tables', { keyPath: ['tableName', 'id'] });
          tablesStore.createIndex('byTable', 'tableName', { unique: false });
          tablesStore.createIndex('byUpdatedAt', 'updatedAt', { unique: false });
        }

        if (!db.objectStoreNames.contains('syncCursor')) {
          db.createObjectStore('syncCursor');
        }

        if (!db.objectStoreNames.contains('meta')) {
            db.createObjectStore('meta');
        }
        // Older caches either lack deletion coverage or persisted unfenced
        // pagination positions. Neither proves a safe timestamp checkpoint.
        // This is a one-time local cache upgrade, not a server epoch rotation.
        if (event.oldVersion > 0 && event.oldVersion < 4) {
          for (const name of ['tables', 'syncCursor', 'meta']) request.transaction!.objectStore(name).clear();
        }
      };
    });

    return this.initPromise;
  }

  private async getDB(): Promise<IDBDatabase> {
    if (!this.db) {
      await this.init();
    }
    if (!this.db) {
      throw new Error('Failed to initialize database');
    }
    return this.db;
  }

  async getAllRows(tableName: string): Promise<unknown[]> {
    const db = await this.getDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(['tables'], 'readonly');
      const store = tx.objectStore('tables');
      const index = store.index('byTable');
      const range = IDBKeyRange.only(tableName);
      const request = index.getAll(range);

      request.onsuccess = () => {
        const result = request.result || [];
        resolve(result.map((row) => {
          const { tableName, ...rest } = row as { tableName: string };
          return rest;
        }));
      };

      request.onerror = () => {
        reject(new Error(`Failed to read rows: ${request.error}`));
      };
    });
  }

  async getRowsPage(tableName: string, offset = 0, limit = 100): Promise<{ rows: unknown[]; hasMore: boolean }> {
    const db = await this.getDB();
    const normalizedOffset = Math.max(0, Math.floor(offset));
    const normalizedLimit = Math.max(1, Math.min(500, Math.floor(limit)));
    return new Promise((resolve, reject) => {
      const tx = db.transaction(['tables'], 'readonly');
      const store = tx.objectStore('tables');
      const index = store.index('byTable');
      const range = IDBKeyRange.only(tableName);
      const rows: unknown[] = [];
      let skipped = 0;
      let hasMore = false;
      const request = index.openCursor(range);

      request.onsuccess = () => {
        const cursor = request.result;
        if (!cursor) {
          resolve({ rows, hasMore });
          return;
        }

        if (skipped < normalizedOffset) {
          skipped += 1;
          cursor.continue();
          return;
        }

        if (rows.length >= normalizedLimit) {
          hasMore = true;
          resolve({ rows, hasMore });
          return;
        }

        const { tableName: _, ...rest } = cursor.value as { tableName: string };
        rows.push(rest);
        cursor.continue();
      };

      request.onerror = () => {
        reject(new Error(`Failed to read rows: ${request.error}`));
      };
    });
  }

  async countRows(tableName: string): Promise<number> {
    const db = await this.getDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(['tables'], 'readonly');
      const store = tx.objectStore('tables');
      const index = store.index('byTable');
      const request = index.count(IDBKeyRange.only(tableName));

      request.onsuccess = () => {
        resolve(request.result);
      };

      request.onerror = () => {
        reject(new Error(`Failed to count rows: ${request.error}`));
      };
    });
  }

  async getAllTables(): Promise<Record<string, unknown[]>> {
    const db = await this.getDB();
    const tables: Record<string, unknown[]> = {};

    return new Promise((resolve, reject) => {
      const tx = db.transaction(['tables'], 'readonly');
      const store = tx.objectStore('tables');
      const request = store.getAll();

      request.onsuccess = () => {
        const allRows = request.result || [];

        for (const row of allRows) {
          const tableName = (row as { tableName: string }).tableName;
          if (!tables[tableName]) {
            tables[tableName] = [];
          }
          const { tableName: _, ...rest } = row as { tableName: string };
          tables[tableName].push(rest);
        }

        resolve(tables);
      };

      request.onerror = () => {
        reject(new Error(`Failed to read tables: ${request.error}`));
      };
    });
  }

  async getSyncCursor(): Promise<SyncCursor> {
    const db = await this.getDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(['syncCursor'], 'readonly');
      const store = tx.objectStore('syncCursor');
      const request = store.get('cursor');

      request.onsuccess = () => {
        resolve((request.result as SyncCursor | undefined) ?? { tables: {} });
      };

      request.onerror = () => {
        reject(new Error(`Failed to read sync cursor: ${request.error}`));
      };
    });
  }

  async getInitialSnapshot(): Promise<{ tables: Record<string, unknown[]>; cursor: SyncCursor; lastAppliedServerRevision: number | null; databaseEpoch: string | null }> {
    const db = await this.getDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(['tables', 'syncCursor', 'meta'], 'readonly');
      const snapshot = { tables: {} as Record<string, unknown[]>, cursor: { tables: {} } as SyncCursor, lastAppliedServerRevision: null as number | null, databaseEpoch: null as string | null };
      const rows = tx.objectStore('tables').getAll();
      rows.onsuccess = () => {
        for (const { tableName, ...row } of rows.result) (snapshot.tables[tableName] ??= []).push(row);
      };
      const cursor = tx.objectStore('syncCursor').get('cursor');
      cursor.onsuccess = () => { snapshot.cursor = cursor.result ?? { tables: {} }; };
      const revision = tx.objectStore('meta').get('lastAppliedServerRevision');
      revision.onsuccess = () => { snapshot.lastAppliedServerRevision = revision.result ?? null; };
      const epoch = tx.objectStore('meta').get('databaseEpoch');
      epoch.onsuccess = () => { snapshot.databaseEpoch = epoch.result ?? null; };
      tx.oncomplete = () => resolve(snapshot);
      tx.onerror = () => reject(new Error(`Failed to read sync snapshot: ${tx.error}`));
      tx.onabort = () => reject(new Error(`Sync snapshot read aborted: ${tx.error}`));
    });
  }

  async putSyncCursor(cursor: SyncCursor): Promise<void> {
    const db = await this.getDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(['syncCursor'], 'readwrite');
      const store = tx.objectStore('syncCursor');
      const request = store.put(cursor, 'cursor');

      request.onsuccess = () => {
        resolve();
      };

      request.onerror = () => {
        reject(new Error(`Failed to write sync cursor: ${request.error}`));
      };
    });
  }

  async getServerRevision(): Promise<number | null> {
    const db = await this.getDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(['meta'], 'readonly');
      const store = tx.objectStore('meta');
      const request = store.get('lastAppliedServerRevision');

      request.onsuccess = () => {
        const value = request.result;
        resolve(typeof value === 'number' ? value : null);
      };

      request.onerror = () => {
        reject(new Error(`Failed to read server revision: ${request.error}`));
      };
    });
  }

  async putServerRevision(serverRevision: number): Promise<void> {
    const db = await this.getDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(['meta'], 'readwrite');
      const store = tx.objectStore('meta');
      const request = store.put(serverRevision, 'lastAppliedServerRevision');

      request.onsuccess = () => {
        resolve();
      };

      request.onerror = () => {
        reject(new Error(`Failed to write server revision: ${request.error}`));
      };
    });
  }

  async getDatabaseEpoch(): Promise<string | null> {
    const db = await this.getDB();
    return new Promise((resolve, reject) => {
      const request = db.transaction(['meta'], 'readonly').objectStore('meta').get('databaseEpoch');
      request.onsuccess = () => resolve(typeof request.result === 'string' ? request.result : null);
      request.onerror = () => reject(new Error(`Failed to read database epoch: ${request.error}`));
    });
  }

  async putDatabaseEpoch(databaseEpoch: string): Promise<void> {
    const db = await this.getDB();
    return new Promise((resolve, reject) => {
      const request = db.transaction(['meta'], 'readwrite').objectStore('meta').put(databaseEpoch, 'databaseEpoch');
      request.onsuccess = () => resolve();
      request.onerror = () => reject(new Error(`Failed to write database epoch: ${request.error}`));
    });
  }

  async resetForDatabaseEpoch(databaseEpoch: string): Promise<void> {
    const db = await this.getDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(['tables', 'syncCursor', 'meta'], 'readwrite');
      tx.objectStore('tables').clear();
      tx.objectStore('syncCursor').clear();
      const meta = tx.objectStore('meta');
      meta.delete('lastAppliedServerRevision');
      meta.put(databaseEpoch, 'databaseEpoch');
      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(new Error(`Failed to reset database epoch: ${tx.error}`));
      tx.onabort = () => reject(new Error(`Database epoch reset aborted: ${tx.error}`));
    });
  }

  async putRows(tableName: string, rows: Array<Record<string, unknown>>): Promise<PutRowsResult> {
    if (rows.length === 0) {
      return { tableName, received: 0, written: 0, skippedOlder: 0 };
    }

    const db = await this.getDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(['tables'], 'readwrite');
      const store = tx.objectStore('tables');

      let error: Error | null = null;
      let written = 0;
      let skippedOlder = 0;
      const existingRows: (Record<string, unknown> | null)[] = new Array(rows.length);
      let readsCompleted = 0;

      tx.oncomplete = () => {
        if (error) {
          reject(error);
        } else {
          resolve({ tableName, received: rows.length, written, skippedOlder });
        }
      };

      tx.onerror = () => {
        reject(new Error(`Transaction failed: ${tx.error}`));
      };

      rows.forEach((row, index) => {
        const request = store.get([tableName, row.id as IDBValidKey]);
        request.onsuccess = () => {
          existingRows[index] = request.result || null;
          readsCompleted += 1;

          if (readsCompleted === rows.length) {
            processWrites();
          }
        };
        request.onerror = () => {
          existingRows[index] = null;
          readsCompleted += 1;

          if (readsCompleted === rows.length) {
            processWrites();
          }
        };
      });

      const processWrites = () => {
        rows.forEach((row, index) => {
          const existing = existingRows[index];

          if (existing && existing.updatedAt != null && row.updatedAt != null) {
            const existingTime = typeof existing.updatedAt === 'number'
              ? existing.updatedAt
              : new Date(existing.updatedAt as string).getTime() / 1000;
            const newTime = typeof row.updatedAt === 'number'
              ? row.updatedAt
              : new Date(row.updatedAt as string).getTime() / 1000;

            if (existingTime > newTime) {
              skippedOlder += 1;
              return;
            }
          } else if (existing && existing.updatedAt != null && row.updatedAt == null) {
            skippedOlder += 1;
            return;
          }

          const rowWithTable = { ...row, tableName };
          const request = store.put(rowWithTable);

          request.onsuccess = () => {
            written += 1;
          };

          request.onerror = () => {
            error = new Error(`Failed to write row: ${request.error}`);
          };
        });
      };
    });
  }

  async putCatchupPage(databaseEpoch: string, serverRevision: number, cursor: SyncCursor, groups: TableGroup[]): Promise<void> {
    const db = await this.getDB();
    return new Promise((resolve, reject) => {
      const tx = db.transaction(['tables', 'syncCursor', 'meta'], 'readwrite');
      let failure: Error | undefined;
      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(failure ?? new Error(`Catchup transaction failed: ${tx.error}`));
      tx.onabort = () => reject(failure ?? new Error(`Catchup transaction aborted: ${tx.error}`));
      const meta = tx.objectStore('meta');
      const epoch = meta.get('databaseEpoch');
      epoch.onsuccess = () => {
        if (epoch.result !== undefined && epoch.result !== databaseEpoch) {
          failure = new Error('Catchup page epoch does not match storage');
          tx.abort();
          return;
        }
        const revision = meta.get('lastAppliedServerRevision');
        revision.onsuccess = () => {
          if (revision.result !== undefined && serverRevision < revision.result) {
            failure = new Error('Stale catchup revision');
            tx.abort();
          }
        };
        const cursors = tx.objectStore('syncCursor');
        const previous = cursors.get('cursor');
        previous.onsuccess = () => {
          try {
            for (const [table, next] of Object.entries(cursor.tables)) {
              const old = (previous.result as SyncCursor | undefined)?.tables[table];
              // Catchup checkpoints deliberately overlap their fenced interval.
              // Snapshot revision and serialized page acknowledgements provide
              // ordering; a smaller key at the same timestamp is not stale.
              if (old?.permission_hash === next.permission_hash && (next.last_seen_delete_sequence ?? 0) < (old.last_seen_delete_sequence ?? 0)) throw new Error('Stale deletion cursor');
            }
            const store = tx.objectStore('tables');
            for (const group of groups) {
              for (const values of group.rows) {
                if (group.headers.length === 1 && group.headers[0] === '$delete') {
                  const key = values[0];
                  if (typeof key !== 'string' && !Number.isSafeInteger(key)) throw new Error('Invalid deletion key');
                  store.delete([group.table_name, key as IDBValidKey]);
                } else {
                  const row = Object.fromEntries(group.headers.map((header, index) => [header, values[index]]));
                  store.put({ ...row, tableName: group.table_name });
                }
              }
            }
            cursors.put(cursor, 'cursor');
            meta.put(databaseEpoch, 'databaseEpoch');
            meta.put(serverRevision, 'lastAppliedServerRevision');
          } catch (error) {
            failure = error instanceof Error ? error : new Error(String(error));
            tx.abort();
          }
        };
      };
    });
  }

  async deleteDatabase(): Promise<void> {
    if (this.db) {
      this.db.close();
      this.db = null;
    }

    return new Promise((resolve, reject) => {
      const request = indexedDB.deleteDatabase(this.dbName);

      request.onsuccess = () => resolve();
      request.onerror = () => reject(new Error(`Failed to delete database: ${request.error}`));
      request.onblocked = () => reject(new Error('Database deletion blocked'));
    });
  }
}

export class IndexedDbService {
  private storage: IndexedDBStorage;
  private elmApp: ElmApp | null = null;
  private debugLog: (...args: unknown[]) => void;
  private onEntityDelta: ((tableGroups: ServerTableGroup[], source: EntityChangeBatchSource) => void) | null;
  private onDatabaseEpochReset: (() => void) | null;
  private onDatabaseEpochStored: ((databaseEpoch: string) => void) | null;
  private operationQueue: Promise<void> = Promise.resolve();

  constructor(
    storage: IndexedDBStorage,
    debugLog?: (...args: unknown[]) => void,
    onEntityDelta?: (tableGroups: ServerTableGroup[], source: EntityChangeBatchSource) => void,
    onDatabaseEpochReset?: () => void,
    onDatabaseEpochStored?: (databaseEpoch: string) => void,
  ) {
    this.storage = storage;
    this.debugLog = debugLog ?? (() => {});
    this.onEntityDelta = onEntityDelta ?? null;
    this.onDatabaseEpochReset = onDatabaseEpochReset ?? null;
    this.onDatabaseEpochStored = onDatabaseEpochStored ?? null;
  }

  attachPorts(elmApp: ElmApp): void {
    this.elmApp = elmApp;

    if (elmApp.ports.indexedDbOut) {
      elmApp.ports.indexedDbOut.subscribe((message) => {
        this.debugLog('[PyreClient] port indexedDbOut <-', message);
        this.operationQueue = this.operationQueue
          .then(() => this.handleMessage(message as { type?: string; tableGroups?: TableGroup[]; databaseEpoch?: string }))
          .catch((error) => {
            console.error('[PyreClient] IndexedDB handler failed:', error);
          });
      });
    }
  }

  private async handleMessage(message: { type?: string; tableGroups?: TableGroup[]; cursor?: SyncCursor; serverRevision?: number; databaseEpoch?: string; entityStreamSource?: string }): Promise<void> {
    if (message.type === 'writeCatchupPage') {
      try {
        if (!message.databaseEpoch || !message.cursor || !Number.isSafeInteger(message.serverRevision)) throw new Error('Invalid catchup page');
        await this.storage.putCatchupPage(message.databaseEpoch, message.serverRevision!, message.cursor, message.tableGroups ?? []);
        this.onDatabaseEpochStored?.(message.databaseEpoch);
        try {
          const source = message.entityStreamSource === 'live' || message.entityStreamSource === 'mutation-response' ? message.entityStreamSource : 'catchup';
          this.onEntityDelta?.(message.tableGroups ?? [], source);
        } catch (error) {
          console.error('[PyreClient] Entity subscriber failed:', error);
        }
        this.elmApp?.ports.receiveIndexedDbMessage?.send({ type: 'catchupPageStored', databaseEpoch: message.databaseEpoch });
      } catch (error) {
        this.elmApp?.ports.receiveIndexedDbMessage?.send({ type: 'catchupPageFailed', error: String(error) });
      }
      return;
    }
    if (message.type === 'requestInitialData') {
      await this.sendInitialData();
      return;
    }

    if (message.type === 'writeDelta') {
      await this.writeDelta(message.tableGroups || [], message.entityStreamSource);
      return;
    }

    if (message.type === 'writeSyncCursor' && message.cursor) {
      await this.writeSyncCursor(message.cursor);
      return;
    }

    if (message.type === 'writeServerRevision' && typeof message.serverRevision === 'number') {
      await this.writeServerRevision(message.serverRevision);
      return;
    }

    if (message.type === 'writeDatabaseEpoch' && typeof message.databaseEpoch === 'string') {
      await this.storage.putDatabaseEpoch(message.databaseEpoch);
      this.onDatabaseEpochStored?.(message.databaseEpoch);
      return;
    }

    if (message.type === 'resetForDatabaseEpoch' && typeof message.databaseEpoch === 'string') {
      await this.resetForDatabaseEpoch(message.databaseEpoch);
    }
  }

  private async sendInitialData(): Promise<void> {
    if (!this.elmApp?.ports.receiveIndexedDbMessage) {
      return;
    }

    try {
      const startedAt = Date.now();
      this.debugLog('[PyreClient] IndexedDB initial data request started');
      await this.storage.init();
      const { tables, cursor, lastAppliedServerRevision, databaseEpoch } = await this.storage.getInitialSnapshot();

      const tableCounts = Object.fromEntries(
        Object.entries(tables).map(([tableName, rows]) => [tableName, rows.length])
      );
      const totalRowCount = Object.values(tableCounts).reduce((sum, count) => sum + count, 0);

      this.elmApp.ports.receiveIndexedDbMessage.send({
        type: 'initialData',
        data: { tables, cursor, lastAppliedServerRevision, databaseEpoch },
      });
      this.debugLog('[PyreClient] IndexedDB initial data loaded', {
        tableCounts,
        totalRowCount,
        cursorTables: Object.keys(cursor.tables).length,
        elapsedMs: Date.now() - startedAt,
      });
    } catch (error) {
      console.error('[PyreClient] Failed to load initial data:', error);
      const fallbackMessage = { type: 'initialData', data: { tables: {}, cursor: { tables: {} }, lastAppliedServerRevision: null, databaseEpoch: null } };
      this.elmApp.ports.receiveIndexedDbMessage.send(fallbackMessage);
      this.debugLog('[PyreClient] port receiveIndexedDbMessage ->', fallbackMessage);
    }
  }

  private async resetForDatabaseEpoch(databaseEpoch: string): Promise<void> {
    if (!this.elmApp?.ports.receiveIndexedDbMessage) {
      return;
    }
    try {
      await this.storage.resetForDatabaseEpoch(databaseEpoch);
      this.onDatabaseEpochReset?.();
      this.onDatabaseEpochStored?.(databaseEpoch);
      this.elmApp.ports.receiveIndexedDbMessage.send({
        type: 'databaseEpochResetCompleted',
        databaseEpoch,
      });
    } catch (error) {
      this.elmApp.ports.receiveIndexedDbMessage.send({
        type: 'databaseEpochResetFailed',
        databaseEpoch,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }

  private async writeDelta(tableGroups: TableGroup[], entityStreamSource?: string): Promise<void> {
    try {
      await this.storage.init();

      this.debugLog('[PyreClient] IndexedDB writeDelta table groups received', {
        tableGroupCount: tableGroups.length,
        rowCount: tableGroups.reduce((sum, group) => sum + group.rows.length, 0),
      });

      for (const tableGroup of tableGroups) {
        const tableName = tableGroup.table_name;
        if (!tableName) {
          continue;
        }

        const rows = tableGroup.rows.map((rowArray) => {
          const rowObj: Record<string, unknown> = {};
          tableGroup.headers.forEach((header, index) => {
            rowObj[header] = rowArray[index];
          });
          return rowObj;
        });

        try {
          const result = await this.storage.putRows(tableName, rows);
          this.debugLog('[PyreClient] IndexedDB writeDelta table written', result);
        } catch (error) {
          console.error('[PyreClient] Failed to write delta table:', tableName, error, {
            rows: rows.length,
            firstRowId: rows[0]?.id,
            firstRowKeys: rows[0] ? Object.keys(rows[0]) : [],
          });
        }
      }

      this.notifyEntityDelta(tableGroups, entityStreamSource);
    } catch (error) {
      console.error('[PyreClient] Failed to write delta:', error);
    }
  }

  private async writeSyncCursor(cursor: SyncCursor): Promise<void> {
    try {
      await this.storage.init();
      await this.storage.putSyncCursor(cursor);
      this.debugLog('[PyreClient] IndexedDB sync cursor written', { cursorTables: Object.keys(cursor.tables).length });
    } catch (error) {
      console.error('[PyreClient] Failed to write sync cursor:', error);
    }
  }

  private async writeServerRevision(serverRevision: number): Promise<void> {
    try {
      await this.storage.init();
      await this.storage.putServerRevision(serverRevision);
      this.debugLog('[PyreClient] IndexedDB server revision written', { serverRevision });
    } catch (error) {
      console.error('[PyreClient] Failed to write server revision:', error);
    }
  }

  private notifyEntityDelta(tableGroups: TableGroup[], source: string | undefined): void {
    if (!this.onEntityDelta || tableGroups.length === 0) {
      return;
    }

    if (source !== 'catchup') {
      return;
    }

    this.onEntityDelta(tableGroups, source);
  }
}
