import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import { after, test } from "node:test";

// From the repo root: node --test packages/client/tests/mutation-envelope.test.mjs
const client = fileURLToPath(new URL("../", import.meta.url));
const temp = mkdtempSync(fileURLToPath(new URL("../../../target/mutation-envelope-", import.meta.url)));
after(() => rmSync(temp, { recursive: true, force: true }));
execFileSync("npx", ["--yes", "--package=elm@0.19.1-6", "elm", "make", "src/Main.elm", "--optimize", `--output=${temp}/main.js`], { cwd: client, stdio: "inherit" });
const compiled = readFileSync(`${temp}/main.js`, "utf8");
const tick = () => new Promise((resolve) => setTimeout(resolve, 10));

function worker(primaryKey = { name: "id", kind: "int" }, autoPrepare = true) {
    const requests = [];
    class XMLHttpRequest {
        listeners = {};
        upload = { addEventListener() {} };
        addEventListener(name, callback) { this.listeners[name] = callback; }
        open() {}
        setRequestHeader() {}
        getAllResponseHeaders() { return ""; }
        send() { requests.push(this); }
        abort() {}
        respond(value) {
            this.status = 200;
            this.statusText = "OK";
            this.responseURL = "http://test/mutation";
            this.response = JSON.stringify(value);
            this.listeners.load();
        }
    }
    const context = vm.createContext({ XMLHttpRequest, setTimeout, clearTimeout, console });
    vm.runInContext(compiled, context);
    const app = context.Elm.Main.init({ flags: {
        schema: {
            tables: { users: { name: "users", links: {}, indices: [{ field: "name", unique: false, primary: false }], primaryKey } },
            queryFieldToTable: { users: "users" },
        },
        server: { baseUrl: "http://test", catchupPath: "/catchup" },
        sync: { autoStart: false },
    } });
    const events = { errors: [], results: [], writes: [], queries: [] };
    for (const [port, key] of Object.entries({ errorOut: "errors", queryManagerOut: "results", indexedDbOut: "writes", queryClientOut: "queries" })) {
        app.ports[port].subscribe((value) => {
            const message = JSON.parse(JSON.stringify(value));
            events[key].push(message);
            if (port === "queryManagerOut" && message.type === "localEdits") {
                events.queries.push(...message.queries);
                if (autoPrepare) for (const event of message.events) {
                    if (event.type === "prepare") app.ports.receiveQueryManagerMessage.send({ type: "localEdits", message: { ...event, type: "prepared" } });
                }
            }
        });
    }
    return {
        events,
        app,
        requests,
        async send(message) {
            app.ports.receiveQueryManagerMessage.send({ type: "localEdits", message });
            await tick();
        },
        async mutate(response) {
            app.ports.receiveQueryManagerMessage.send({ type: "sendMutation", requestId: "request", mutationId: "update", baseUrl: "http://test", input: {} });
            for (let i = 0; requests.length === 0 && i < 100; i++) await tick();
            assert.equal(requests.length, 1, "mutation should reach the HTTP boundary");
            requests.shift().respond(response);
            for (let i = 0; events.results.length === 0 && i < 100; i++) await tick();
            await tick();
            assert.equal(events.results.length, 1);
            return events.results.shift().result;
        },
    };
}

const group = { table_name: "users", headers: ["id"], rows: [[1]] };
const malformed = [
    ["row width", { type: "delta", data: [{ ...group, rows: [[1, 2]] }] }],
    ["duplicate headers", { type: "delta", data: [{ ...group, headers: ["id", "id"], rows: [[1, 1]] }] }],
    ["missing delta data", { type: "delta" }],
    ["wrong data type", { type: "delta", data: {} }],
    ["unknown envelope type", { type: "unknown" }],
    ["null envelope", null],
];

for (const [name, sync] of malformed) {
    test(`Main rejects ${name} without advancing mutation progress`, async () => {
        const { events, mutate } = worker();
        const result = await mutate({ serverRevision: 100, sync });
        assert.equal(result.ok, false);
        assert.match(result.error, /Invalid mutation sync envelope/);
        assert.deepEqual(events.errors, [result.error]);
        assert.deepEqual(events.writes, [], "no delta, cursor, or revision persistence");
        assert.deepEqual(events.queries, [], "no query publication");

        const valid = await mutate({ serverRevision: 50, sync: { type: "delta", data: [group] } });
        assert.equal(valid.ok, true);
        assert.ok(events.writes.some((event) => event.type === "writeDelta" && event.tableGroups[0].rows[0][0] === 1), "lower valid revision must still apply, not be treated as stale");
        assert.deepEqual(events.writes.filter((event) => event.type === "writeServerRevision"), [{ type: "writeServerRevision", serverRevision: 50 }]);
    });
}

test("Main still accepts an absent sync envelope for a nonoptimistic mutation", async () => {
    const { events, mutate } = worker();
    const response = { serverRevision: 100, data: {} };
    assert.deepEqual(await mutate(response), { ok: true, value: response });
    assert.deepEqual(events.errors, []);
    assert.deepEqual(events.writes, [{ type: "writeServerRevision", serverRevision: 100 }]);
});

const fence = { databaseId: "main", instance: "worker-1", authGeneration: 0, namespace: "Main", manifest: "m1", databaseEpoch: "e1" };
const wire = (type, fields = {}) => ({ ...fence, type, ...fields });
const editEvents = (w, type) => w.events.results.flatMap((message) => message.type === "localEdits" ? message.events : []).filter((event) => !type || event.type === type);
const last = (values) => values.at(-1);
const lifecycle = (w, id) => editEvents(w, "lifecycle").filter((event) => event.requestId === id).map((event) => event.state);
const rows = (w) => last(editEvents(w, "visible"))?.tables.users ?? [];
const prediction = (kind, id, fields = {}, table = "users") => ({ safe: true, kind, table, id, fields, writableFields: ["name", "note"], materializedFields: ["key", "name", "note"] });
const op = (id, fields, kind = "update") => ({ operation: `${kind}@m1`, input: { id, ...fields }, prediction: prediction(kind, id, fields) });
const submit = (requestId, operations) => wire("submit", { requestId, operations });
const hint = (atLeast, extra = {}) => ({ kind: "replaceRequired", atLeast, invalidate: false, ...extra });
const response = (requestId, fields) => wire("response", { requestId, response: { ...fence, requestId, ...fields } });
const accepted = (requestId, revision, operations = [op(1, {})], reconciliation = hint(revision)) => response(requestId, { status: "accepted", commitRevision: revision, reconciliation, results: operations.map((operation, index) => ({ index, operation: operation.operation, value: { id: operation.input.id } })) });
const rejected = (requestId) => response(requestId, { status: "rejected", code: "TargetNotWritable" });
function snapshot(w, serverRevision, data, fields = {}) {
    const request = last(editEvents(w, "catchup"));
    assert.ok(request, "worker must capture a replacement request before installation");
    return wire("replacement", { requestId: request.requestId, target: request.target, serverRevision, scope: "database", complete: true, tables: { users: { rows: data } }, ...fields });
}
async function configured(data = [{ id: 1, name: "base", note: "original" }], primaryKey, autoPrepare = true) {
    const w = worker(primaryKey, autoPrepare);
    await w.send(wire("configure", { minimumSafeRevision: 0 }));
    await w.send(snapshot(w, 0, data));
    return w;
}

test("fenced Main replays overlapping field intent after rejection without inverse rows", async () => {
    const w = await configured();
    await w.send(submit("a", [op(1, { name: "first" })]));
    await w.send(submit("b", [op(1, { name: "last" })]));
    assert.equal(rows(w)[0].name, "last");
    assert.deepEqual(editEvents(w, "dispatch").map((e) => e.requestId), ["a"]);
    await w.send(rejected("a"));
    assert.equal(rows(w)[0].name, "last");
    assert.deepEqual(editEvents(w, "dispatch").map((e) => e.requestId), ["a", "b"]);
    await w.send(rejected("b"));
    assert.deepEqual(rows(w), [{ id: 1, name: "base", note: "original" }]);
    assert.deepEqual(w.events.writes, [], "optimism is never persisted as an authoritative delta");
});

test("fenced Main refuses legacy mutation dispatch outside the ordered edit queue", async () => {
    const w = await configured();
    w.app.ports.receiveQueryManagerMessage.send({ type: "sendMutation", requestId: "legacy", mutationId: "update", baseUrl: "http://test", input: {} });
    await tick();
    assert.equal(w.requests.length, 0);
    const result = w.events.results.find(event => event.type === "mutationResult" && event.requestId === "legacy");
    assert.equal(result?.result.ok, false);
});

test("invalid accepted identities do not suppress permission invalidation", async () => {
    const w = await configured();
    await w.send(submit("a", [op(1, { name: "local" })]));
    await w.send(accepted("a", 3, [op(2, {})], hint(3, { invalidate: true, minimumSafeRevision: 3 })));
    assert.deepEqual(lifecycle(w, "a"), ["locallyApplied", "sent", "outcomeUnknown"]);
    assert.deepEqual(rows(w), []);
    assert.equal(last(editEvents(w, "catchup")).target, 3);
});

test("a fresh authenticated lifetime can recover revision-free security uncertainty", async () => {
    const w = await configured();
    await w.send(wire("syncRequired"));
    assert.deepEqual(rows(w), []);
    await w.send(wire("configure", { instance: "fresh", minimumSafeRevision: 4 }));
    await w.send(snapshot(w, 4, [{ id: 1, name: "safe" }], { instance: "fresh" }));
    assert.equal(rows(w)[0].name, "safe");
});

test("replacement quarantines sent intent, preserves untouched server fields and later unsent intent", async () => {
    const w = await configured();
    await w.send(submit("a", [op(1, { name: "first" })]));
    await w.send(submit("b", [op(1, { name: "last" })]));
    await w.send(wire("syncRequired", { reconciliation: hint(1) }));
    await w.send(snapshot(w, 1, [{ id: 1, name: "server", note: "corrected" }]));
    assert.deepEqual(rows(w), [{ id: 1, name: "last", note: "corrected" }]);
    assert.deepEqual(editEvents(w, "quarantined").map((e) => e.requestId), ["a"]);
    assert.deepEqual(lifecycle(w, "a"), ["locallyApplied", "sent"]);
    assert.deepEqual(editEvents(w, "failure"), []);
    await w.send(accepted("a", 1));
    assert.deepEqual(lifecycle(w, "a"), ["locallyApplied", "sent", "accepted", "confirmed"]);
    assert.equal(rows(w)[0].note, "corrected");
    await w.send(rejected("b"));
    assert.equal(rows(w)[0].name, "server");
});

for (const order of ["accept-first", "replace-first"]) {
    test(`acceptance is not confirmation: ${order}`, async () => {
        const w = await configured();
        await w.send(submit("a", [op(1, { name: "local" })]));
        if (order === "accept-first") {
            await w.send(accepted("a", 2));
            assert.equal(last(lifecycle(w, "a")), "accepted");
            assert.equal(rows(w)[0].name, "local");
        } else {
            await w.send(wire("syncRequired", { reconciliation: hint(2) }));
        }
        await w.send(snapshot(w, 2, [{ id: 1, name: "canonical", note: "server" }]));
        if (order === "replace-first") {
            assert.equal(last(lifecycle(w, "a")), "sent");
            await w.send(accepted("a", 2));
        }
        assert.equal(last(lifecycle(w, "a")), "confirmed");
        assert.equal(rows(w)[0].name, "canonical");
        const count = lifecycle(w, "a").length;
        await w.send(accepted("a", 2));
        await w.send(rejected("a"));
        assert.equal(lifecycle(w, "a").length, count, "duplicate completion must be silent");
    });
}

test("actual unknown blocks dispatch across replacement and resolves only on definitive evidence", async () => {
    const w = await configured();
    await w.send(submit("a", [op(1, { name: "first" })]));
    await w.send(submit("b", [op(1, { note: "later" })]));
    await w.send(wire("unknown", { requestId: "a" }));
    await w.send(wire("unknown", { requestId: "a" }));
    await w.send(wire("syncRequired", { reconciliation: hint(3) }));
    await w.send(snapshot(w, 3, [{ id: 1, name: "server", note: "new" }]));
    assert.equal(last(lifecycle(w, "a")), "outcomeUnknown");
    assert.deepEqual(editEvents(w, "dispatch").map((e) => e.requestId), ["a"]);
    assert.equal(editEvents(w, "failure").length, 1);
    assert.deepEqual(rows(w), [{ id: 1, name: "server", note: "later" }]);
    await w.send(accepted("a", 3));
    assert.equal(last(lifecycle(w, "a")), "confirmed");
    assert.deepEqual(editEvents(w, "dispatch").map((e) => e.requestId), ["a", "b"]);
});

test("new hints do not move an in-flight replacement target; security minima do", async () => {
    const w = await configured();
    await w.send(wire("syncRequired", { reconciliation: hint(10) }));
    const intermediate = snapshot(w, 10, [{ id: 1, name: "ten" }]);
    await w.send(wire("syncRequired", { reconciliation: hint(12) }));
    await w.send(intermediate);
    assert.equal(rows(w)[0].name, "ten");
    assert.equal(last(editEvents(w, "catchup")).target, 12);
    const twelve = snapshot(w, 12, [{ id: 1, name: "twelve" }]);
    await w.send(wire("syncRequired", { reconciliation: hint(15, { invalidate: true, minimumSafeRevision: 15 }) }));
    assert.deepEqual(rows(w), [], "permission uncertainty clears the entire visible scope immediately");
    await w.send(twelve);
    assert.deepEqual(rows(w), []);
    await w.send(snapshot(w, 15, []));
    assert.deepEqual(rows(w), []);
    assert.equal(last(editEvents(w, "replacementInstalled")).serverRevision, 15);
});

test("unknown security barrier cannot be cleared by a complete snapshot or ordinary hint", async () => {
    const w = await configured();
    await w.send(wire("syncRequired", { reconciliation: hint(12, { invalidate: true, minimumSafeRevision: 12 }) }));
    const outstanding = snapshot(w, 20, [{ id: 1, name: "unsafe" }]);
    await w.send(wire("syncRequired", { reconciliation: { kind: "replaceRequired", atLeast: 20 } }));
    await w.send(outstanding);
    assert.deepEqual(rows(w), []);
    await w.send(wire("syncRequired", { reconciliation: hint(21) }));
    await w.send(outstanding);
    assert.deepEqual(rows(w), []);
    await w.send(wire("syncRequired", { reconciliation: hint(20, { invalidate: true, minimumSafeRevision: 20 }) }));
    await w.send(snapshot(w, 21, [{ id: 1, name: "unsafe" }]));
    assert.equal(rows(w)[0].name, "unsafe", "authenticated barrier now permits revision 20");
    assert.equal(last(editEvents(w, "catchup")).target, 21);
});

for (const [name, fields] of [
    ["partial", { complete: false }],
    ["wrong scope", { scope: "table" }],
    ["missing table", { tables: {} }],
    ["unexpected table", { tables: { users: { rows: [] }, private: { rows: [] } } }],
    ["wrong request", { requestId: "foreign" }],
    ["forged target", { target: 999 }],
    ["old revision", { serverRevision: 0 }],
    ["wrong auth", { authGeneration: 1 }],
    ["wrong epoch", { databaseEpoch: "foreign" }],
    ["invalid identity", { tables: { users: { rows: [{ id: "not-an-int" }] } } }],
    ["duplicate identity", { tables: { users: { rows: [{ id: 1 }, { id: 1 }] } } }],
]) {
    test(`replacement ingress rejects ${name} atomically`, async () => {
        const w = await configured();
        await w.send(wire("syncRequired", { reconciliation: hint(2) }));
        const count = editEvents(w, "replacementInstalled").length;
        await w.send(snapshot(w, 2, [{ id: 1, name: "bad" }], fields));
        assert.equal(editEvents(w, "replacementInstalled").length, count);
        assert.equal(rows(w)[0].name, "base");
    });
}

const uuid = "11111111-1111-4111-8111-111111111111";
const completeCreate = op(uuid, { key: uuid, name: "created", note: null }, "create");
test("complete UUID create/update/delete replay is atomic and rejected creates leave no ghosts", async () => {
    const w = await configured([], { name: "key", kind: "uuid" });
    await w.send(submit("create", [completeCreate]));
    await w.send(submit("update", [op(uuid, { name: "updated" })]));
    assert.equal(rows(w)[0].name, "updated");
    await w.send(rejected("create"));
    assert.deepEqual(rows(w), [], "later update cannot fabricate a rejected create");
    await w.send(rejected("update"));
    await w.send(submit("atomic", [completeCreate, op(uuid, { note: "inside" }), op(uuid, {}, "delete")]));
    assert.deepEqual(rows(w), [], "create then update then delete publishes no intermediate row");
    await w.send(rejected("atomic"));
    assert.deepEqual(rows(w), []);
});

test("a missing replay target suppresses the whole batch including a preceding create", async () => {
    const w = await configured([], { name: "key", kind: "uuid" });
    const missing = "22222222-2222-4222-8222-222222222222";
    await w.send(submit("atomic", [completeCreate, op(missing, { name: "missing" })]));
    assert.deepEqual(rows(w), []);
    assert.equal(lifecycle(w, "atomic")[0], "queued");
    assert.equal(editEvents(w, "dispatch").length, 1, "suppression is not a server rejection");
});

test("delete rollback is a replay from base and complete replacement removes all absent identities", async () => {
    const w = await configured();
    await w.send(submit("delete", [op(1, {}, "delete")]));
    assert.deepEqual(rows(w), []);
    await w.send(rejected("delete"));
    assert.equal(rows(w)[0].name, "base");
    await w.send(submit("delete2", [op(1, {}, "delete")]));
    await w.send(accepted("delete2", 1, [op(1, {}, "delete")]));
    await w.send(snapshot(w, 1, []));
    assert.deepEqual(rows(w), []);
    assert.equal(last(lifecycle(w, "delete2")), "confirmed");
});

test("mixed named/integer/incomplete creates are entirely nonoptimistic", async () => {
    const w = await configured();
    await w.send(submit("named", [op(1, { name: "must-not-appear" }), { operation: "RenameUser@hash", input: { id: 1, name: "named" } }]));
    assert.equal(rows(w)[0].name, "base");
    await w.send(rejected("named"));
    await w.send(submit("integer", [op(2, { id: 2, name: "must-not-appear" }, "create")]));
    assert.equal(rows(w).length, 1);
    await w.send(rejected("integer"));
    await w.send(submit("unsafe", [{ ...op(1, { name: "must-not-appear" }), prediction: { safe: false } }]));
    assert.equal(rows(w)[0].name, "base");
});

test("disconnected queues reserve order, deduplicate effects, and empty batches do not dispatch or publish", async () => {
    const w = await configured();
    await w.send(wire("connection", { connected: false }));
    const before = editEvents(w, "visible").length;
    await w.send(submit("empty", []));
    assert.deepEqual(lifecycle(w, "empty"), ["confirmed"]);
    assert.equal(editEvents(w, "visible").length, before);
    await w.send(submit("a", [op(1, { name: "first" })]));
    await w.send(submit("a", [op(1, { name: "duplicate" })]));
    await w.send(submit("b", [op(1, { note: null })]));
    assert.deepEqual(editEvents(w, "dispatch"), []);
    assert.deepEqual(rows(w), [{ id: 1, name: "first", note: null }]);
    await w.send(wire("cancel", { requestId: "a" }));
    assert.deepEqual(rows(w), [{ id: 1, name: "base", note: null }]);
    await w.send(wire("connection", { connected: true }));
    assert.deepEqual(editEvents(w, "dispatch").map((e) => [e.requestId, e.sequence]), [["b", 2]]);
});

test("malformed same-fence commit evidence produces unknown, never rejection or false acceptance", async () => {
    const w = await configured();
    await w.send(submit("a", [op(1, { name: "local" })]));
    const malformed = accepted("a", 1);
    malformed.response.results[0].operation = "wrong@hash";
    await w.send(malformed);
    assert.equal(last(lifecycle(w, "a")), "outcomeUnknown");
    assert.equal(last(editEvents(w, "failure")).certainty, "unknown");
    await w.send(accepted("a", 1));
    assert.equal(last(lifecycle(w, "a")), "accepted");
    await w.send(wire("unknown", { requestId: "a" }));
    assert.equal(last(lifecycle(w, "a")), "accepted");
});

test("catchup failures retain acceptance and report once across read-only retries", async () => {
    const w = await configured();
    await w.send(submit("a", [op(1, { name: "local" })]));
    await w.send(accepted("a", 1));
    await w.send(wire("catchupFailed", { requestId: last(editEvents(w, "catchup")).requestId }));
    assert.equal(last(lifecycle(w, "a")), "accepted");
    assert.equal(rows(w)[0].name, "local");
    await w.send(wire("retryCatchup"));
    await w.send(wire("catchupFailed", { requestId: last(editEvents(w, "catchup")).requestId }));
    assert.equal(editEvents(w, "reconciliationFailure").length, 1);
    assert.equal(editEvents(w, "failure").length, 1);
    await w.send(wire("retryCatchup"));
    await w.send(snapshot(w, 1, [{ id: 1, name: "server" }]));
    assert.equal(last(lifecycle(w, "a")), "confirmed");
});

test("fencing ends queued/sent/accepted work with distinct certainty and ignores old traffic", async () => {
    const w = await configured();
    await w.send(submit("accepted", [op(1, { name: "first" })]));
    await w.send(accepted("accepted", 3));
    await w.send(submit("sent", [op(1, { name: "second" })]));
    await w.send(submit("queued", [op(1, { note: "third" })]));
    const old = snapshot(w, 3, [{ id: 1, name: "old" }]);
    await w.send(wire("configure", { instance: "worker-2", authGeneration: 1, minimumSafeRevision: 0 }));
    assert.deepEqual(rows(w), []);
    assert.equal(last(lifecycle(w, "accepted")), "acceptedUnreconciled");
    assert.equal(last(lifecycle(w, "sent")), "outcomeUnknown");
    assert.equal(last(lifecycle(w, "queued")), "rejected");
    const count = editEvents(w).length;
    await w.send(old);
    await w.send(accepted("sent", 3));
    assert.equal(editEvents(w).length, count);
});

test("fenced query readers use visible replay, and legacy data ingress cannot contaminate it", async () => {
    const w = await configured();
    w.app.ports.receiveQueryClientMessage.send({ type: "register", queryId: "users", querySource: { users: { id: true, name: true, note: true } }, queryInput: {} });
    await tick();
    await w.send(submit("a", [op(1, { name: "local" })]));
    assert.equal(last(w.events.queries).result.users[0].name, "local");
    w.app.ports.receiveIndexedDbMessage.send({ type: "initialData", data: { tables: { users: [{ id: 1, name: "stale-cache" }] }, cursor: { tables: {} } } });
    await tick();
    assert.equal(last(w.events.queries).result.users[0].name, "local");
    await w.send(rejected("a"));
    assert.equal(last(w.events.queries).result.users[0].name, "base");
    await w.send(submit("delete", [op(1, {}, "delete")]));
    assert.deepEqual(last(w.events.queries).result.users, []);
});

test("fenced publication combines query and entity state before dispatch in one ordered port envelope", async () => {
    const w = await configured();
    w.app.ports.receiveQueryClientMessage.send({ type: "register", queryId: "indexed", querySource: { users: { id: true, name: true, "@where": { name: { $eq: "local" } } } }, queryInput: {} });
    await tick();
    assert.deepEqual(last(w.events.queries).result.users, []);
    const independentPublications = [];
    w.app.ports.queryClientOut.subscribe((event) => independentPublications.push(event));
    await w.send(submit("a", [op(1, { name: "local" })]));
    const envelope = last(w.events.results.filter((event) => event.type === "localEdits" && event.queries.length));
    assert.equal(envelope.queries[0].result.users[0].name, "local");
    assert.equal(envelope.events.find((event) => event.type === "visible").tables.users[0].name, "local");
    assert.ok(envelope.events.findIndex((event) => event.type === "visible") < envelope.events.findIndex((event) => event.type === "prepare"));
    assert.deepEqual(independentPublications, [], "no unordered Cmd.batch publication on a second port");
    await w.send(rejected("a"));
    assert.deepEqual(last(w.events.queries).result.users, [], "replayed base rebuilds the index as well as rows");
});

test("updates never manufacture missing materialized fields, and JSON/null are whole-value setters", async () => {
    const w = await configured([{ id: 1, name: "base" }]);
    await w.send(submit("missing", [op(1, { note: "not-materialized" })]));
    assert.equal(lifecycle(w, "missing")[0], "queued");
    assert.deepEqual(rows(w), [{ id: 1, name: "base" }]);
    await w.send(rejected("missing"));
    await w.send(wire("syncRequired", { reconciliation: hint(1) }));
    await w.send(snapshot(w, 1, [{ id: 1, name: "base", note: { a: 1, b: 2 } }]));
    await w.send(submit("json", [op(1, { note: { a: 3 } })]));
    assert.deepEqual(rows(w)[0].note, { a: 3 }, "JSON members are not independently merged");
    await w.send(submit("null", [op(1, { note: null })]));
    assert.equal(rows(w)[0].note, null);
    await w.send(rejected("json"));
    assert.equal(rows(w)[0].note, null);
    await w.send(rejected("null"));
    assert.deepEqual(rows(w)[0].note, { a: 1, b: 2 });
});

test("disposal cannot reopen a retired fence or silently turn unknown into rejection", async () => {
    const w = await configured();
    await w.send(submit("a", [op(1, { name: "local" })]));
    await w.send(wire("unknown", { requestId: "a" }));
    await w.send(wire("dispose"));
    assert.equal(last(lifecycle(w, "a")), "outcomeUnknown");
    assert.equal(editEvents(w, "failure").length, 1);
    assert.deepEqual(rows(w), []);
    const count = editEvents(w).length;
    await w.send(wire("configure", { minimumSafeRevision: 0 }));
    await w.send(submit("new", [op(1, { name: "must-not-send" })]));
    assert.equal(editEvents(w).length, count);
});

test("worker ingress captures transport input and accepted results by value", async () => {
    const w = await configured();
    await w.send(wire("connection", { connected: false }));
    const submission = submit("a", [op(1, { name: "captured", note: { nested: [1, 2] } })]);
    await w.send(submission);
    submission.operations[0].input.name = "mutated";
    submission.operations[0].input.note.nested.push(3);
    submission.operations.length = 0;
    await w.send(wire("connection", { connected: true }));
    assert.deepEqual(last(editEvents(w, "dispatch")).operations[0].input, { id: 1, name: "captured", note: { nested: [1, 2] } });
    const result = accepted("a", 1);
    await w.send(result);
    result.response.results[0].value.id = 99;
    await w.send(snapshot(w, 1, [{ id: 1, name: "server" }]));
    const confirmation = last(editEvents(w, "lifecycle").filter((event) => event.state === "confirmed"));
    assert.equal(confirmation.results[0].value.id, 1);
});

test("preparation reserves order before async work but does not falsely mark a write sent", async () => {
    const w = await configured(undefined, undefined, false);
    await w.send(submit("a", [op(1, { name: "first" })]));
    await w.send(submit("b", [op(1, { name: "second" })]));
    assert.deepEqual(editEvents(w, "prepare").map((event) => event.requestId), ["a"]);
    assert.deepEqual(editEvents(w, "dispatch"), []);
    assert.deepEqual(lifecycle(w, "a"), ["locallyApplied"]);
    await w.send(wire("unknown", { requestId: "a" }));
    assert.deepEqual(editEvents(w, "failure"), [], "a request not dispatched cannot have an unknown server outcome");
    const preparation = last(editEvents(w, "prepare"));
    await w.send({ ...preparation, type: "preparationFailed" });
    assert.equal(last(lifecycle(w, "a")), "rejected");
    assert.equal(editEvents(w, "failure")[0].certainty, "rejected");
    assert.deepEqual(editEvents(w, "prepare").map((event) => event.requestId), ["a", "b"]);
    await w.send({ ...preparation, type: "prepared" });
    assert.deepEqual(editEvents(w, "dispatch"), [], "late preparation completion cannot dispatch rejected work");
    await w.send({ ...last(editEvents(w, "prepare")), type: "prepared" });
    assert.deepEqual(editEvents(w, "dispatch").map((event) => event.requestId), ["b"]);
});

test("disconnect during preparation keeps the queue and fences stale preparation attempts", async () => {
    const w = await configured(undefined, undefined, false);
    await w.send(submit("a", [op(1, { name: "local" })]));
    const old = last(editEvents(w, "prepare"));
    await w.send({ ...old, type: "notDispatched" });
    assert.deepEqual(lifecycle(w, "a"), ["locallyApplied"]);
    await w.send(wire("connection", { connected: true }));
    const fresh = last(editEvents(w, "prepare"));
    assert.notEqual(fresh.dispatchId, old.dispatchId);
    await w.send({ ...old, type: "prepared" });
    assert.deepEqual(editEvents(w, "dispatch"), []);
    await w.send({ ...fresh, type: "prepared" });
    assert.deepEqual(lifecycle(w, "a"), ["locallyApplied", "sent"]);
});

test("replacement during preparation replays unsent intent without quarantine", async () => {
    const w = await configured(undefined, undefined, false);
    await w.send(submit("a", [op(1, { name: "local" })]));
    await w.send(wire("syncRequired", { reconciliation: hint(1) }));
    await w.send(snapshot(w, 1, [{ id: 1, name: "server", note: "corrected" }]));
    assert.deepEqual(rows(w), [{ id: 1, name: "local", note: "corrected" }]);
    assert.deepEqual(editEvents(w, "quarantined"), []);
    await w.send(wire("dispose"));
    assert.equal(last(lifecycle(w, "a")), "rejected", "disposal before dispatch has definite noncommit certainty");
});

for (const [name, alter] of [
    ["missing response fence", (value) => { delete value.response.authGeneration; }],
    ["wrong result identity", (value) => { value.response.results[0].value.id = "not-an-int"; }],
    ["missing result", (value) => { value.response.results = []; }],
    ["wrong result index", (value) => { value.response.results[0].index = 1; }],
]) {
    test(`malformed correlated acceptance: ${name}`, async () => {
        const w = await configured();
        await w.send(submit("a", [op(1, { name: "local" })]));
        const bad = accepted("a", 1);
        alter(bad);
        await w.send(bad);
        assert.equal(last(lifecycle(w, "a")), "outcomeUnknown");
        assert.equal(editEvents(w, "failure")[0].certainty, "unknown");
    });
}

test("Main accepts an empty valid delta envelope", async () => {
    const { events, mutate } = worker();
    assert.equal((await mutate({ serverRevision: 100, sync: { type: "delta", data: [] } })).ok, true);
    assert.deepEqual(events.errors, []);
    assert.ok(events.writes.some((event) => event.type === "writeServerRevision" && event.serverRevision === 100));
});
