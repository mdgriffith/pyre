// @ts-nocheck -- Bun test globals follow the other client runtime tests.
import { expect, test } from 'bun:test';
import { batch, captureOperations, createId, operation } from './operations';

test('create IDs are canonical UUIDv7 and captured once across repeated submissions', () => {
  const id = createId();
  expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  const input = { id, payload: { value: 'original' } };
  const edit = operation({ operation: 'insert', id: 'create', primary_db: 'Main' }, input);
  input.payload.value = 'changed';
  const first = captureOperations([edit]);
  (first[0].input as typeof input).payload.value = 'transport changed';
  expect(captureOperations([edit])[0].input).toEqual({ id, payload: { value: 'original' } });
  expect(createId()).not.toBe(id);
});

test('composition preserves order and rejects cross-namespace or forged operations', () => {
  const make = (primary_db: string) => operation({ operation: 'update', id: 'edit', primary_db }, {});
  const a = make('A');
  const b = make('B');
  const items = [a, a];
  const composed = batch(items);
  items.pop();
  expect(captureOperations(composed)).toHaveLength(2);
  expect(() => batch([a, b])).toThrow('one database namespace');
  expect(() => captureOperations([{} as any])).toThrow('Expected an operation');
});
