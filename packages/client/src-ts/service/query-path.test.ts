// @ts-nocheck
import { expect, test } from 'bun:test';
import { applyQueryDelta, parsePath } from './query-delta';
import { QueryClientService } from './query-client';

function applyThroughClient(base, ops) {
  let receive;
  let result;
  const errors = [];
  const service = new QueryClientService((error) => errors.push(error));
  service.attachPorts({ ports: {
    queryClientOut: { subscribe(callback) { receive = callback; } },
    receiveQueryClientMessage: { send() {} },
  } });
  service.registerQuery({ queryId: 'q', querySource: {}, input: {} }, (update) => { result = update.result; });
  receive({ type: 'full', queryId: 'q', revision: 1, result: base });
  receive({ type: 'delta', queryId: 'q', revision: 2, delta: { ops } });
  return { result, errors };
}

for (const apply of [(base, ops) => applyQueryDelta('q', base, { ops }), applyThroughClient]) {
  test(`${apply.name}: typed selectors never conflate numeric and string identities`, () => {
    const base = { rows: [{ id: '1', value: 'string' }, { id: 1, value: 'number' }] };
    const number = apply(base, [{ op: 'set-row', path: '.rows#(1)', row: { id: 1, value: 'changed' } }]);
    expect(number.errors).toEqual([]);
    expect(number.result.rows).toEqual([base.rows[0], { id: 1, value: 'changed' }]);
    const string = apply(base, [{ op: 'set-row', path: '.rows#("1")', row: { id: '1', value: 'changed' } }]);
    expect(string.errors).toEqual([]);
    expect(string.result.rows).toEqual([{ id: '1', value: 'changed' }, base.rows[1]]);
    expect(apply({ rows: [{ id: '1' }] }, [{ op: 'set-row', path: '.rows#(1)', row: {} }]).errors).toHaveLength(1);
  });

  test(`${apply.name}: UUID and escaped string selectors`, () => {
    for (const id of ['12345678-1234-1234-1234-123456789abc', 'a.b)\\c']) {
      const result = apply({ rows: [{ id }] }, [{ op: 'set-row', path: `.rows#(${JSON.stringify(id)})`, row: { id, value: 1 } }]);
      expect(result.errors).toEqual([]);
      expect(result.result).toEqual({ rows: [{ id, value: 1 }] });
    }
  });

  test(`${apply.name}: aliases and nested relationship projections need no identity`, () => {
    const base = { aliasedIssues: [{ title: 'before', children: [{ caption: 'child' }] }] };
    const result = apply(base, [{ op: 'set-row', path: '.aliasedIssues[0].children[0]', row: { caption: 'after' } }]);
    expect(result.errors).toEqual([]);
    expect(result.result).toEqual({ aliasedIssues: [{ title: 'before', children: [{ caption: 'after' }] }] });
    expect(base.aliasedIssues[0].children[0]).toEqual({ caption: 'child' });
  });
}

test('malformed and unsafe selectors reject', () => {
  for (const path of ['.', '.rows.', '.rows#()', '.rows#(9007199254740992)', '.rows[-1]', '.rows#("unterminated)', '.rows#("bad\\q")']) {
    expect(parsePath(path).ok).toBe(false);
  }
});
