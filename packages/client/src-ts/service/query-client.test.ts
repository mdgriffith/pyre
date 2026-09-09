// @ts-nocheck
import { expect, test } from 'bun:test';
import { QueryClientService } from './query-client';

const sessionError = 'Local queries cannot reference Session; use explicit inputs or execute on the server.';

function harness() {
  const sent: unknown[] = [];
  let receive: (message: unknown) => void;
  const service = new QueryClientService();
  service.attachPorts({ ports: {
    queryClientOut: { subscribe(callback) { receive = callback; } },
    receiveQueryClientMessage: { send(message) { sent.push(message); } },
  } });
  return { service, sent, receive: (message: unknown) => receive(message) };
}

const rejectedShapes = [
  [{ $error: sessionError }, '$["$error"]'],
  [{ posts: { '@where': { 'Session.userId': 1 } } }, '$["posts"]["@where"]["Session.userId"]'],
  [{ posts: { '@where': { $and: [{ $or: [{ 'Session.isAdmin': true }] }] } } }, '$["posts"]["@where"]["$and"][0]["$or"][0]["Session.isAdmin"]'],
  [{ posts: { comments: { '@where': { 'Session.userId': { $eq: 1 } } } } }, '$["posts"]["comments"]["@where"]["Session.userId"]'],
  [{ posts: { '@where': { owner: { $eq: { $session: 'userId' } } } } }, '$["posts"]["@where"]["owner"]["$eq"]["$session"]'],
  [{ posts: { '@where': { owner: { $in: [1, { $session: 'userId' }] } } } }, '$["posts"]["@where"]["owner"]["$in"][1]["$session"]'],
  [{ posts: { '@where': { owner: { $eq: { nested: { $session: 'userId' } } } } } }, '$["posts"]["@where"]["owner"]["$eq"]["nested"]["$session"]'],
  [{ posts: { comments: { '@where': { $or: [{ owner: { $session: 'userId' } }] } } } }, '$["posts"]["comments"]["@where"]["$or"][0]["owner"]["$session"]'],
  [{ posts: { '@limit': { $session: 'limit' } } }, '$["posts"]["@limit"]["$session"]'],
];

for (const [shape, path] of rejectedShapes) {
  test(`registration rejects Session at ${path} without adding or replacing state`, () => {
    const { service, sent, receive } = harness();
    const results: unknown[] = [];
    const validShape = { posts: { '@where': { owner: { $var: 'owner' } } } };
    service.registerQuery({ queryId: 'existing', querySource: validShape, input: { owner: 1 } }, (update) => results.push(update));
    receive({ type: 'full', queryId: 'existing', revision: 3, result: ['original'] });
    sent.length = 0;

    for (const queryId of ['new-query', 'existing']) {
      expect(() => service.registerQuery({ queryId, querySource: shape, input: { owner: 2 } }, () => {
        throw new Error('rejected callback must never be registered');
      })).toThrow(`${sessionError} queryId=${queryId} path=${path}`);
    }
    expect(service.getRegisteredQueryIds()).toEqual(['existing']);
    expect(sent).toEqual([]);
    service.refreshQuery('existing');
    expect(sent[0].queryInput).toEqual({ owner: 1 });
    expect(sent[0].querySource).toEqual({ posts: { '@where': { owner: 1 } } });
    receive({ type: 'delta', queryId: 'existing', revision: 4, delta: { ops: [] } });
    expect(results.at(-1)).toEqual({ queryId: 'existing', revision: 4, result: ['original'] });
  });

  test(`update revalidates templates atomically at ${path}`, () => {
    const { service, sent } = harness();
    const template: Record<string, unknown> = {};
    service.registerQuery({ queryId: 'query', querySource: template, input: { owner: 1 } }, () => {});
    Object.assign(template, shape);
    sent.length = 0;
    expect(() => service.updateQueryInput('query', { owner: 2 })).toThrow(`queryId=query path=${path}`);
    expect(sent).toEqual([]);
    Object.keys(template).forEach((key) => delete template[key]);
    service.refreshAllQueries();
    expect(sent[0].queryInput).toEqual({ owner: 1 });
  });
}

test('resolved legacy placeholders are rejected before registration or input mutation', () => {
  const { service, sent } = harness();
  const querySource = { posts: { '@where': { owner: { $var: 'owner' } } } };
  const invalidInput = { owner: { $session: 'userId' } };
  expect(() => service.registerQuery({ queryId: 'bad', querySource, input: invalidInput }, () => {})).toThrow(sessionError);
  expect(service.getRegisteredQueryIds()).toEqual([]);
  expect(sent).toEqual([]);
  service.registerQuery({ queryId: 'good', querySource, input: { owner: 1 } }, () => {});
  sent.length = 0;
  expect(() => service.updateQueryInput('good', invalidInput)).toThrow(sessionError);
  expect(sent).toEqual([]);
  service.refreshQuery('good');
  expect(sent[0].queryInput).toEqual({ owner: 1 });
});

test('$var inputs, literal Session strings, nested selections and sync refresh remain supported', () => {
  const { service, sent } = harness();
  const querySource = { posts: {
    '@where': { $and: [{ owner: { $var: 'owner' } }, { title: { $eq: 'Session.userId' } }] },
    comments: { '@where': { body: { $in: ['contains Session.userId text', { $var: 'body' }] } } },
  } };
  service.registerQuery({ queryId: 'query', querySource, input: { owner: 1, body: 'first' } }, () => {});
  service.updateQueryInput('query', { owner: 2, body: 'Session.userId' });
  service.refreshQuery('query');
  service.refreshAllQueries();
  expect(sent.map((message) => message.type)).toEqual(['register', 'update-input', 'update-input', 'update-input']);
  expect(sent[1].querySource).toEqual({ posts: {
    '@where': { $and: [{ owner: 2 }, { title: { $eq: 'Session.userId' } }] },
    comments: { '@where': { body: { $in: ['contains Session.userId text', 'Session.userId'] } } },
  } });
  expect(sent[2]).toEqual(sent[1]);
  expect(sent[3]).toEqual(sent[1]);
  expect(querySource.posts['@where'].$and[0].owner).toEqual({ $var: 'owner' });
});

test('JSON operands remain literal through registration, input updates and refresh', () => {
  const { service, sent } = harness();
  const literals = [
    { $session: 'ordinary data', other: 1 },
    { 'Session.userId': 42 },
    { nested: { $session: false } },
    { nested: { 'Session.userId': 42 } },
    { '@where': { 'Session.userId': 42 }, $and: [{ 'Session.isAdmin': true }] },
  ];
  const querySource = { posts: {
    '@where': { $and: [{ json: { $var: 'json' } }, { otherJson: { $eq: { $var: 'json' } } }] },
    comments: { '@where': { json: { $in: [{ $var: 'json' }] } } },
  } };
  for (const [index, json] of literals.entries()) {
    service.registerQuery({ queryId: 'query', querySource, input: { json } }, () => {});
    expect(sent.at(-1).querySource.posts['@where'].$and).toEqual([{ json }, { otherJson: { $eq: json } }]);
    expect(sent.at(-1).querySource.posts.comments['@where']).toEqual({ json: { $in: [json] } });
    const updatedJson = literals[(index + 1) % literals.length];
    service.updateQueryInput('query', { json: updatedJson });
    const updatedSource = sent.at(-1).querySource;
    expect(updatedSource.posts['@where'].$and).toEqual([{ json: updatedJson }, { otherJson: { $eq: updatedJson } }]);
    service.refreshQuery('query');
    expect(sent.at(-1).querySource).toEqual(updatedSource);
    service.registerQuery({ queryId: 'literal', querySource: { posts: { '@where': { json: { $eq: json } } } }, input: {} }, () => {});
    expect(sent.at(-1).querySource.posts['@where']).toEqual({ json: { $eq: json } });
  }
});
