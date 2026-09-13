// @ts-nocheck
import { test, expect } from 'bun:test';
import { execFileSync } from 'node:child_process';
import { readFileSync, cpSync, mkdirSync } from 'node:fs';
import vm from 'node:vm';
import { LocalEditsRuntime } from '../../../packages/client/src-ts/service/local-edits';
import { elmLocalEdits } from '../../../packages/client/src-ts/service/elm-local-edits';

const directory = process.env.PYRE_ELM_EDIT_FIXTURE;
const tick = () => Bun.sleep(10);
async function until(check) {
  for (let n = 0; n < 100 && !check(); n++) await tick();
  expect(Boolean(check())).toBe(true);
}

test.skipIf(!directory)(
  'generated Elm effect crosses manifest-aware host and real worker exactly once',
  async () => {
    const generated = await import(`${directory}/typescript/edits/Main.ts`);
    const effect = JSON.parse(readFileSync(`${directory}/effect.json`, 'utf8'));
    expect(effect.requestId).toBe('elm:fixture:1');
    mkdirSync(`${directory}/worker`, { recursive: true });
    cpSync('packages/client/src', `${directory}/worker/src`, { recursive: true });
    cpSync('packages/client/elm.json', `${directory}/worker/elm.json`);
    execFileSync(
      'npx',
      [
        '--yes',
        '--package',
        'elm@0.19.1-6',
        'elm',
        'make',
        'src/Main.elm',
        `--output=${directory}/worker.js`,
      ],
      { cwd: `${directory}/worker`, stdio: 'pipe' },
    );
    const context = vm.createContext({ setTimeout, clearTimeout, console });
    vm.runInContext(readFileSync(`${directory}/worker.js`, 'utf8'), context);
    const schema = {
      tables: {
        issues: {
          name: 'issues',
          primaryKey: { name: 'id', kind: 'uuid' },
          links: {},
          indices: [],
        },
        audits: {
          name: 'audits',
          primaryKey: { name: 'id', kind: 'int' },
          links: {},
          indices: [],
        },
      },
      queryFieldToTable: { issue: 'issues', audit: 'audits' },
    };
    const lifecycle = [],
      ingress = [],
      writes = [],
      publications = [];
    const elmContext = vm.createContext({ setTimeout, clearTimeout, console });
    vm.runInContext(readFileSync(`${directory}/test.js`, 'utf8'), elmContext);
    const elmApp = elmContext.Elm.Test.init();
    const settled = [];
    elmApp.ports.settled.subscribe(value => settled.push(value));
    const createRuntime = (databaseId) => {
      const app = context.Elm.Main.init({
        flags: {
          schema,
          server: { baseUrl: '', catchupPath: '' },
          sync: { autoStart: false },
        },
      });
      const fence = {
        databaseId,
        instance: `elm-host-${databaseId}`,
        authGeneration: 1,
        namespace: generated.Main.name,
        manifest: generated.manifestVersion,
        databaseEpoch: 'epoch',
      };
      let revision = 0;
      const runtime = new LocalEditsRuntime(
        {
          fence,
          minimumSafeRevision: 0,
          operations: generated.operations,
          prepare: async (request) => ({
            dispatch: async () => {
              writes.push(request);
              revision++;
              return {
                ...fence,
                requestId: request.requestId,
                status: 'accepted',
                commitRevision: revision,
                results: request.operations.map((operation, index) => ({
                  index,
                  operation: operation.operation,
                  value:
                    index === 2
                      ? { audit: [{ id: 24, message: 'named', updatedAt: 1700000000 }] }
                      : {
                          id:
                            index === 0
                              ? '00000000-0000-4000-8000-000000000001'
                              : 23,
                        },
                })),
                reconciliation: {
                  kind: 'replaceRequired',
                  atLeast: revision,
                  invalidate: false,
                },
              };
            },
          }),
          replacement: async (request) => ({
            ...request,
            type: 'replacement',
            serverRevision: revision,
            scope: 'database',
            complete: true,
            tables: { issues: { rows: [] }, audits: { rows: [] } },
          }),
        },
        {
          send: (message) => {
            ingress.push(message);
            app.ports.receiveQueryManagerMessage.send(message);
          },
          install: (publication) => {
            publications.push({ databaseId, ...publication });
            return () => {};
          },
          persist: async () => {},
        },
      );
      app.ports.queryManagerOut.subscribe((message) => {
        if (message.type === 'localEdits') runtime.receiveEnvelope(message);
      });
      return runtime;
    };
    const runtimes = new Map(
      ['one', 'two'].map((database) => [database, createRuntime(database)]),
    );
    const bridge = elmLocalEdits(
      (database) =>
        runtimes.has(database)
          ? {
              runtime: runtimes.get(database),
              operations: generated.operations,
            }
          : undefined,
      (event) => { lifecycle.push(event); elmApp.ports.incoming.send(event); },
    );
    for (const runtime of runtimes.values()) runtime.start();
    await until(() =>
      ['one', 'two'].every((database) =>
        publications.some(
          (publication) =>
            publication.databaseId === database && !publication.invalid,
        ),
      ),
    );
    expect(bridge.forward({ type: 'register' })).toBe(false);
    expect(bridge.forward(effect)).toBe(true);
    expect(bridge.forward(effect)).toBe(true);
    await until(() => lifecycle.some((event) => event.state === 'confirmed'));
    await until(() => settled.length === 1);
    expect(settled).toEqual([true]);
    expect(writes).toHaveLength(1);
    expect(
      writes[0].operations.map((operation) => operation.operation),
    ).toEqual(effect.operations.map((operation) => operation.operation));
    expect(writes[0].operations[0].input.assignee).toBeNull();
    expect(
      lifecycle.filter((event) => event.state === 'confirmed'),
    ).toHaveLength(1);
    expect(
      lifecycle.find((event) => event.state === 'confirmed').results[1].value
        .id,
    ).toBe(23);
    expect(
      lifecycle.find((event) => event.state === 'confirmed').results[2].value
        .audit[0].id,
    ).toBe(24);
    expect(
      lifecycle.every((event) => event.requestId === effect.requestId),
    ).toBe(true);
    expect(
      ingress.filter((message) => message.message.type === 'submit'),
    ).toHaveLength(1);
    bridge.forward({ ...effect, databaseId: 'two', requestId: 'elm:second:1' });
    await until(() =>
      lifecycle.some(
        (event) => event.requestId === 'elm:second:1' && event.state === 'confirmed',
      ),
    );
    expect(writes).toHaveLength(2);
    expect(writes.map((write) => write.databaseId)).toEqual(['one', 'two']);
    const wrong = {
      ...effect,
      requestId: 'elm:fixture:3',
      operations: effect.operations.map((operation) => ({
        ...operation,
        namespace: 'wrong',
      })),
    };
    bridge.forward(wrong);
    expect(
      lifecycle.some(
        (event) => event.requestId === 'elm:fixture:3' && event.type === 'failure',
      ),
    ).toBe(true);
    expect(writes).toHaveLength(2);
    bridge.forward({
      ...effect,
      requestId: 'elm:fixture:4',
      operations: [
        {
          ...effect.operations[0],
          input: { id: effect.operations[0].input.id },
        },
        effect.operations[1],
      ],
    });
    await until(() =>
      lifecycle.some(
        (event) => event.requestId === 'elm:fixture:4' && event.state === 'rejected',
      ),
    );
    expect(
      lifecycle.some(
        (event) =>
          event.requestId === 'elm:fixture:4' &&
          event.type === 'failure' &&
          event.certainty === 'rejected',
      ),
    ).toBe(true);
    expect(writes).toHaveLength(2);
    await Promise.all(
      [...runtimes.values()].map((runtime) => runtime.dispose()),
    );
    bridge.dispose();
  },
);
