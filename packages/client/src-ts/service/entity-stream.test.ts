// @ts-nocheck
import { expect, test } from 'bun:test';

import { EntityStreamService, validateEntitySubscription } from './entity-stream';

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
      ['a', 1, 'Nice'],
      ['b', 3, 'Hidden'],
    ],
  },
];

test('engine-visible projection emits removals on filter exit and reset without retaining secret rows', () => {
  const service = new EntityStreamService();
  service.handleVisibleState(delta, 'catchup');
  const batches: any[] = [];
  service.subscribe({ tables: [{ tableName: 'posts', where: { published: true } }] }, (batch) => batches.push(batch));
  service.handleVisibleState([{ ...delta[0], rows: [[1, 10, 'Private now', false], [2, 20, 'Draft', false]] }], 'optimistic');
  expect(batches[0].changes).toEqual([{ tableName: 'posts', id: 1, op: 'remove', row: { id: 1 } }]);
  service.handleVisibleState(delta, 'mutation-response');
  expect(batches[1].changes[0].row.title).toBe('Hello');
  service.handleVisibleState([], 'catchup');
  expect(batches[2].changes[0].op).toBe('remove');
  expect(service.snapshot().size).toBe(0);
});

test('entity stream emits matching table rows as batches', () => {
  const service = new EntityStreamService();
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
  const service = new EntityStreamService();
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
    { tableName: 'comments', id: 'a', op: 'row', row: { id: 'a', post_id: 1, body: 'Nice' } },
  ]);
});

test('entity stream emits one row when duplicate table subscriptions match the same id', () => {
  const service = new EntityStreamService();
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
  const service = new EntityStreamService();
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
  const service = new EntityStreamService();
  const batches: unknown[] = [];

  service.subscribe({ tables: [{ tableName: 'posts' }] }, (batch) => {
    batches.push(batch);
  });

  service.handleTableDelta(delta, 'optimistic', 'main');
  service.handleTableDelta(delta, 'mutation-response', 'main');

  expect(batches.map((batch) => batch.source)).toEqual(['optimistic', 'mutation-response']);
});

test('entity stream supports negative filters and unsubscribe', () => {
  const service = new EntityStreamService();
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
  const service = new EntityStreamService();
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

test('entity stream does not emit rows without usable ids', () => {
  const service = new EntityStreamService();
  const batches: unknown[] = [];

  service.subscribe({ tables: [{ tableName: 'events' }] }, (batch) => {
    batches.push(batch);
  });

  service.handleTableDelta([
    {
      table_name: 'events',
      headers: ['name'],
      rows: [['No ID']],
    },
  ], 'live');

  expect(batches).toHaveLength(0);
});
