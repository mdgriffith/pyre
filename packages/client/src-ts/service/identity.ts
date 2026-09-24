import type { SchemaMetadata } from '../types';

export function primaryKeys(schema: SchemaMetadata): Record<string, string> {
  return Object.fromEntries(Object.entries(schema.tables).map(([name, table]) =>
    [name, table.indices.find((index) => index.primary)?.field ?? 'id']));
}
