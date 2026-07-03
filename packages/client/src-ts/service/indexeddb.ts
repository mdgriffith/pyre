import type { ElmApp } from '../types';
import type { EntityChangeBatchSource, ServerTableGroup } from './entity-stream';

export interface TableGroup {
  table_name: string;
  headers: string[];
  rows: unknown[][];
}

export interface SyncCursorEntry {
  last_seen_updated_at: number | null;
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

const DB_VERSION = 2;

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

  constructor(
    storage: IndexedDBStorage,
    debugLog?: (...args: unknown[]) => void,
    onEntityDelta?: (tableGroups: ServerTableGroup[], source: EntityChangeBatchSource) => void
  ) {
    this.storage = storage;
    this.debugLog = debugLog ?? (() => {});
    this.onEntityDelta = onEntityDelta ?? null;
  }

  attachPorts(elmApp: ElmApp): void {
    this.elmApp = elmApp;

    if (elmApp.ports.indexedDbOut) {
      elmApp.ports.indexedDbOut.subscribe((message) => {
        this.debugLog('[PyreClient] port indexedDbOut <-', message);
        this.handleMessage(message as { type?: string; tableGroups?: TableGroup[] }).catch((error) => {
          console.error('[PyreClient] IndexedDB handler failed:', error);
        });
      });
    }
  }

  private async handleMessage(message: { type?: string; tableGroups?: TableGroup[]; cursor?: SyncCursor; serverRevision?: number; entityStreamSource?: string }): Promise<void> {
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
      const tables = await this.storage.getAllTables();
      const cursor = await this.storage.getSyncCursor();
      const lastAppliedServerRevision = await this.storage.getServerRevision();

      const tableCounts = Object.fromEntries(
        Object.entries(tables).map(([tableName, rows]) => [tableName, rows.length])
      );
      const totalRowCount = Object.values(tableCounts).reduce((sum, count) => sum + count, 0);

      this.elmApp.ports.receiveIndexedDbMessage.send({
        type: 'initialData',
        data: { tables, cursor, lastAppliedServerRevision },
      });
      this.debugLog('[PyreClient] IndexedDB initial data loaded', {
        tableCounts,
        totalRowCount,
        cursorTables: Object.keys(cursor.tables).length,
        elapsedMs: Date.now() - startedAt,
      });
    } catch (error) {
      console.error('[PyreClient] Failed to load initial data:', error);
      const fallbackMessage = { type: 'initialData', data: { tables: {}, cursor: { tables: {} }, lastAppliedServerRevision: null } };
      this.elmApp.ports.receiveIndexedDbMessage.send(fallbackMessage);
      this.debugLog('[PyreClient] port receiveIndexedDbMessage ->', fallbackMessage);
    }
  }

  private async writeDelta(tableGroups: TableGroup[], entityStreamSource?: string): Promise<void> {
    try {
      await this.storage.init();

      this.debugLog('[PyreClient] IndexedDB writeDelta table groups received', tableGroups.map((group) => ({
        tableName: group.table_name,
        rows: group.rows.length,
        headers: group.headers,
      })));

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
