import { describe, expect, test } from 'bun:test';
import fixtures from '../../tests/fixtures/database-context.json';
import { parseContextMessage, parseContextRequest } from './index';

describe('shared database-context fixtures', () => {
  for (const { name, value } of fixtures.validRequests) {
    test(`valid request: ${name}`, () => {
      expect(parseContextRequest(value)).toEqual(value);
      expect(parseContextRequest(JSON.parse(JSON.stringify(value)))).toEqual(value);
    });
  }
  for (const { name, value } of fixtures.invalidRequests) {
    test(`invalid request: ${name}`, () => {
      expect(() => parseContextRequest(value)).toThrow(Error);
    });
  }
  for (const { name, value } of fixtures.validMessages) {
    test(`valid message: ${name}`, () => {
      const parsed = parseContextMessage(value);
      expect(parsed).toEqual(value);
      expect(parseContextMessage(JSON.parse(JSON.stringify(parsed)))).toEqual(value);
    });
  }
  for (const { name, value } of fixtures.invalidMessages) {
    test(`invalid message: ${name}`, () => {
      expect(() => parseContextMessage(value)).toThrow(Error);
    });
  }
  for (const [cases, parse] of [
    [fixtures.rawRequests, parseContextRequest],
    [fixtures.rawMessages, parseContextMessage],
  ] as const) {
    for (const { name, json, valid } of cases) {
      test(`raw JSON: ${name}`, () => {
        if (valid) {
          expect(parse(JSON.parse(json))).toEqual(JSON.parse(json));
        } else {
          expect(() => parse(JSON.parse(json))).toThrow(Error);
        }
      });
    }
  }
});

const context = {
  type: 'context', protocolVersion: 1, databaseId: ' db ', contextId: 'ctx',
  schemaId: 'schema', cacheScope: 'scope', authorityRevision: 'revision',
  databaseEpoch: 'epoch', session: {}, expiresAt: 0,
};

describe('JavaScript decode boundary', () => {
  for (const [name, value] of [
    ['undefined', undefined], ['NaN', NaN], ['infinity', Infinity],
    ['negative infinity', -Infinity], ['function', () => null],
    ['bigint', 1n], ['symbol', Symbol('value')], ['date', new Date(0)],
    ['map', new Map()], ['sparse array', new Array(1)],
    ['array with extra property', Object.assign([], { extra: true })],
    ['symbol property', { [Symbol('key')]: true }],
    ['hidden property', Object.defineProperty({}, 'hidden', { value: true })],
    ['accessor', { get value() { throw new Error('must not invoke getter'); } }],
  ] as const) {
    test(`rejects nested ${name}`, () => {
      expect(() => parseContextMessage({ ...context, session: { nested: [value] } })).toThrow(Error);
    });
  }

  test('rejects cycles but permits repeated references', () => {
    const cycle: Record<string, unknown> = {};
    cycle.self = cycle;
    expect(() => parseContextMessage({ ...context, session: cycle })).toThrow(Error);
    const shared = { value: true };
    const message = { ...context, session: { a: shared, b: shared } };
    expect(parseContextMessage(message)).toEqual(message);
  });

  test('preserves identifiers and accepts expired timestamps by shape', () => {
    expect(parseContextRequest({ protocolVersion: 1, databaseId: ' db ' }).databaseId).toBe(' db ');
    expect(parseContextMessage(context)).toEqual(context);
  });

  test('accepts integer JSON numbers in decimal and exponent notation', () => {
    expect(parseContextRequest(JSON.parse('{"protocolVersion":1.0,"databaseId":"db"}')).protocolVersion).toBe(1);
    for (const number of ['1', '1.0', '1e0']) {
      const value = JSON.parse(JSON.stringify({ ...context, expiresAt: 1 }).replace('"expiresAt":1', `"expiresAt":${number}`));
      expect(parseContextMessage(value)).toEqual({ ...context, expiresAt: 1 });
    }
  });

  test('rejects non-JSON envelopes and unsafe JS timestamps', () => {
    for (const value of [undefined, NaN, () => null, new Date(0)]) {
      expect(() => parseContextRequest(value)).toThrow(Error);
      expect(() => parseContextMessage(value)).toThrow(Error);
    }
    for (const expiresAt of [NaN, Infinity, -Infinity, undefined, Number.MAX_SAFE_INTEGER + 1]) {
      expect(() => parseContextMessage({ ...context, expiresAt })).toThrow(Error);
    }
    expect(() => parseContextRequest({ protocolVersion: 1, databaseId: 'db', extra: undefined })).toThrow(Error);
  });
});
