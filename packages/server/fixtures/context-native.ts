// Isolate real WASM from the module mocks used by other server tests.
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createClient, type Client } from "@libsql/client";
import { createContextManager } from "../context";
import { run, type QueryMap } from "../query";
import { ensureDatabase } from "../schema";
import initWasm from "../wasm/pyre_wasm.js";
import { databases } from "./compiled-batch/generated/databases";
import type { Session } from "./compiled-batch/generated/decode";
import { meta as createMeta, RawInputValidator, type Input } from "./compiled-batch/generated/queries/metadata/entryCreate";
import { sql as createSql } from "./compiled-batch/generated/queries/sql/entryCreate";
import { meta as readMeta } from "./compiled-batch/generated/queries/metadata/entriesForContext";
import { sql as readSql } from "./compiled-batch/generated/queries/sql/entriesForContext";

await initWasm({ module_or_path: readFileSync(new URL("../wasm/pyre_wasm_bg.wasm", import.meta.url)) });
const directory = mkdtempSync(join(tmpdir(), "pyre-context-native-"));
const db = createClient({ url: `file:${join(directory, "test.db")}` });
try {
    await ensureDatabase(db, "_default", databases._default.schemaSource);
    const queries: QueryMap = {
        insert: { ...createMeta, sql: createSql },
        read: { ...readMeta, sql: readSql },
    };
    const session: Session = {
        userId: 7, role: "Member", unrelated: "required",
        context: { _type: "Note", count: 2, enabled: false },
    };
    const login = { id: "authenticated-login" };
    let resolutions = 0;
    const manager = createContextManager({
        getSessionKey: (global: typeof login) => global.id,
        resolveSession: async (global, id) => {
            assert.equal(global, login);
            assert.equal(id, "a");
            resolutions++;
            return session;
        },
        getDatabase: async (id) => {
            assert.equal(id, "a");
            return db;
        },
        maxAgeMs: 60_000,
    });
    // The application adapter receives the actual Client, not a manager SQL wrapper.
    const insert = (database: Client, resolved: Session, input: Input) => {
        assert.equal(database, db);
        assert.equal(resolved, session);
        return run(database, queries, "insert", input, resolved);
    };
    const context = await manager.get(login, "a");
    const ids = ["01890f6c-7b80-7000-8000-000000000001", "01890f6c-7b80-7000-8000-000000000002"];
    for (const id of ids) {
        assert.equal(await manager.get(login, "a"), context);
        const result = await context.run(insert, RawInputValidator.parse({
            id, release: "release", enabled: true, count: 1,
            role: { _type: "Member" }, details: { _type: "Note", count: 2, enabled: false },
        }));
        assert.equal(result.kind, "success", JSON.stringify(result.error));
    }
    const result = await context.run((database: Client, resolved: Session) => {
        assert.equal(database, db);
        assert.equal(resolved, session);
        return run(database, queries, "read", {}, resolved);
    }, undefined);
    assert.equal(result.kind, "success", JSON.stringify(result.error));
    assert.deepEqual(readMeta.ReturnData.parse(result.response).entry.map(row => row.id).sort(), ids);
    assert.equal(resolutions, 1);
    manager.invalidateDatabase("a");
    assert.equal(db.closed, false);
    assert.deepEqual((await db.execute("select id from entries order by id")).rows.map(row => row.id), ids);
} finally {
    db.close();
    rmSync(directory, { recursive: true, force: true });
}
