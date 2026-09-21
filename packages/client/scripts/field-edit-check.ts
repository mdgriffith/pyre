import assert from 'node:assert/strict';
import { realpathSync } from 'node:fs';
import { dirname } from 'node:path';

// Use the installed Playwright rather than adding another browser dependency.
const { chromium } = await import(`${dirname(realpathSync(Bun.which('playwright')!))}/index.mjs`);
const url = process.argv[2].replace(/\/$/, '');
const browser = await chromium.launch({ headless: true });
try {
  const context = await browser.newContext();
  const a = await context.newPage();
  const b = await context.newPage();
  for (const [page, name] of [[a, 'a'], [b, 'b']] as const) {
    page.on('pageerror', console.error);
    await page.goto(`${url}/${name}`);
    try {
      await page.waitForFunction('window.proof?.rows?.length === 2 && proof.connected');
    } catch (error) {
      console.error('Initial state', name, await page.evaluate('window.proof && { rows: proof.rows, connected: proof.connected, batches: proof.batches }'));
      throw error;
    }
  }
  await a.waitForFunction("async () => (await (await fetch('/stats')).json()).connections.length === 2");
  const migration = await a.evaluate('proof.verifyIdentityMigration()');
  assert.deepEqual(migration.before, { rows: {}, revision: null, stamps: [], cursor: { tables: {} } });
  assert.deepEqual(migration.rows, [{ id: '00000000-0000-7000-8000-000000000001', title: 'Current' }]);
  assert.deepEqual(migration.stamps, [['notes', '00000000-0000-7000-8000-000000000001', 2]]);
  assert.deepEqual(await a.evaluate('proof.verifyRemovalPersistence()'), { rows: [], stamps: [['notes', '00000000-0000-7000-8000-000000000003', 3]] });
  const initial = await (await a.request.get(`${url}/stats`)).json();
  await a.evaluate("proof.edit('normalized')");
  await a.waitForFunction("proof.rows[0].title === 'NORMALIZED'");
  await b.waitForFunction("proof.rows[0].title === 'NORMALIZED'");
  assert(await a.evaluate("proof.batches.some(b => b.source === 'optimistic' && b.changes.some(c => c.row.title === 'normalized'))"));
  assert(await a.evaluate("proof.batches.some(b => b.source === 'mutation-response' && b.changes.some(c => c.row.title === 'NORMALIZED'))"));
  assert(await b.evaluate("proof.batches.some(b => b.source === 'live' && b.changes.some(c => c.row.title === 'NORMALIZED'))"));
  const stats = await (await a.request.get(`${url}/stats`)).json();
  assert.equal(stats.catchup, initial.catchup);
  assert.equal(stats.delta, initial.delta + 1);
  assert.deepEqual(await a.evaluate('proof.rows'), await b.evaluate('proof.rows'));
  await a.waitForFunction("async () => (await proof.persisted())[0].title === 'NORMALIZED'");

  await a.evaluate("proof.edit('reject')");
  await a.waitForFunction("proof.results.length === 2 && proof.rows[0].title === 'NORMALIZED'");
  assert(await a.evaluate("proof.results.at(-1).ok === false"));
  assert(await a.evaluate("proof.batches.at(-1).changes[0].row.title === 'NORMALIZED'"));

  const beforeBatch = await (await a.request.get(`${url}/stats`)).json();
  const optimisticBefore = await a.evaluate("proof.batches.filter(b => b.source === 'optimistic').length");
  const batch = await a.evaluate("proof.batch([{ id: '00000000-0000-7000-8000-000000000001', title: 'intermediate' }, { id: '00000000-0000-7000-8000-000000000002', title: 'second' }, { id: '00000000-0000-7000-8000-000000000001', title: 'final' }])");
  assert.equal(batch.ok, true);
  assert.deepEqual(batch.value.map((entry: any) => entry.index), [0, 1, 2]);
  await b.waitForFunction("proof.rows[0].title === 'FINAL' && proof.rows[1].title === 'SECOND'");
  assert.deepEqual(await a.evaluate('proof.rows'), await b.evaluate('proof.rows'));
  assert.equal(await a.evaluate("proof.batches.filter(b => b.source === 'optimistic').length"), optimisticBefore + 1);
  const afterBatch = await (await a.request.get(`${url}/stats`)).json();
  assert.equal(afterBatch.mutations, beforeBatch.mutations + 1);
  assert.equal(afterBatch.delta, beforeBatch.delta + 1);
  assert.equal(afterBatch.catchup, beforeBatch.catchup);
  const rejected = await a.evaluate("proof.batch([{ id: '00000000-0000-7000-8000-000000000001', title: 'rollback' }, { id: '00000000-0000-7000-8000-000000000999', title: 'missing' }])");
  assert.equal(rejected.ok, false);
  assert.deepEqual(await a.evaluate('proof.rows'), await b.evaluate('proof.rows'));
  await a.waitForFunction("async () => (await proof.persisted())[0].title === 'FINAL'");

  // Keep the committed HTTP response held across a permission-loss reset.
  await b.evaluate("proof.edit('hold')");
  await b.waitForFunction("proof.rows[0].title === 'hold'");
  await a.waitForFunction("proof.rows[0].title === 'HOLD'");
  assert.equal(await b.evaluate("(async () => (await proof.late())[0].changes[0].row.title)()"), 'hold');
  await a.evaluate("proof.edit('hidden')");
  await a.waitForFunction("proof.rows[0].title === 'HIDDEN'");
  await b.waitForFunction("proof.rows.length === 1 && proof.rows[0].id === '00000000-0000-7000-8000-000000000002'");
  await b.waitForFunction("async () => (await proof.persisted()).length === 1");
  assert(await b.evaluate("proof.batches.some(b => b.changes.some(c => c.id === '00000000-0000-7000-8000-000000000001' && c.op === 'remove'))"));
  await b.request.get(`${url}/release`);
  await b.waitForFunction('proof.results.length === 1');
  assert.equal(await b.evaluate('proof.results[0].ok'), false);
  assert.equal(await b.evaluate('proof.rows[0].id'), '00000000-0000-7000-8000-000000000002');
  assert.deepEqual(await b.evaluate('(async () => (await proof.persisted()).map(r => r.id))()'), ['00000000-0000-7000-8000-000000000002']);

  await b.reload();
  await b.waitForFunction('window.proof?.connected && proof.rows?.length === 1');
  assert.equal(await b.evaluate('proof.rows[0].id'), '00000000-0000-7000-8000-000000000002');
  assert.deepEqual(await b.evaluate('(async () => (await proof.late())[0].changes.map(c => c.id))()'), ['00000000-0000-7000-8000-000000000002']);
  console.log('PASS: two native clients, atomic repeated/multi-row submission and rollback, incremental HTTP/SSE, normalization, rejection, late readers, permission removal, held-response IndexedDB safety and reload');
} finally { await browser.close(); }
