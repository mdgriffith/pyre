import type { SchemaMetadata } from '@pyre/core';
import { primaryKeyForTable, rowIdentity } from './identity';

export interface ServerTableGroup {
  table_name: string;
  headers: string[];
  rows: unknown[][];
}

export type EntityWhereValue =
  | string
  | number
  | boolean
  | null
  | { $eq: unknown }
  | { $ne: unknown }
  | { $in: unknown[] }
  | { $nin: unknown[] };

export type EntityWhere = Record<string, EntityWhereValue>;

export interface EntityTableSubscription {
  tableName: string;
  where?: EntityWhere;
}

export interface EntitySubscription {
  tables: EntityTableSubscription[];
}

export type EntityChange = {
  tableName: string;
  id: string | number;
  op: 'row';
  row: Record<string, unknown>;
} | { tableName: string; id: string | number; op: 'remove' };

export type EntityChangeBatchSource = 'indexeddb-initial' | 'catchup' | 'live' | 'optimistic' | 'mutation-response' | 'local-edits';

export interface EntityChangeBatch {
  type: 'entity-change-batch';
  databaseId?: string;
  sequence: number;
  source: EntityChangeBatchSource;
  changes: EntityChange[];
}

type EntityChangeCallback = (batch: EntityChangeBatch) => void;

interface EntityStreamRegistration {
  subscription: EntitySubscription;
  callback: EntityChangeCallback;
}

export class EntityStreamService {
  private registrations: Set<EntityStreamRegistration> = new Set();
  private sequence = 0;
  private visible = new Map<string, Array<Record<string, unknown>>>();

  constructor(private readonly schema: SchemaMetadata) {}

  clear(): void { this.visible.clear(); this.registrations.clear(); }

  getVisibleTables(): Record<string, Array<Record<string, unknown>>> { return Object.fromEntries(this.visible); }

  subscribeVisible(subscription: EntitySubscription, callback: EntityChangeCallback, databaseId?: string): () => void {
    const unsubscribe = this.subscribe(subscription, callback);
    callback(this.createBatchFromRows(subscription, this.visible, 'local-edits', databaseId) ?? {
      type: 'entity-change-batch', databaseId, sequence: this.sequence, source: 'local-edits', changes: [],
    });
    return unsubscribe;
  }

  /** Complete visible scope, including filter exits and absent identities. No persistence. */
  installVisible(tables: Record<string, Array<Record<string, unknown>>>, databaseId?: string): () => void {
    const next = new Map(Object.entries(tables));
    for (const [table, rows] of next) {
      const ids = rows.map(row => rowIdentity(this.schema, table, row));
      if (new Set(ids).size !== ids.length) throw new Error(`Duplicate entity identity for table ${table}`);
    }
    const sequence = this.reserveSequence();
    const notifications = [...this.registrations].map(registration => {
      const old = collectChanges(this.schema, registration.subscription, this.visible);
      const current = collectChanges(this.schema, registration.subscription, next);
      const key = (change: EntityChange) => JSON.stringify([change.tableName, change.id]);
      const keys = new Set(current.map(key));
      const changes: EntityChange[] = [
        ...old.filter(change => !keys.has(key(change))).map(change => ({ tableName: change.tableName, id: change.id, op: 'remove' as const })),
        ...current,
      ];
      return () => {
        if (!this.registrations.has(registration)) return;
        try { registration.callback({ type: 'entity-change-batch', databaseId, sequence, source: 'local-edits', changes }); }
        catch (error) { console.error('[PyreClient] Entity listener failed', error); }
      };
    });
    this.visible = next;
    return () => notifications.forEach(notify => notify());
  }

  subscribe(subscription: EntitySubscription, callback: EntityChangeCallback): () => void {
    validateEntitySubscription(subscription);
    subscription.tables.forEach((table) => primaryKeyForTable(this.schema, table.tableName));
    const registration = { subscription, callback };
    this.registrations.add(registration);
    return () => {
      this.registrations.delete(registration);
    };
  }

  reserveSequence(): number {
    this.sequence += 1;
    return this.sequence;
  }

  createBatchFromRows(
    subscription: EntitySubscription,
    rowsByTable: Map<string, Array<Record<string, unknown>>>,
    source: EntityChangeBatchSource,
    databaseId?: string,
    sequence = this.reserveSequence()
  ): EntityChangeBatch | null {
    validateEntitySubscription(subscription);
    const changes = collectChanges(this.schema, subscription, rowsByTable);
    if (changes.length === 0) {
      return null;
    }

    return {
      type: 'entity-change-batch',
      databaseId,
      sequence,
      source,
      changes,
    };
  }

  handleTableDelta(
    tableGroups: ServerTableGroup[],
    source: EntityChangeBatchSource,
    databaseId?: string
  ): void {
    const rowsByTable = expandTableGroups(tableGroups);
    rowsByTable.forEach((rows, tableName) => {
      primaryKeyForTable(this.schema, tableName);
      const identities = new Set<string | number>();
      rows.forEach((row) => {
        const id = rowIdentity(this.schema, tableName, row);
        if (identities.has(id)) throw new Error(`Duplicate entity identity for table ${tableName}`);
        identities.add(id);
      });
    });
    if (rowsByTable.size === 0) {
      return;
    }

    const batches = Array.from(this.registrations, (registration) => {
      const batch = this.createBatchFromRows(registration.subscription, rowsByTable, source, databaseId);
      return { registration, batch };
    });
    batches.forEach(({ registration, batch }) => {
      if (!batch) {
        return;
      }

      registration.callback(batch);
    });
  }
}

export function validateEntitySubscription(subscription: EntitySubscription): void {
  if (!isRecord(subscription)) {
    throw new Error('Entity subscription must be an object');
  }

  if (!Array.isArray(subscription.tables) || subscription.tables.length === 0) {
    throw new Error('Entity subscription must include at least one table');
  }

  subscription.tables.forEach((table, index) => {
    if (!isRecord(table)) {
      throw new Error(`Entity subscription table at index ${index} must be an object`);
    }

    if (typeof table.tableName !== 'string' || table.tableName.trim() === '') {
      throw new Error(`Entity subscription table at index ${index} must include a non-empty tableName`);
    }

    if (table.where !== undefined) {
      validateWhere(table.where, `Entity subscription table ${table.tableName} where`);
    }
  });
}

function validateWhere(where: unknown, label: string): void {
  if (!isRecord(where)) {
    throw new Error(`${label} must be an object`);
  }

  Object.entries(where).forEach(([field, condition]) => {
    if (field.trim() === '') {
      throw new Error(`${label} field names must be non-empty`);
    }

    validateWhereValue(condition, `${label}.${field}`);
  });
}

function validateWhereValue(value: unknown, label: string): void {
  if (value === null || typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean') {
    return;
  }

  if (!isRecord(value)) {
    throw new Error(`${label} must be a scalar value or supported operator object`);
  }

  const operators = Object.keys(value);
  if (operators.length !== 1) {
    throw new Error(`${label} must contain exactly one operator`);
  }

  const operator = operators[0];
  if (operator !== '$eq' && operator !== '$ne' && operator !== '$in' && operator !== '$nin') {
    throw new Error(`${label} uses unsupported operator ${operator}`);
  }

  if ((operator === '$in' || operator === '$nin') && !Array.isArray(value[operator])) {
    throw new Error(`${label}.${operator} must be an array`);
  }
}

export function expandTableGroups(tableGroups: ServerTableGroup[]): Map<string, Array<Record<string, unknown>>> {
  const rowsByTable = new Map<string, Array<Record<string, unknown>>>();
  if (!Array.isArray(tableGroups)) throw new Error('Invalid entity table groups');

  tableGroups.forEach((group) => {
    if (!group || typeof group.table_name !== 'string' || !group.table_name || !Array.isArray(group.headers) || !Array.isArray(group.rows)
      || group.headers.some((header) => typeof header !== 'string') || new Set(group.headers).size !== group.headers.length) {
      throw new Error('Invalid entity table group');
    }

    const rows = rowsByTable.get(group.table_name) ?? [];
    group.rows.forEach((values) => {
      if (!Array.isArray(values) || values.length !== group.headers.length) {
        throw new Error(`Invalid entity row for table ${group.table_name}`);
      }

      const row = Object.fromEntries(group.headers.map((header, index) => [header, values[index]]));
      rows.push(row);
    });

    rowsByTable.set(group.table_name, rows);
  });

  return rowsByTable;
}

function collectChanges(
  schema: SchemaMetadata,
  subscription: EntitySubscription,
  rowsByTable: Map<string, Array<Record<string, unknown>>>
): EntityChange[] {
  const changes: EntityChange[] = [];
  const emitted = new Set<string>();

  subscription.tables.forEach((tableSubscription) => {
    primaryKeyForTable(schema, tableSubscription.tableName);
    const rows = rowsByTable.get(tableSubscription.tableName);
    if (!rows) {
      return;
    }

    rows.forEach((row) => {
      const id = rowIdentity(schema, tableSubscription.tableName, row);
      if (!matchesWhere(row, tableSubscription.where)) {
        return;
      }

      const key = JSON.stringify([tableSubscription.tableName, typeof id, id]);
      if (emitted.has(key)) {
        return;
      }

      emitted.add(key);
      changes.push({
        tableName: tableSubscription.tableName,
        id,
        op: 'row',
        row,
      });
    });
  });

  return changes;
}

function matchesWhere(row: Record<string, unknown>, where?: EntityWhere): boolean {
  if (!where) {
    return true;
  }

  return Object.entries(where).every(([field, condition]) => matchesCondition(row[field], condition));
}

function matchesCondition(value: unknown, condition: EntityWhereValue): boolean {
  if (isOperatorCondition(condition)) {
    if ('$eq' in condition) {
      return valuesEqual(value, condition.$eq);
    }
    if ('$ne' in condition) {
      return !valuesEqual(value, condition.$ne);
    }
    if ('$in' in condition) {
      return condition.$in.some((candidate) => valuesEqual(value, candidate));
    }
    if ('$nin' in condition) {
      return condition.$nin.every((candidate) => !valuesEqual(value, candidate));
    }
  }

  return valuesEqual(value, condition);
}

function isOperatorCondition(value: EntityWhereValue): value is Extract<EntityWhereValue, object> {
  return value !== null && typeof value === 'object';
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function valuesEqual(left: unknown, right: unknown): boolean {
  return Object.is(left, right);
}
