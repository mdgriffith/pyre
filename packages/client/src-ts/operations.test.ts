// @ts-nocheck -- Bun test globals follow the other client runtime tests.
import { expect, test } from 'bun:test';
import { batch, captureOperations, createId, database, decodeOperationResults, operation } from './operations';

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

test('submission checks namespace and decodes ordered compiled results', () => {
  const edit = operation({ operation: 'insert', id: 'create', primary_db: 'Main', ReturnData: {
    parse(value) { return { createdAt: new Date(value.createdAt) }; },
  } }, {});
  expect(() => captureOperations([edit], database('Other', 'tenant:1'))).toThrow('one database namespace');
  expect(captureOperations([edit], database('Main', 'tenant:1'))).toHaveLength(1);
  const result = decodeOperationResults([edit], [{ index: 0, queryId: 'create', result: { createdAt: '2026-01-01' } }]);
  expect(result[0].result.createdAt).toBeInstanceOf(Date);
  expect(() => decodeOperationResults([edit], [])).toThrow('result count');
  expect(() => decodeOperationResults([edit], [{ index: 0, queryId: 'wrong', result: {} }])).toThrow('result identity');
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
