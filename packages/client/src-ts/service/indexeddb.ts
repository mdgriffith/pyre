import type { ElmApp } from '../types';
import type { SchemaMetadata } from '@pyre/core';
import { primaryKeyForTable, rowIdentity } from './identity';
import { expandTableGroups } from './entity-stream';
import type { EntityChangeBatchSource, ServerTableGroup } from './entity-stream';

export interface TableGroup {
  table_name: string;
  headers: string[];
  rows: unknown[][];
}

export interface SyncCursorEntry {
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

// v1/v2 flattened row.id caches cannot be safely reinterpreted. Reset rows and
// their progress together so the worker fetches the complete scope again.
const DB_VERSION = 3;

export class IndexedDBStorage {
  private dbName: string;
  private db: IDBDatabase | null = null;
  private initPromise: Promise<IDBDatabase> | null = null;

  constructor(dbName: string, private readonly schema: SchemaMetadata) {
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
        this.db.onversionchange = () => {
          this.db?.close();
          this.db = null;
        };
        this.initPromise = null;
        resolve(this.db);
      };

      request.onupgradeneeded = (event) => {
        const db = (event.target as IDBOpenDBRequest).result;

        if (event.oldVersion > 0 && event.oldVersion < 3) {
          for (const name of ['tables', 'syncCursor', 'meta']) {
            if (db.objectStoreNames.contains(name)) db.deleteObjectStore(name);
          }
        }

        if (!db.objectStoreNames.contains('tables')) {
          const tablesStore = db.createObjectStore('tables', { keyPath: ['tableName', 'identity'] });
          tablesStore.createIndex('byTable', 'tableName', { unique: false });
          tablesStore.createIndex('byUpdatedAt', 'updatedAt', { unique: false });
        }

        if (!db.objectStoreNames.contains('syncCursor')) {
          db.createObjectStore('syncCursor');
        }

        if (!db.objectStoreNames.contains('meta')) {
          db.createObjectStore('meta');
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
        try {
          resolve(result.map((row) => this.unpackRow(row)));
        } catch (error) { reject(error); }
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

        try {
          rows.push(this.unpackRow(cursor.value));
        } catch (error) { reject(error); return; }
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
    const tables: Record<string, unknown[]> = Object.create(null);

    return new Promise((resolve, reject) => {
      const tx = db.transaction(['tables'], 'readonly');
      const store = tx.objectStore('tables');
      const request = store.getAll();

      request.onsuccess = () => {
        const allRows = request.result || [];

        try {
          for (const row of allRows) {
            const tableName = (row as { tableName: string }).tableName;
            if (!tables[tableName]) {
              tables[tableName] = [];
            }
            tables[tableName].push(this.unpackRow(row));
          }
        } catch (error) {
          reject(error);
          return;
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

  private unpackRow(stored: { tableName: string; identity: unknown; row: Record<string, unknown> }): Record<string, unknown> {
    if (!stored.row || rowIdentity(this.schema, stored.tableName, stored.row) !== stored.identity) {
      throw new Error(`Invalid persisted identity for table ${stored.tableName}`);
    }
    return stored.row;
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
    primaryKeyForTable(this.schema, tableName);
    const identities = rows.map((row) => rowIdentity(this.schema, tableName, row));
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
      tx.onabort = () => reject(new Error(`Transaction aborted: ${tx.error}`));

      rows.forEach((row, index) => {
        const request = store.get([tableName, identities[index]]);
        request.onsuccess = () => {
          existingRows[index] = request.result?.row || null;
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

          const rowWithTable = { tableName, identity: identities[index], updatedAt: row.updatedAt, row };
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
  private initialData: Promise<{
    tables: Record<string, unknown[]>;
    cursor: SyncCursor;
    lastAppliedServerRevision: number | null;
    databaseEpoch: string | null;
  }> | null = null;

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

  initialize() {
    // Share both success and failure: neither client may resume from progress
    // until the entire persisted snapshot has passed identity validation.
    return this.initialData ??= (async () => {
      await this.storage.init();
      const tables = await this.storage.getAllTables();
      const cursor = await this.storage.getSyncCursor();
      const lastAppliedServerRevision = await this.storage.getServerRevision();
      const databaseEpoch = await this.storage.getDatabaseEpoch();
      return { tables, cursor, lastAppliedServerRevision, databaseEpoch };
    })();
  }

  private async sendInitialData(): Promise<void> {
    if (!this.elmApp?.ports.receiveIndexedDbMessage) {
      return;
    }

    try {
      const startedAt = Date.now();
      this.debugLog('[PyreClient] IndexedDB initial data request started');
      const { tables, cursor, lastAppliedServerRevision, databaseEpoch } = await this.initialize();

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
      throw error;
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

      for (const [tableName, rows] of expandTableGroups(tableGroups)) {
        const result = await this.storage.putRows(tableName, rows);
        this.debugLog('[PyreClient] IndexedDB writeDelta table written', result);
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
