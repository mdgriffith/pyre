// Real libsql + WASM permissions + HTTP/SSE + Chromium IndexedDB proof.
// Run after building the server WASM and client engine:
// npm exec --yes --package=bun@latest -- bun scripts/field-edit-proof.ts
import { createClient } from '@libsql/client';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { z } from 'zod';
import init from '../../server/wasm/pyre_wasm.js';
import { ensureDatabase, loadSchemaFromDatabase } from '../../server/schema';
import { runWithSync } from '../../server/query-sync';
import { catchup } from '../../server/sync';

await init({ module_or_path: await Bun.file(new URL('../../server/wasm/pyre_wasm_bg.wasm', import.meta.url)).arrayBuffer() });
const directory = await mkdtemp(`${tmpdir()}/pyre-field-proof-`);
const db = createClient({ url: `file:${directory}/proof.db` });
await ensureDatabase(db, 'proof', `
session {
  admin Bool
}
record Note {
  @tablename("notes")
  @watch
  @allow(query) { title != "HIDDEN" || Session.admin == True }
  @allow(update, insert, delete) { True }
  id Id.Uuid @id
  title String
  updatedAt Int
}
`);
await db.execute("insert into notes (id, title, updatedAt) values ('00000000-0000-7000-8000-000000000001', 'Initial', 1), ('00000000-0000-7000-8000-000000000002', 'Untouched', 1)");
await loadSchemaFromDatabase('proof', db);
// Precompiled one-field operation fixture, including its existing affected-row
// output. No client-authored SQL or runtime query compilation is accepted.
const queries = { edit: {
  generatedEdit: { writeStatement: 0 },
  id: 'edit', session_args: [], optional_input_args: [], json_input_args: [],
  InputValidator: z.object({ id: z.string().uuid(), title: z.string() }), SessionValidator: z.object({ admin: z.boolean() }),
  sql: [
    { include: false, params: ['id', 'title'], sql: 'update notes set title = upper($title), updatedAt = updatedAt + 1 where id = $id' },
    { include: true, params: ['id'], sql: `select json_array(json_object('table_name', 'notes', 'headers', json_array('id', 'title', 'updatedAt'), 'rows', json_array(json_array(id, title, updatedAt)))) as _affectedRows from notes where id = $id` },
  ],
} };
const bundle = await Bun.build({ entrypoints: [new URL('./field-edit-browser.ts', import.meta.url).pathname], target: 'browser' });
if (!bundle.success) throw new Error(bundle.logs.join('\n'));
const script = await bundle.outputs[0].text();
const streams = new Map<string, ReadableStreamDefaultController>();
const counts = { catchup: 0, delta: 0, invalidation: 0, mutations: 0 };
const sessions = new Map([['a', { session: { admin: true } }], ['b', { session: { admin: false } }]]);
let release: (() => void) | undefined;
let held = false;
const send = (id: string, message: any) => {
  if (message.type === 'delta') counts.delta++;
  if (message.type === 'invalidate') counts.invalidation++;
  streams.get(id)?.enqueue(new TextEncoder().encode(`data: ${JSON.stringify(message)}\n\n`));
};
const server = Bun.serve({ port: 0, idleTimeout: 0, async fetch(request) {
  const url = new URL(request.url);
  const [who, endpoint] = url.pathname.slice(1).split('/');
  try {
    if (who === 'bundle.js') return new Response(script, { headers: { 'content-type': 'text/javascript' } });
    if (who === 'stats') return Response.json({ ...counts, held, connections: [...streams.keys()] });
    if (who === 'release') { release?.(); release = undefined; return new Response('ok'); }
    if (!endpoint) return new Response('<script type="module" src="/bundle.js"></script>', { headers: { 'content-type': 'text/html' } });
    const session = sessions.get(who)!.session;
    if (endpoint === 'sync') {
      counts.catchup++;
      const body = await request.json();
      return Response.json(await catchup(db, body.syncCursor, session, 100, 'proof', body.databaseEpoch));
    }
    if (endpoint === 'events') {
      return new Response(new ReadableStream({ start(controller) {
        streams.set(who, controller);
        void db.execute('select database_epoch from _pyre_sync').then((result) => send(who, { type: 'connected', connectionId: who, databaseId: 'proof', databaseEpoch: result.rows[0].database_epoch }));
      }, cancel() { streams.delete(who); } }), { headers: { 'content-type': 'text/event-stream' } });
    }
    counts.mutations++;
    const input = await request.json();
    if (input.title === 'reject') return Response.json({ error: 'Rejected' }, { status: 403 });
    const result = await runWithSync(db, queries, Array.isArray(input) ? input : 'edit', input, session, sessions, 'proof', who);
    if (result.kind === 'error') return Response.json(result.error, { status: 400 });
    await result.sync(send);
    if (input.title === 'hold') { held = true; await new Promise<void>((resolve) => { release = resolve; }); held = false; }
    return Response.json(result.response);
  } catch (error) { console.error(error); return Response.json({ error: String(error) }, { status: 500 }); }
} });
try {
  const child = Bun.spawn([process.execPath, new URL('./field-edit-check.ts', import.meta.url).pathname, server.url.toString()], { stdout: 'inherit', stderr: 'inherit' });
  if (await child.exited !== 0) throw new Error('Browser field-edit proof failed');
} finally { server.stop(true); db.close(); await rm(directory, { recursive: true, force: true }); }
