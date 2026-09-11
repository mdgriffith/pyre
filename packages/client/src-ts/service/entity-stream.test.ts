// @ts-nocheck
import { expect, test } from 'bun:test';

import { EntityStreamService, validateEntitySubscription } from './entity-stream';

const uuidA = '00000000-0000-0000-0000-000000000001';
const uuidB = '00000000-0000-0000-0000-000000000002';
const schema = {
  tables: {
    posts: { name: 'posts', primaryKey: { name: 'id', kind: 'int' }, links: {}, indices: [] },
    comments: { name: 'comments', primaryKey: { name: 'id', kind: 'uuid' }, links: {}, indices: [] },
    events: { name: 'events', primaryKey: { name: 'eventKey', kind: 'uuid' }, links: {}, indices: [] },
    audits: { name: 'audits', primaryKey: { name: 'sequence', kind: 'int' }, links: {}, indices: [] },
  },
  queryFieldToTable: {},
};

const delta = [
  {
    table_name: 'posts',
    headers: ['id', 'author_id', 'title', 'published'],
    rows: [
      [1, 10, 'Hello', true],
      [2, 20, 'Draft', false],
    ],
  },
  {
    table_name: 'comments',
    headers: ['id', 'post_id', 'body'],
    rows: [
      [uuidA, 1, 'Nice'],
      [uuidB, 3, 'Hidden'],
    ],
  },
];

test('entity stream emits matching table rows as batches', () => {
  const service = new EntityStreamService(schema);
  const batches: unknown[] = [];

  service.subscribe({ tables: [{ tableName: 'posts' }] }, (batch) => {
    batches.push(batch);
  });

  service.handleTableDelta(delta, 'live', 'main');

  expect(batches).toEqual([
    {
      type: 'entity-change-batch',
      databaseId: 'main',
      sequence: 1,
      source: 'live',
      changes: [
        { tableName: 'posts', id: 1, op: 'row', row: { id: 1, author_id: 10, title: 'Hello', published: true } },
        { tableName: 'posts', id: 2, op: 'row', row: { id: 2, author_id: 20, title: 'Draft', published: false } },
      ],
    },
  ]);
});

test('entity stream applies equality and membership filters locally', () => {
  const service = new EntityStreamService(schema);
  const batches: unknown[] = [];

  service.subscribe(
    {
      tables: [
        { tableName: 'posts', where: { author_id: 10, published: { $eq: true } } },
        { tableName: 'comments', where: { post_id: { $in: [1, 2] } } },
      ],
    },
    (batch) => {
      batches.push(batch);
    }
  );

  service.handleTableDelta(delta, 'catchup');

  expect(batches[0].changes).toEqual([
    { tableName: 'posts', id: 1, op: 'row', row: { id: 1, author_id: 10, title: 'Hello', published: true } },
    { tableName: 'comments', id: uuidA, op: 'row', row: { id: uuidA, post_id: 1, body: 'Nice' } },
  ]);
});

test('entity stream emits one row when duplicate table subscriptions match the same id', () => {
  const service = new EntityStreamService(schema);
  const batches: unknown[] = [];

  service.subscribe(
    {
      tables: [
        { tableName: 'posts', where: { author_id: 10 } },
        { tableName: 'posts', where: { published: true } },
      ],
    },
    (batch) => {
      batches.push(batch);
    }
  );

  service.handleTableDelta(delta, 'live', 'main');

  expect(batches[0].changes).toEqual([
    { tableName: 'posts', id: 1, op: 'row', row: { id: 1, author_id: 10, title: 'Hello', published: true } },
  ]);
});

test('entity stream preserves reserved initial sequence before live batches', () => {
  const service = new EntityStreamService(schema);
  const batches: unknown[] = [];
  const subscription = { tables: [{ tableName: 'posts' }] };
  const initialSequence = service.reserveSequence();

  service.subscribe(subscription, (batch) => {
    batches.push(batch);
  });

  const initialBatch = service.createBatchFromRows(
    subscription,
    new Map([['posts', [{ id: 9, author_id: 10, title: 'Cached' }]]]),
    'indexeddb-initial',
    'main',
    initialSequence
  );
  service.handleTableDelta(delta, 'live', 'main');

  expect(initialBatch?.sequence).toBe(1);
  expect(batches[0].sequence).toBe(2);
  expect(batches[0].source).toBe('live');
});

test('entity stream emits optimistic and mutation response sources', () => {
  const service = new EntityStreamService(schema);
  const batches: unknown[] = [];

  service.subscribe({ tables: [{ tableName: 'posts' }] }, (batch) => {
    batches.push(batch);
  });

  service.handleTableDelta(delta, 'optimistic', 'main');
  service.handleTableDelta(delta, 'mutation-response', 'main');

  expect(batches.map((batch) => batch.source)).toEqual(['optimistic', 'mutation-response']);
});

test('entity stream supports negative filters and unsubscribe', () => {
  const service = new EntityStreamService(schema);
  const batches: unknown[] = [];

  const unsubscribe = service.subscribe(
    { tables: [{ tableName: 'posts', where: { author_id: { $ne: 10 }, id: { $nin: [3] } } }] },
    (batch) => {
      batches.push(batch);
    }
  );

  service.handleTableDelta(delta, 'live');
  unsubscribe();
  service.handleTableDelta(delta, 'live');

  expect(batches).toHaveLength(1);
  expect(batches[0].changes).toEqual([
    { tableName: 'posts', id: 2, op: 'row', row: { id: 2, author_id: 20, title: 'Draft', published: false } },
  ]);
});

test('entity stream builds initial batches from persisted rows', () => {
  const service = new EntityStreamService(schema);
  const rowsByTable = new Map([
    ['posts', [
      { id: 1, author_id: 10, title: 'Hello' },
      { id: 2, author_id: 20, title: 'Draft' },
    ]],
  ]);

  const batch = service.createBatchFromRows(
    { tables: [{ tableName: 'posts', where: { author_id: 10 } }] },
    rowsByTable,
    'indexeddb-initial',
    'main'
  );

  expect(batch).toEqual({
    type: 'entity-change-batch',
    databaseId: 'main',
    sequence: 1,
    source: 'indexeddb-initial',
    changes: [
      { tableName: 'posts', id: 1, op: 'row', row: { id: 1, author_id: 10, title: 'Hello' } },
    ],
  });
});

test('entity stream validates subscriptions', () => {
  expect(() => validateEntitySubscription({ tables: [] })).toThrow('at least one table');
  expect(() => validateEntitySubscription({ tables: [{ tableName: '' }] })).toThrow('non-empty tableName');
  expect(() => validateEntitySubscription({ tables: [{ tableName: 'posts', where: [] }] })).toThrow('where must be an object');
  expect(() => validateEntitySubscription({ tables: [{ tableName: 'posts', where: { id: { $in: 1 } } }] })).toThrow('$in must be an array');
  expect(() => validateEntitySubscription({ tables: [{ tableName: 'posts', where: { id: { $gt: 1 } } }] })).toThrow('unsupported operator $gt');
  expect(() => validateEntitySubscription({ tables: [{ tableName: 'posts', where: { id: { $eq: 1, $ne: 2 } } }] })).toThrow('exactly one operator');
});

test('entity stream rejects rows without declared identities', () => {
  const service = new EntityStreamService(schema);
  const batches: unknown[] = [];

  service.subscribe({ tables: [{ tableName: 'events' }] }, (batch) => {
    batches.push(batch);
  });

  expect(() => service.handleTableDelta([
    {
      table_name: 'events',
      headers: ['name'],
      rows: [['No ID']],
    },
  ], 'live')).toThrow('Invalid uuid identity for events.eventKey');

  expect(batches).toHaveLength(0);
});

test('non-id PK streams preserve rows, relationships, table and database scopes', () => {
  const subscription = { tables: [{ tableName: 'events', where: { parent: uuidB } }, { tableName: 'comments' }, { tableName: 'audits' }] };
  const rows = new Map([
    ['events', [{ eventKey: uuidA, id: 'ordinary column', parent: uuidB }]],
    ['comments', [{ id: uuidA, post_id: 1 }]],
    ['audits', [{ sequence: 1, id: 'not the primary key' }]],
  ]);
  for (const databaseId of ['main', 'other']) {
    const service = new EntityStreamService(schema);
    const batch = service.createBatchFromRows(subscription, rows, 'indexeddb-initial', databaseId);
    expect(batch.databaseId).toBe(databaseId);
    expect(batch.changes.map(({ tableName, id }) => [tableName, id])).toEqual([
      ['events', uuidA], ['comments', uuidA], ['audits', 1],
    ]);
    expect(batch.changes[0].row).toEqual(rows.get('events')[0]);
  }
});

test('invalid identities fail before any stream callback, even behind a filter', () => {
  for (const [tableName, field, invalid] of [
    ['audits', 'sequence', [undefined, null, '1', 1.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1]],
    ['events', 'eventKey', [undefined, null, 1, '1', '', 'not-a-uuid']],
  ]) {
    for (const value of invalid) {
      const service = new EntityStreamService(schema);
      const batches = [];
      service.subscribe({ tables: [{ tableName: 'posts' }] }, (batch) => batches.push(batch));
      service.subscribe({ tables: [{ tableName, where: { visible: true } }] }, (batch) => batches.push(batch));
      expect(() => service.handleTableDelta([
        delta[0], { table_name: tableName, headers: [field, 'visible'], rows: [[value, false]] },
      ], 'live')).toThrow('identity');
      expect(batches).toEqual([]);
    }
  }
});

test('missing metadata and malformed groups are explicit errors', () => {
  const service = new EntityStreamService(schema);
  expect(() => service.subscribe({ tables: [{ tableName: 'unknown' }] }, () => {})).toThrow('metadata');
  const missing = new EntityStreamService({ tables: { posts: { name: 'posts' } }, queryFieldToTable: {} });
  expect(() => missing.subscribe({ tables: [{ tableName: 'posts' }] }, () => {})).toThrow('metadata');
  service.subscribe({ tables: [{ tableName: 'posts' }] }, () => {});
  expect(() => service.handleTableDelta([{ table_name: 'posts', headers: ['id'], rows: [[]] }], 'live')).toThrow('Invalid entity row');
});
