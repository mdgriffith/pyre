// Real pyre serve + HTTP/SSE + public @pyre/client + Chromium proof.
// Run after building the client worker:
// bun scripts/ephemeral-serve-proof.ts
// If Bun is not installed locally: npx --yes bun scripts/ephemeral-serve-proof.ts
import assert from 'node:assert/strict';
import { createClient } from '@libsql/client';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const pyre = resolve(root, 'target/debug/pyre');
const directory = await mkdtemp(`${tmpdir()}/pyre-ephemeral-browser-proof-`);
const project = resolve(directory, 'pyre');
const generated = resolve(directory, 'generated');
const databasePath = resolve(directory, 'proof.db');
const databaseId = 'proof';
const ephemeralSentinels = [
  'EPHEMERAL_CONNECTION_A',
  'EPHEMERAL_CONNECTION_B',
  'EPHEMERAL_CONNECTION_B_NEW',
  'EPHEMERAL_SHARED_A',
  'EPHEMERAL_SHARED_B_CURRENT',
  'EPHEMERAL_READ_ONLY',
];
const indexedDbSentinels = [...ephemeralSentinels, 'shared-default'];
const durableSentinel = 'DURABLE_ROW_SURVIVES_RESTART';
const diagnostics: string[] = [];
const children: Array<ReturnType<typeof Bun.spawn>> = [];

async function command(args: string[], cwd = root): Promise<void> {
  const child = Bun.spawn(args, { cwd, stdout: 'pipe', stderr: 'pipe' });
  const [status, stdout, stderr] = await Promise.all([
    child.exited,
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
  ]);
  if (status !== 0) throw new Error(`${args.join(' ')} failed (${status})\n${stdout}${stderr}`);
}

async function freePort(): Promise<number> {
  const probe = Bun.serve({ port: 0, fetch: () => new Response('probe') });
  const port = probe.port;
  probe.stop(true);
  return port;
}

async function waitFor(url: string, label: string, timeoutMs = 20_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let lastError = 'not attempted';
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
      lastError = `HTTP ${response.status}`;
    } catch (error) {
      lastError = String(error);
    }
    await Bun.sleep(50);
  }
  throw new Error(`Timed out waiting for ${label}: ${lastError}`);
}

async function startPyreServe(port: number, browserOrigin: string) {
  const child = Bun.spawn([
    pyre, '--in', project, 'serve', databasePath,
    '--generated', generated,
    '--database-id', databaseId,
    '--port', String(port),
    '--dev-session', '{"userId":7}',
    '--participant-shared-writes',
    '--cors-origin', browserOrigin,
  ], { cwd: directory, stdout: 'pipe', stderr: 'pipe' });
  children.push(child);
  const collect = async (stream: ReadableStream<Uint8Array>, channel: string) => {
    const reader = stream.getReader();
    const decoder = new TextDecoder();
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      const text = decoder.decode(value, { stream: true });
      diagnostics.push(`[pyre ${channel}] ${text.trimEnd()}`);
    }
  };
  void collect(child.stdout, 'stdout');
  void collect(child.stderr, 'stderr');
  await waitFor(`http://127.0.0.1:${port}/health`, 'pyre serve');
  return child;
}

async function stop(child: ReturnType<typeof Bun.spawn>): Promise<void> {
  if (child.exitCode === null) child.kill();
  await Promise.race([child.exited, Bun.sleep(5_000)]);
}

await mkdir(project);
await writeFile(resolve(project, 'session.pyre'), `session {
    userId Int
}
`);
await writeFile(resolve(project, 'schema.pyre'), `

state Connection {
    userId Int = Session.userId
    cursor String?
    label String @default("none")
}

state Shared {
    marker String @default("shared-default")
    count Int @default(0)
}

record DurableMarker {
    @tablename("durable_markers")
    @public

    id Id.Uuid @id
    label String
}
`);

let browser: { close(): Promise<void> } | undefined;
let staticServer: ReturnType<typeof Bun.serve> | undefined;
const pages: Record<string, any> = {};
try {
  await command([pyre, '--in', project, 'generate', '--out', generated]);
  await command([pyre, '--in', project, 'migrate', databasePath, '--push']);
  const db = createClient({ url: `file:${databasePath}` });
  await db.execute({
    sql: 'INSERT INTO durable_markers (id, label) VALUES (?, ?)',
    args: ['00000000-0000-7000-8000-000000000159', durableSentinel],
  });
  db.close();

  const bundle = await Bun.build({
    entrypoints: [new URL('./ephemeral-serve-browser.ts', import.meta.url).pathname],
    target: 'browser',
  });
  if (!bundle.success) throw new Error(bundle.logs.join('\n'));
  const script = await bundle.outputs[0].text();
  let serveUrl = '';
  const eventStreams = new Map<string, () => void>();
  staticServer = Bun.serve({
    port: 0,
    error(error) {
      if (error?.name === 'AbortError') return new Response(null, { status: 499 });
      diagnostics.push(`[proxy error] ${error?.stack ?? error}`);
      return new Response('proxy error', { status: 502 });
    },
    fetch(request) {
      const url = new URL(request.url);
      if (url.pathname === '/bundle.js') {
        return new Response(script, { headers: { 'content-type': 'text/javascript' } });
      }
      if (url.pathname.startsWith('/disconnect/')) {
        eventStreams.get(url.pathname.slice('/disconnect/'.length))?.();
        return new Response('disconnected');
      }
      const proxied = url.pathname.match(/^\/pyre\/([^/]+)(\/.*)$/);
      if (proxied) {
        const [, clientName, path] = proxied;
        const target = new URL(`${path}${url.search}`, serveUrl);
        if (request.method !== 'GET') {
          void request.clone().text().then((body) => diagnostics.push(
            `[proxy] ${request.method} ${target.pathname} ${request.headers.get('content-type') ?? '-'} ${body}`,
          ));
        }
        const upstream = new Request(target, request);
        if (path === '/sync/events') {
          eventStreams.get(clientName)?.();
          return fetch(upstream).then((response) => {
            const reader = response.body!.getReader();
            let closed = false;
            let downstream: ReadableStreamDefaultController<Uint8Array>;
            const close = () => {
              if (closed) return;
              closed = true;
              void reader.cancel().catch(() => {});
              try { downstream.close(); } catch {}
            };
            const body = new ReadableStream<Uint8Array>({
              start(controller) {
                downstream = controller;
                void (async () => {
                  try {
                    while (!closed) {
                      const chunk = await reader.read();
                      if (chunk.done) break;
                      controller.enqueue(chunk.value);
                    }
                  } catch (error) {
                    if (!closed) controller.error(error);
                    return;
                  }
                  close();
                })();
              },
              cancel: close,
            });
            eventStreams.set(clientName, close);
            return new Response(body, { status: response.status, headers: response.headers });
          });
        }
        return fetch(upstream);
      }
      return new Response('<script type="module" src="/bundle.js"></script>', { headers: { 'content-type': 'text/html' } });
    },
  });
  const browserOrigin = `http://127.0.0.1:${staticServer.port}`;
  const servePort = await freePort();
  let serve = await startPyreServe(servePort, browserOrigin);
  serveUrl = `http://127.0.0.1:${servePort}`;

  const playwrightPath = Bun.which('playwright');
  if (!playwrightPath) throw new Error('Playwright is required on PATH');
  const { chromium } = await import(`${dirname(realpathSync(playwrightPath))}/index.mjs`);
  browser = await chromium.launch({ headless: true });

  const contexts: any[] = [];
  const open = async (name: string, readOnly = false) => {
    const context = await browser!.newContext();
    contexts.push(context);
    const page = await context.newPage();
    pages[name] = page;
    page.on('console', (message: any) => {
      if (message.type() === 'error') diagnostics.push(`[browser ${name}] ${message.text()}`);
    });
    page.on('pageerror', (error: Error) => diagnostics.push(`[browser ${name}] ${error.stack ?? error.message}`));
    const browserServerUrl = `${browserOrigin}/pyre/${name}`;
    const url = `${browserOrigin}/?name=${name}&readOnly=${readOnly}&server=${encodeURIComponent(browserServerUrl)}`;
    await page.goto(url);
    await page.waitForFunction(async () => (window as any).proof?.syncStatus() === 'live'
      && (await (window as any).proof.snapshot()).authoritative.freshness.status === 'live', null, { timeout: 20_000 });
    return { context, page };
  };
  const a = await open('a');
  const b = await open('b');
  const reader = await open('reader', true);
  const snapshot = (page: any) => page.evaluate(() => (window as any).proof.snapshot());
  const liveSnapshot = async (page: any) => {
    const deadline = Date.now() + 20_000;
    while (Date.now() < deadline) {
      const value = await snapshot(page);
      if (value.authoritative.freshness.status === 'live' && value.authoritative.connectionId) return value;
      await Bun.sleep(25);
    }
    throw new Error(`Client did not retain a live ephemeral identity: ${JSON.stringify(await snapshot(page))}`);
  };
  const aInitial = await liveSnapshot(a.page);
  const bInitial = await liveSnapshot(b.page);
  const readerInitial = await liveSnapshot(reader.page);
  const epoch = aInitial.authoritative.epoch;
  const aId = aInitial.authoritative.connectionId;
  const bId = bInitial.authoritative.connectionId;
  const readerId = readerInitial.authoritative.connectionId;
  assert(epoch, `missing first epoch: ${JSON.stringify(aInitial)}`);
  assert(aId, `missing first connection ID: ${JSON.stringify(aInitial)}`);
  assert(bId, `missing second connection ID: ${JSON.stringify(bInitial)}`);
  assert(readerId, `missing read-only connection ID: ${JSON.stringify(readerInitial)}`);
  assert.equal(bInitial.authoritative.epoch, epoch);
  assert.equal(readerInitial.authoritative.epoch, epoch);
  assert.equal(new Set([aId, bId, readerId]).size, 3, 'same-user clients must have distinct Connection IDs');
  await a.page.waitForFunction(async (ids: string[]) => {
    const connections = (await (window as any).proof.snapshot()).authoritative.connections;
    return ids.every((id) => id in connections);
  }, [aId, bId, readerId]);

  await a.page.evaluate(() => (window as any).proof.updateConnection({ cursor: 'EPHEMERAL_CONNECTION_A', label: 'a' }));
  await b.page.waitForFunction(async (id: string) => (await (window as any).proof.snapshot()).authoritative.connections[id]?.cursor === 'EPHEMERAL_CONNECTION_A', aId);
  await b.page.evaluate(() => (window as any).proof.updateConnection({ cursor: 'EPHEMERAL_CONNECTION_B', label: 'b' }));
  await a.page.waitForFunction(async (id: string) => (await (window as any).proof.snapshot()).authoritative.connections[id]?.cursor === 'EPHEMERAL_CONNECTION_B', bId);
  await a.page.evaluate(() => (window as any).proof.updateShared({ marker: 'EPHEMERAL_SHARED_A', count: 1 }));
  await reader.page.waitForFunction(async () => (await (window as any).proof.snapshot()).authoritative.shared?.marker === 'EPHEMERAL_SHARED_A');
  await a.page.waitForFunction(async () => (await (window as any).proof.snapshot()).authoritative.shared?.marker === 'EPHEMERAL_SHARED_A');

  const readOnlyRejection = await reader.page.evaluate(() => (window as any).proof.rejectedReadOnly());
  assert.equal(readOnlyRejection.accepted, false);
  assert.equal(readOnlyRejection.outcome?.status, 'rejected');
  assert.match(readOnlyRejection.message, /read-only/i);
  const invalid = await a.page.evaluate(() => (window as any).proof.invalidConnection());
  assert.equal(invalid.accepted, false);
  assert.equal(invalid.outcome?.status, 'rejected');
  assert.match(invalid.message, /expected string/i);
  await a.page.evaluate(() => (window as any).proof.updateConnection({ cursor: 'EPHEMERAL_CONNECTION_A' }));

  await reader.context.close();
  await a.page.waitForFunction(async (id: string) => !(id in (await (window as any).proof.snapshot()).authoritative.connections), readerId);
  assert(await a.page.evaluate((id: string) => (window as any).proof.history.some((entry: any) => id in entry.authoritative.connections), readerId));
  await a.page.waitForFunction(async ([peerId]: string[]) => {
    const state = (await (window as any).proof.snapshot()).authoritative;
    return state.shared?.marker === 'EPHEMERAL_SHARED_A'
      && state.connections[peerId]?.cursor === 'EPHEMERAL_CONNECTION_B';
  }, [bId]);
  const beforeDisconnect = await snapshot(a.page);
  const disconnectedAId = beforeDisconnect.authoritative.connectionId;
  assert(disconnectedAId, 'A must have a live identity before disconnect');

  await a.context.setOffline(true);
  await fetch(`${browserOrigin}/disconnect/a`);
  await a.page.waitForFunction(async () => (await (window as any).proof.snapshot()).authoritative.freshness.stale === true);
  const stale = await snapshot(a.page);
  assert.equal(stale.authoritative.shared.marker, 'EPHEMERAL_SHARED_A');
  assert.equal(stale.authoritative.connections[bId].cursor, 'EPHEMERAL_CONNECTION_B');
  await b.page.evaluate(() => (window as any).proof.updateConnection({ cursor: 'EPHEMERAL_CONNECTION_B_NEW' }));
  await b.page.evaluate(() => (window as any).proof.updateShared({ marker: 'EPHEMERAL_SHARED_B_CURRENT', count: 2 }));
  await Bun.sleep(150);
  const stillStale = await snapshot(a.page);
  assert.equal(stillStale.authoritative.shared.marker, 'EPHEMERAL_SHARED_A', 'disconnected remote view must remain stale');
  assert.equal(stillStale.authoritative.connections[bId].cursor, 'EPHEMERAL_CONNECTION_B');

  await a.context.setOffline(false);
  await a.page.waitForFunction(async (oldId: string) => {
    const state = await (window as any).proof.snapshot();
    return state.authoritative.freshness.status === 'live' && state.authoritative.connectionId !== oldId;
  }, disconnectedAId, { timeout: 20_000 });
  const reconnected = await liveSnapshot(a.page);
  const newAId = reconnected.authoritative.connectionId;
  assert(newAId && newAId !== disconnectedAId, 'reconnect must create a fresh Connection identity');
  assert.equal(reconnected.authoritative.shared.marker, 'EPHEMERAL_SHARED_B_CURRENT');
  await b.page.waitForFunction(async ([oldId, newId]: string[]) => {
    const connections = (await (window as any).proof.snapshot()).authoritative.connections;
    return !(oldId in connections) && connections[newId]?.cursor === 'EPHEMERAL_CONNECTION_A';
  }, [disconnectedAId, newAId]);
  await Bun.sleep(200);
  assert.equal((await snapshot(b.page)).authoritative.shared.marker, 'EPHEMERAL_SHARED_B_CURRENT', 'stale Shared desired state must not replay');
  assert.equal(reconnected.desired.shared.marker, 'EPHEMERAL_SHARED_A', 'desired and authoritative Shared state remain distinct');

  for (const page of [a.page, b.page]) {
    const indexed = JSON.stringify(await page.evaluate(() => (window as any).proof.scanIndexedDb()));
    for (const marker of indexedDbSentinels) assert(!indexed.includes(marker), `${marker} leaked into IndexedDB`);
  }

  await Promise.all(contexts.filter((context) => context !== reader.context).map((context) => context.close()));
  await stop(serve);
  const persisted = createClient({ url: `file:${databasePath}` });
  const durable = await persisted.execute('SELECT label FROM durable_markers');
  assert.equal(durable.rows[0]?.label, durableSentinel, 'durable row must survive runtime shutdown');
  const tables = await persisted.execute("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name");
  let durableJson = '';
  for (const row of tables.rows) {
    const name = String(row.name);
    if (!/^[A-Za-z0-9_]+$/.test(name)) throw new Error(`Unsafe SQLite table name: ${name}`);
    durableJson += JSON.stringify((await persisted.execute(`SELECT * FROM "${name}"`)).rows);
  }
  persisted.close();
  for (const marker of ephemeralSentinels) assert(!durableJson.includes(marker), `${marker} leaked into durable SQLite tables`);

  serve = await startPyreServe(servePort, browserOrigin);
  const fresh = await open('fresh');
  const freshSnapshot = await liveSnapshot(fresh.page);
  assert.notEqual(freshSnapshot.authoritative.epoch, epoch, 'server restart must create a fresh ephemeral epoch');
  assert.deepEqual(freshSnapshot.authoritative.shared, { count: 0, marker: 'shared-default' });
  assert.equal(freshSnapshot.authoritative.connections[freshSnapshot.authoritative.connectionId].cursor, null);
  const freshIndexed = JSON.stringify(await fresh.page.evaluate(() => (window as any).proof.scanIndexedDb()));
  for (const marker of indexedDbSentinels) assert(!freshIndexed.includes(marker), `${marker} leaked into fresh IndexedDB`);
  await fresh.context.close();
  await stop(serve);
  console.log('PASS: real pyre serve two-client ephemeral lifecycle, read-only/rejection, reconnect, restart, and non-persistence');
} catch (error) {
  const snapshots: Record<string, unknown> = {};
  // Page evaluation is best-effort so the original failure remains visible.
  for (const [name, page] of Object.entries(pages)) {
    try { snapshots[name] = await (page as any).evaluate(() => (window as any).proof?.snapshot()); } catch {}
  }
  console.error('Ephemeral browser proof failed', error);
  if (Object.keys(snapshots).length) console.error('Snapshots:', JSON.stringify(snapshots, null, 2));
  if (diagnostics.length) console.error(diagnostics.join('\n'));
  throw error;
} finally {
  await browser?.close();
  staticServer?.stop(true);
  await Promise.all(children.map(stop));
  await rm(directory, { recursive: true, force: true });
}
