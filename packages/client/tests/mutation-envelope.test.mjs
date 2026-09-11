import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import { after, test } from "node:test";

// From the repo root: node --test packages/client/tests/mutation-envelope.test.mjs
const client = fileURLToPath(new URL("../", import.meta.url));
const temp = mkdtempSync(`${client}tests/.mutation-envelope-`);
after(() => rmSync(temp, { recursive: true, force: true }));
execFileSync("npx", ["--yes", "--package=elm@0.19.1-6", "elm", "make", "src/Main.elm", "--optimize", `--output=${temp}/main.js`], { cwd: client, stdio: "inherit" });
const compiled = readFileSync(`${temp}/main.js`, "utf8");
const tick = () => new Promise((resolve) => setTimeout(resolve, 10));

function worker() {
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
            tables: { users: { name: "users", links: {}, indices: [], primaryKey: { name: "id", kind: "int" } } },
            queryFieldToTable: { users: "users" },
        },
        server: { baseUrl: "http://test", catchupPath: "/catchup" },
        sync: { autoStart: false },
    } });
    const events = { errors: [], results: [], writes: [], queries: [] };
    for (const [port, key] of Object.entries({ errorOut: "errors", queryManagerOut: "results", indexedDbOut: "writes", queryClientOut: "queries" })) {
        app.ports[port].subscribe((value) => events[key].push(JSON.parse(JSON.stringify(value))));
    }
    return {
        events,
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

test("Main accepts an empty valid delta envelope", async () => {
    const { events, mutate } = worker();
    assert.equal((await mutate({ serverRevision: 100, sync: { type: "delta", data: [] } })).ok, true);
    assert.deepEqual(events.errors, []);
    assert.ok(events.writes.some((event) => event.type === "writeServerRevision" && event.serverRevision === 100));
});
