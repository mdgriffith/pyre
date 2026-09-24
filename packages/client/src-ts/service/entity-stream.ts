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

export interface EntityChange {
  tableName: string;
  id: string | number;
  op: 'row' | 'remove';
  row: Record<string, unknown>;
}

export type EntityChangeBatchSource = 'indexeddb-initial' | 'catchup' | 'live' | 'optimistic' | 'mutation-response';

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
  constructor(private primaryKeys: Record<string, string> = {}) {}

  private registrations: Set<EntityStreamRegistration> = new Set();
  private sequence = 0;
  private visibleRows = new Map<string, Array<Record<string, unknown>>>();

  snapshot(): Map<string, Array<Record<string, unknown>>> {
    return structuredClone(this.visibleRows);
  }

  handleVisibleState(tableGroups: ServerTableGroup[], source: EntityChangeBatchSource, databaseId?: string): void {
    const next = expandTableGroups(tableGroups);
    const previous = this.visibleRows;
    this.visibleRows = next;
    this.registrations.forEach(({ subscription, callback }) => {
      const before = collectChanges(subscription, previous, this.primaryKeys);
      const after = collectChanges(subscription, next, this.primaryKeys);
      const key = (change: EntityChange) => JSON.stringify([change.tableName, change.id]);
      const oldRows = new Map(before.map((change) => [key(change), change]));
      const newRows = new Map(after.map((change) => [key(change), change]));
      const changes: EntityChange[] = [
        ...before.filter((change) => !newRows.has(key(change))).map((change): EntityChange => ({ ...change, op: 'remove', row: { [this.primaryKeys[change.tableName] ?? 'id']: change.id } })),
        ...after.filter((change) => JSON.stringify(oldRows.get(key(change))?.row) !== JSON.stringify(change.row)),
      ];
      if (changes.length > 0) callback({ type: 'entity-change-batch', databaseId, sequence: this.reserveSequence(), source, changes: structuredClone(changes) });
    });
  }

  subscribe(subscription: EntitySubscription, callback: EntityChangeCallback): () => void {
    validateEntitySubscription(subscription);
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
    const changes = collectChanges(subscription, rowsByTable, this.primaryKeys);
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
    if (this.registrations.size === 0 || tableGroups.length === 0) {
      return;
    }

    const rowsByTable = expandTableGroups(tableGroups);
    if (rowsByTable.size === 0) {
      return;
    }

    this.registrations.forEach((registration) => {
      const batch = this.createBatchFromRows(registration.subscription, rowsByTable, source, databaseId);
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

function expandTableGroups(tableGroups: ServerTableGroup[]): Map<string, Array<Record<string, unknown>>> {
  const rowsByTable = new Map<string, Array<Record<string, unknown>>>();

  tableGroups.forEach((group) => {
    if (!group.table_name || !Array.isArray(group.headers) || !Array.isArray(group.rows)) {
      return;
    }

    const rows = rowsByTable.get(group.table_name) ?? [];
    group.rows.forEach((values) => {
      if (!Array.isArray(values)) {
        return;
      }

      const row: Record<string, unknown> = {};
      group.headers.forEach((header, index) => {
        row[header] = values[index];
      });
      rows.push(row);
    });

    if (rows.length > 0) {
      rowsByTable.set(group.table_name, rows);
    }
  });

  return rowsByTable;
}

function collectChanges(
  subscription: EntitySubscription,
  rowsByTable: Map<string, Array<Record<string, unknown>>>,
  primaryKeys: Record<string, string>
): EntityChange[] {
  const changes: EntityChange[] = [];
  const emitted = new Set<string>();

  subscription.tables.forEach((tableSubscription) => {
    const rows = rowsByTable.get(tableSubscription.tableName);
    if (!rows) {
      return;
    }

    rows.forEach((row) => {
      const id = row[primaryKeys[tableSubscription.tableName] ?? 'id'];
      if ((typeof id !== 'string' && typeof id !== 'number') || !matchesWhere(row, tableSubscription.where)) {
        return;
      }

      const key = `${tableSubscription.tableName}:${String(id)}`;
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
