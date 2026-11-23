// Runs the actual compiler output through server execution and the browser bridge.
import assert from 'node:assert/strict';
import { realpathSync } from 'node:fs';
import { dirname } from 'node:path';
import { createClient } from '@libsql/client';
import init from '../packages/server/wasm/pyre_wasm.js';
import { databases, queries } from './typescript/server';
import { Note, Reply, Document, database, noteId } from './typescript/core/edits';
import { captureOperations } from '@pyre/client/operations';
import { executeOperations } from '@pyre/server/operations';
import { runWithSync } from '@pyre/server/query-sync';
import { loadSchemaFromDatabase } from '@pyre/server/schema';
import { catchup } from '@pyre/server/sync';
import { Elm as Checks } from './acquire-checks.js';

const acquisitionChecks: Record<string, boolean> = await new Promise(resolve => Checks.AcquireChecks.init({ flags: null }).ports.output.subscribe(resolve));
for (const [check, passed] of Object.entries(acquisitionChecks)) assert(passed, check);

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
const denied = await executeOperations(db, queries, database('_default', 'first'), [Note.create(Note.newId(), { id: 'ordinary', title: 'Denied' })], { write: false }, { mode: 'normal' });
assert.equal(denied.ok, false);
const seedId = Note.newId();
const seeded = await executeOperations(db, queries, database('_default', 'first'), [Note.create(seedId, { id: 'ordinary', title: 'Seed' })], actor, { mode: 'normal' });
assert.equal(seeded.ok, true);
const id = seeded.value[0].result.note[0].noteKey;
assert.equal(id, seedId);
assert.match(id, /^[0-9a-f-]{14}7[0-9a-f-]{3}-[89ab]/);
const rejected = await executeOperations(db, queries, database('_default', 'first'), [Note.update(id, { title: 'Rollback' }), Note.delete(noteId('00000000-0000-7000-8000-000000000999'))], actor, { mode: 'normal' });
assert.equal(rejected.ok, false);
const messages: any[] = [];
const updated = await executeOperations(db, queries, database('_default', 'first'), [Note.update(id, { title: 'Seed committed' })], actor, { mode: 'sync', sessions: new Map([['reader', { session: actor }]]), publish: (_, value) => messages.push(value) });
assert.equal(updated.ok, true);
assert.equal(messages[0].type, 'delta');
// Nullable omission/null/value and whole JSON replacement through real generated SQL.
const doc = await executeOperations(db, queries, database('_default', 'first'), [Document.create(Document.newId(), { title: 'Doc', owner: 'Owner', summary: null, tags: ['first'] })], actor, { mode: 'normal' });
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
const submissions: any[] = [];
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
  submissions.push(input);
  await Bun.sleep(50);
  const result = await runWithSync(db, queries, input, undefined, actor, new Map(), name);
  await result.sync(() => {});
  if (input[0]?.input.title === 'Uncertain') {
    assert.equal(result.kind, 'success');
    return Response.json({ errorType: 'OutcomeUnknown' }, { status: 400 });
  }
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
  const { id: created, result } = await page.evaluate("example.tsCreate('first')");
  assert.equal(result.ok, true);
  assert.equal(result.value[0].result.note[0].noteKey, created);
  assert.equal(await page.evaluate('example.rows.first.length'), 2);
  assert.equal(await page.evaluate('example.rows.second.length'), 1);
  await page.waitForFunction('example.counts.at(-1)?.first === 2 && example.counts.at(-1)?.second === 1');
  assert.equal((await page.evaluate("example.tsUpdate('first')")).ok, true);
  assert(await page.evaluate("example.events.some(e => e.instance === 'first' && e.source === 'optimistic' && e.changes.some(c => c.row.title === 'TS optimistic update'))"));
  const rollback = await page.evaluate(`example.rollback('first', ${JSON.stringify(id)})`);
  assert.equal(rollback.ok, false);
  assert(await page.evaluate("example.rows.first.some(row => row.title === 'TS optimistic update')"));

  const ts = await page.evaluate("example.tsRelated('first', 'TS related')");
  assert.equal(ts.result.ok, true);
  assert.equal(ts.result.value[0].result.note[0].noteKey, ts.note);
  assert.equal(ts.result.value[1].result.reply[0].replyKey, ts.reply);
  assert.equal(ts.result.value[1].result.reply[0].noteKey, ts.note);
  assert(await page.evaluate(`example.replies.first.some(row => row.replyKey === '${ts.reply}' && row.noteKey === '${ts.note}')`));
  const duplicate = captureOperations([Reply.create(ts.reply, { noteKey: ts.note, title: 'TS related' })])
    .map(({ queryId, input }) => ({ queryId, input }));
  const deniedDuplicate = await runWithSync(db, queries, duplicate, undefined, { write: false }, new Map(), 'first');
  const allowedDuplicate = await runWithSync(db, queries, duplicate, undefined, actor, new Map(), 'first');
  assert.equal(deniedDuplicate.kind, 'error');
  assert.equal(deniedDuplicate.error.operationIndex, 0, 'permission filters out the insert before constraints run');
  assert.equal(allowedDuplicate.kind, 'error');
  assert.equal(allowedDuplicate.error.operationIndex, undefined, 'authorized insert reaches the SQLite constraint');

  await page.evaluate('example.attachAcquisition()');
  for (const [index, explicit] of [true, false, true, false].entries()) {
    const title = ['Elm explicit', 'Elm auto', 'Elm explicit', 'Forbidden'][index];
    const before = submissions.length;
    const eventsBefore = await page.evaluate('example.events.length');
    await page.evaluate(`example.acquire(${explicit})`);
    await page.waitForFunction(`example.acquired.length === ${index + 1}`);
    const ids = await page.evaluate('example.acquired.at(-1)');
    assert.deepEqual(await page.evaluate('example.outgoingAcquisition.at(-1)'), { type: 'acquire-ids', requestId: `pyre:${index}`, count: explicit ? 2 : 1 });
    assert.equal(submissions.length, before, 'acquisition does not send a mutation');
    assert.equal(await page.evaluate('example.events.length'), eventsBefore, 'acquisition does not publish optimistic state');
    assert.equal(await page.evaluate(`example.rows.second.some(row => row.noteKey === '${ids.note}')`), false);
    await page.evaluate(`example.submitRelated('second', ${JSON.stringify(title)})`);
    await page.waitForFunction(`example.related.length === ${index + 1}`);
    const receipt = await page.evaluate('example.related.at(-1)');
    assert.equal(submissions.length, before + 1);
    const [parent, child] = submissions.at(-1);
    assert.equal(parent.input.noteKey, ids.note);
    assert.equal(child.input.noteKey, ids.note);
    if (explicit) assert.equal(child.input.replyKey, ids.reply);
    else assert.match(child.input.replyKey, /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    if (index < 2) {
      assert.deepEqual(receipt, { notes: [ids.note], replies: [child.input.replyKey] });
      assert(await page.evaluate(`example.replies.second.some(row => row.replyKey === '${child.input.replyKey}' && row.noteKey === '${ids.note}')`));
    } else {
      assert.equal(typeof receipt.error, 'string', 'child constraint or permission rejects the batch');
      assert.equal(await page.evaluate(`example.rows.second.some(row => row.noteKey === '${ids.note}')`), false);
      assert.equal((await stores.get('second').execute({ sql: 'select count(*) as count from notes where noteKey = ?', args: [ids.note] })).rows[0].count, 0);
    }
  }
  const beforeUnknown = submissions.length;
  const unknown = await page.evaluate("example.tsRelated('first', 'Uncertain')");
  assert.equal(unknown.result.ok, false);
  await Bun.sleep(150);
  assert.equal(submissions.length, beforeUnknown + 1, 'unknown outcome is not replayed');
  assert.equal((await db.execute({ sql: 'select count(*) as count from notes where noteKey = ?', args: [unknown.note] })).rows[0].count, 1);
  console.log('PASS: generated TS/Elm create IDs, optional local acquisition, ordered related writes, authoritative reconciliation, rollback, and unknown outcome');
} finally {
  await browser.close(); server.stop(true); for (const db of stores.values()) db.close();
}
