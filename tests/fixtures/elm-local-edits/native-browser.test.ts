// @ts-nocheck
import { test, expect } from 'bun:test';
import { cpSync, mkdirSync, readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { resolve } from 'node:path';
import { createClient } from '@libsql/client';
import initWasm from '../../../packages/server/wasm/pyre_wasm.js';
import { loadSchemaFromDatabase } from '../../../packages/server/schema';
import { runBatchWithSync, catchupReplacement } from '../../../packages/server/query-sync';
import { planKey } from '@pyre/core/local-edits';

const directory = process.env.PYRE_ELM_EDIT_FIXTURE;
const playwright = process.env.PYRE_PLAYWRIGHT_MODULE;

// Optional like indexeddb.browser.test.ts. The Rust fixture generates/compiles
// the shared app first; all build and SQLite artifacts stay in its target dir.
test.skipIf(!directory || !playwright)('native Chromium: built PyreClient, generated TS/Elm edits, SQLite and IndexedDB', async () => {
  const client = `${directory}/browser-client`;
  mkdirSync(client, { recursive: true });
  for (const path of ['src', 'src-ts', 'scripts', 'elm.json']) cpSync(`packages/client/${path}`, `${client}/${path}`, { recursive: true });
  execFileSync('npx', ['--yes', '--package', 'elm@0.19.1-6', '-c', 'bash scripts/build.sh --optimize'], { cwd: client, stdio: 'pipe' });
  const build = await Bun.build({ entrypoints: [resolve('tests/fixtures/elm-local-edits/native-browser.ts')], target: 'browser', plugins: [{ name: 'fixture', setup(build) {
    build.onResolve({ filter: /^fixture:/ }, ({ path }) => ({ path: path === 'fixture:client' ? `${client}/src-ts/index.ts` : `${directory}/${path.slice(8)}` }));
  } }] });
  expect(build.success).toBe(true);
  const script = await build.outputs[0].text();
  const { manifest, databases } = await import(`${directory}/typescript/server.ts`);
  const { Main, Records } = await import(`${directory}/typescript/edits/Main.ts`);
  await initWasm({ module_or_path: readFileSync(process.env.PYRE_CONFORMANCE_WASM) });
  const db = createClient({ url: `file:${directory}/native-browser.db` });
  await databases._default.ensureDatabase(db);
  await loadSchemaFromDatabase('one', db);
  const fence = { databaseId: 'one', instance: 'browser-1', authGeneration: 1, namespace: Main.name, manifest: Main.manifest,
    databaseEpoch: (await db.execute('select database_epoch from _pyre_sync')).rows[0].database_epoch };
  const requests = [], errors = [], streams = new Set();
  const encoder = new TextEncoder();
  const server = Bun.serve({ hostname: '127.0.0.1', port: 0, async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === '/client.js') return new Response(script, { headers: { 'Content-Type': 'text/javascript' } });
    if (path === '/elm.js') return new Response(readFileSync(`${directory}/test.js`), { headers: { 'Content-Type': 'text/javascript' } });
    if (path === '/events') {
      let stream;
      return new Response(new ReadableStream({ start(controller) { stream = controller; streams.add(stream); controller.enqueue(encoder.encode(': ready\n\n')); }, cancel() { streams.delete(stream); } }), { headers: { 'Content-Type': 'text/event-stream' } });
    }
    if (path === '/auth') { fence.instance = 'browser-2'; fence.authGeneration = 2; return Response.json(fence); }
    if (path === '/hint') {
      const { definition, input } = Records.Issue.create({ id: '00000000-0000-4000-8000-000000000099', title: 'external', owner: 'me' })[planKey].operations[0];
      const result = await runBatchWithSync(db, manifest, fence, { ...fence, version: 1, requestId: 'external', sequence: 100, operations: [{ operation: definition.id, input }] }, {});
      if (result.kind !== 'success') throw new Error(JSON.stringify(result));
      const revision = Number((await db.execute('select server_revision from _pyre_sync')).rows[0].server_revision);
      for (const stream of streams) { try { stream.enqueue(encoder.encode(`data: ${JSON.stringify({ ...fence, type: 'syncRequired', reconciliation: result.response.reconciliation })}\n\n`)); } catch { streams.delete(stream); } }
      return Response.json({ revision });
    }
    if (path === '/batch' || path === '/replacement') {
      const body = await request.json();
      requests.push({ path, body });
      const result = path === '/batch' ? await runBatchWithSync(db, manifest, fence, body, {}) : await catchupReplacement(db, manifest, fence, body, {});
      if (result.kind === 'success') return Response.json(result.response);
      if (path === '/batch' && result.kind === 'error') return Response.json({ ...fence, requestId: body.requestId, status: 'rejected', code: result.error.errorType, operationIndex: result.error.index });
      errors.push(result);
      return Response.json(result, { status: 500 });
    }
    return new Response('<!doctype html><title>Local edit integration</title><script src="/elm.js"></script>', { headers: { 'Content-Type': 'text/html' } });
  } });
  const { chromium } = await import(playwright);
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage();
    const pageErrors = [];
    page.on('pageerror', error => pageErrors.push(error.message));
    await page.goto(server.url.href);
    const result = await page.evaluate(async fence => (await import('/client.js')).verify(fence), fence);
    expect(result).toEqual(['read-only catchup', 'pending not persisted', 'TS/Elm reader agreement', 'fire-and-forget rollback', 'EventSource hint', 'auth/dispose fencing']);
    expect(pageErrors).toEqual([]);
    expect(errors).toEqual([]);
    expect(requests.filter(r => r.path === '/batch' && r.body.authGeneration === 2)).toHaveLength(0);
    expect((await db.execute('select id from issues')).rows).toEqual([]);
    console.log(`Native Chromium verified: ${result.join('; ')}`);
  } finally { await browser.close(); server.stop(true); db.close(); }
}, 60_000);
