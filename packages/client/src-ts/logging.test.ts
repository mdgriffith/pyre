// @ts-nocheck
import { expect, test } from 'bun:test';
import { describeDebugCall, describePyreEvent, withDatabase } from './logging';

test('formats live deltas as self-contained summaries', () => {
  expect(describePyreEvent('sync:delta', {
    databaseId: 'campaign:9f246734-c2fe-40d0-a03a-f299d90d866f',
    serverRevision: 43,
    data: [
      { table_name: 'assets', rows: [[1], [2]] },
    ],
  })).toMatchObject({
    actor: 'APP',
    direction: '<-',
    operation: 'live-delta',
    summary: '[pyre] APP <- live-delta campaign:9f246734 rev=43 tables=1 rows=2',
  });
});

test('formats IndexedDB work with useful scalar measurements', () => {
  expect(describeDebugCall([
    '[PyreClient] IndexedDB writeDelta table written',
    { tableName: 'assets', received: 312, written: 310, skippedOlder: 2 },
  ], 'campaign:9f246734-c2fe-40d0-a03a-f299d90d866f')).toMatchObject({
    actor: 'IDB',
    operation: 'delta.persisted',
    summary: '[pyre] IDB delta.persisted campaign:9f246734 table=assets received=312 written=310 skipped=2',
  });
});

test('flattens an internal event when attaching its database', () => {
  const event = describePyreEvent('elm:catchup-update', {
    status: 'syncing',
    touchedTables: ['assets', 'assetLayers'],
    dbCmdCount: 3,
  });

  expect(withDatabase(event, 'campaign:9f246734-c2fe-40d0-a03a-f299d90d866f')).toMatchObject({
    operation: 'catchup.updated',
    summary: '[pyre] APP catchup.updated campaign:9f246734 tables=2 commands=3 status=syncing',
    payload: {
      databaseId: 'campaign:9f246734-c2fe-40d0-a03a-f299d90d866f',
      status: 'syncing',
      touchedTables: ['assets', 'assetLayers'],
      dbCmdCount: 3,
    },
  });
});
