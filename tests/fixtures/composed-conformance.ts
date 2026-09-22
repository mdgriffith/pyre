// Runs the actual compiler output through server execution and the browser bridge.
import assert from 'node:assert/strict';
import { realpathSync } from 'node:fs';
import { dirname } from 'node:path';
import { createClient } from '@libsql/client';
import init from '../packages/server/wasm/pyre_wasm.js';
import { databases, queries } from './typescript/server';
import { Note, Document, database, noteId } from './typescript/core/edits';
import { executeOperations } from '@pyre/server/operations';
import { runWithSync } from '@pyre/server/query-sync';
import { loadSchemaFromDatabase } from '@pyre/server/schema';
import { catchup } from '@pyre/server/sync';

await init({ module_or_path: await Bun.file('../packages/server/wasm/pyre_wasm_bg.wasm').arrayBuffer() });
const stores = new Map();
for (const instance of ['first', 'second']) {
  const db = createClient({ url: `file:${process.cwd()}/${instance}.db` });
  await databases._default.ensureDatabase(db);
  await loadSchemaFromDatabase(instance, db);
  stores.set(instance, db);
}
const db = stores.get('first');
const actor = { write: true };
// Explicit-session seed and simple request/response execution need no browser.
const denied = await executeOperations(db, queries, database('_default', 'first'), [Note.create({ id: 'ordinary', title: 'Denied' })], { write: false }, { mode: 'normal' });
assert.equal(denied.ok, false);
const seeded = await executeOperations(db, queries, database('_default', 'first'), [Note.create({ id: 'ordinary', title: 'Seed' })], actor, { mode: 'normal' });
assert.equal(seeded.ok, true);
const id = seeded.value[0].result.note[0].noteKey;
assert.match(id, /^[0-9a-f-]{14}7[0-9a-f-]{3}-[89ab]/);
const rejected = await executeOperations(db, queries, database('_default', 'first'), [Note.update(id, { title: 'Rollback' }), Note.delete(noteId('00000000-0000-7000-8000-000000000999'))], actor, { mode: 'normal' });
assert.equal(rejected.ok, false);
const messages: any[] = [];
const updated = await executeOperations(db, queries, database('_default', 'first'), [Note.update(id, { title: 'Seed committed' })], actor, { mode: 'sync', sessions: new Map([['reader', { session: actor }]]), publish: (_, value) => messages.push(value) });
assert.equal(updated.ok, true);
assert.equal(messages[0].type, 'delta');
// Nullable omission/null/value and whole JSON replacement through real generated SQL.
const doc = await executeOperations(db, queries, database('_default', 'first'), [Document.create({ title: 'Doc', owner: 'Owner', summary: null, tags: ['first'] })], actor, { mode: 'normal' });
assert.equal(doc.ok, true);
const documentId = doc.value[0].result.document[0].id;
for (const [patch, summary, tags] of [[{ summary: 'Set' }, 'Set', ['first']], [{ tags: ['replacement'] }, 'Set', ['replacement']], [{ summary: null }, null, ['replacement']]] as const) {
  const result = await executeOperations(db, queries, database('_default', 'first'), [Document.update(documentId, patch)], actor, { mode: 'normal' });
  assert.equal(result.ok, true);
  assert.equal(result.value[0].result.document[0].summary, summary);
  assert.deepEqual(result.value[0].result.document[0].tags, tags);
}

const bundle = await Bun.build({ entrypoints: ['./composed-browser.ts'], target: 'browser' });
assert.equal(bundle.success, true, String(bundle.logs));
const script = await bundle.outputs[0].text();
const streams = new Map();
const server = Bun.serve({ port: 0, idleTimeout: 0, async fetch(request) {
  const url = new URL(request.url);
  if (url.pathname === '/') return new Response('<script type="module" src="/bundle.js"></script>', { headers: { 'content-type': 'text/html' } });
  if (url.pathname === '/bundle.js') return new Response(script, { headers: { 'content-type': 'text/javascript' } });
  const instance = url.searchParams.get('databaseId') ?? request.headers.get('X-Pyre-Database-Id');
  const input = request.method === 'POST' ? await request.json() : null;
  const name = instance ?? input?.databaseId ?? 'first';
  const db = stores.get(name);
  if (url.pathname.endsWith('/sync')) return Response.json(await catchup(db, input.syncCursor, actor, 100, name, input.databaseEpoch));
  if (url.pathname.endsWith('/events')) return new Response(new ReadableStream({ start(controller) {
    streams.set(name, controller);
    void db.execute('select database_epoch from _pyre_sync').then(result => controller.enqueue(new TextEncoder().encode(`data: ${JSON.stringify({ type: 'connected', connectionId: name, databaseId: name, databaseEpoch: result.rows[0].database_epoch })}\n\n`)));
  } }), { headers: { 'content-type': 'text/event-stream' } });
  // Hold transport long enough for observable optimism before authority returns.
  await Bun.sleep(50);
  const result = await runWithSync(db, queries, input, undefined, actor, new Map(), name);
  await result.sync(() => {});
  return Response.json(result.kind === 'success' ? result.response : result.error, { status: result.kind === 'success' ? 200 : 400 });
} });
const { chromium } = await import(`${dirname(realpathSync(Bun.which('playwright')!))}/index.mjs`);
const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage();
  page.on('pageerror', console.error);
  await page.goto(server.url.toString());
  await page.waitForFunction('window.example && example.rows.first?.length === 1 && example.rows.second?.length === 0');
  await page.evaluate('example.observe()');
  await page.waitForFunction('example.counts.at(-1)?.first === 1 && example.counts.at(-1)?.second === 0');
  await page.evaluate("example.elmCreate('second')");
  await page.waitForFunction("example.completions.length === 1");
  assert.deepEqual(await page.evaluate('example.completions[0]'), { databaseId: 'second', ok: true, count: 1 });
  // Compiler-managed updatedAt makes create server-only. Updates predict existing rows.
  await page.evaluate("example.elmUpdate('second')");
  await page.waitForFunction('example.completions.length === 2');
  assert.equal(await page.evaluate('example.completions[1].ok'), true);
  assert(await page.evaluate("example.events.some(e => e.instance === 'second' && e.source === 'optimistic' && e.changes.some(c => c.row.title === 'Elm optimistic update'))"));
  assert.equal(await page.evaluate('example.rows.first.length'), 1);
  const result = await page.evaluate("example.tsCreate('first')");
  assert.equal(result.ok, true);
  assert.equal(await page.evaluate('example.rows.first.length'), 2);
  assert.equal(await page.evaluate('example.rows.second.length'), 1);
  await page.waitForFunction('example.counts.at(-1)?.first === 2 && example.counts.at(-1)?.second === 1');
  assert.equal((await page.evaluate("example.tsUpdate('first')")).ok, true);
  assert(await page.evaluate("example.events.some(e => e.instance === 'first' && e.source === 'optimistic' && e.changes.some(c => c.row.title === 'TS optimistic update'))"));
  const rollback = await page.evaluate(`example.rollback('first', ${JSON.stringify(id)})`);
  assert.equal(rollback.ok, false);
  assert(await page.evaluate("example.rows.first.some(row => row.title === 'TS optimistic update')"));
  console.log('PASS: generated TS/Elm bridge, custom-key optimism, typed receipts, two database instances, seed/normal/sync execution, atomic rejection, nullable and JSON replacement');
} finally {
  await browser.close(); server.stop(true); for (const db of stores.values()) db.close();
}
