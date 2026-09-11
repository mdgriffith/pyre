import type { SchemaMetadata, TableMetadata } from '@pyre/core';

export type EntityId = number | string;

export function primaryKeyForTable(schema: SchemaMetadata, tableName: string): TableMetadata['primaryKey'] {
  const table = Object.prototype.hasOwnProperty.call(schema.tables, tableName) ? schema.tables[tableName] : undefined;
  const key = table?.primaryKey;
  if (!key || typeof key.name !== 'string' || key.name.length === 0 || (key.kind !== 'int' && key.kind !== 'uuid')) {
    throw new Error(`Missing or unsupported primary key metadata for table ${tableName}`);
  }
  return key;
}

export function rowIdentity(schema: SchemaMetadata, tableName: string, row: Record<string, unknown>): EntityId {
  const key = primaryKeyForTable(schema, tableName);
  const value = Object.prototype.hasOwnProperty.call(row, key.name) ? row[key.name] : undefined;
  if (key.kind === 'int' && typeof value === 'number' && Number.isSafeInteger(value)) {
    return value;
  }
  if (key.kind === 'uuid' && typeof value === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(value)) {
    return value;
  }
  throw new Error(`Invalid ${key.kind} identity for ${tableName}.${key.name}`);
}
