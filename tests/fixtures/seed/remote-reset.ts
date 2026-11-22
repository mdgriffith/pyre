import assert from "node:assert/strict";
import { createClient, type Client } from "@libsql/client";
import { init } from "@pyre/server/wasm";
import { DatabaseOutcomeUnknown, inspectDatabasePolicy, reseedDatabase, resetDatabase } from "@pyre/server/schema";

await init();
const db = createClient({ url: process.env.PYRE_TEST_SQLD_URL! });
const namespace = "_default";
const source = await Bun.file("pyre/schema.pyre").text();
const cycles = await Bun.file("cycles.sql").text();
const fixtures = await Bun.file("fixtures.json").json();
const expected = await Bun.file("expected.json").json();
const scalar = async (sql: string) => (await db.execute(sql)).rows[0][0];
const epoch = () => scalar("select database_epoch from _pyre_sync");
const withMigrate = (migrate: Client["migrate"]) => ({
    protocol: db.protocol, transaction: db.transaction.bind(db), migrate,
}) as Client;
const rejectedByGuard = (error: unknown) => error instanceof DatabaseOutcomeUnknown
    && error.cause instanceof Error && /malformed JSON/i.test(error.cause.message);

try {
    await db.executeMultiple(cycles);
    const oldEpoch = await epoch();
    assert.equal(await scalar("pragma foreign_keys"), 1);
    assert.equal(await scalar("select count(*) from pragma_foreign_key_check"), 0);
    const assertOldDatabase = async () => {
        assert.equal(await epoch(), oldEpoch);
        assert.equal(await scalar("select count(*) from cycle_x"), 1);
        assert.equal(await scalar("select count(*) from posts"), 2);
    };

    // Run the exact batch over real Hrana, but with a transport that leaves FKs on.
    // A shadowing pragma_foreign_keys table still contains 0.
    const fkOn = withMigrate(statements => db.batch(statements, "write"));
    let loaderCalled = false;
    await assert.rejects(reseedDatabase(fkOn, namespace, source, () => {
        loaderCalled = true;
        return fixtures;
    }), (error: any) => error.kind === "reset-outcome-unknown" && rejectedByGuard(error.cause));
    assert.equal(loaderCalled, false);
    await assertOldDatabase();

    // Mutate each reset precondition after planning and before the write batch.
    const races = [
        ["create table late_change (x)", "drop table late_change"],
        ["update _pyre_database_policy set policy = 'preserved'", "update _pyre_database_policy set policy = 'disposable'"],
        ["update _pyre_migrations set schema_namespace = 'Other'", "update _pyre_migrations set schema_namespace = '_default'"],
    ];
    for (const [change, restore] of races) {
        const raced = withMigrate(async statements => {
            await db.execute(change);
            return db.migrate(statements);
        });
        await assert.rejects(resetDatabase(raced, namespace, source), rejectedByGuard);
        await assertOldDatabase();
        await db.execute(restore);
    }

    // This is valid Pyre but SQLite rejects the new name after the old drops.
    const invalid = source.replace("record User {", 'record User {\n    @tablename("sqlite_reserved_example")');
    await assert.rejects(resetDatabase(db, namespace, invalid), error => error instanceof DatabaseOutcomeUnknown
        && error.cause instanceof Error && /reserved/i.test(error.cause.message));
    await assertOldDatabase();

    assert.deepEqual(await reseedDatabase(db, namespace, source, async () => {
        assert.notEqual(await epoch(), oldEpoch);
        assert.equal(await scalar("select count(*) from posts"), 0);
        assert.equal(await scalar("select count(*) from sqlite_schema where name glob 'cycle_*' or name glob 'pragma_*' or name = 'self_ref'"), 0);
        return fixtures;
    }), expected);
    assert.equal(await inspectDatabasePolicy(db, namespace), "disposable");
    assert.equal(await scalar("pragma foreign_keys"), 1);

    const wsUrl = process.env.PYRE_TEST_SQLD_URL!.replace(/^http/, "ws");
    const ws = createClient({ url: wsUrl });
    try {
        await ws.executeMultiple(cycles);
        const httpEpoch = await epoch();
        await resetDatabase(ws, namespace, source);
        assert.notEqual(await epoch(), httpEpoch);
        assert.equal(await scalar("select count(*) from users"), 0);
        assert.equal(await scalar("select count(*) from sqlite_schema where name = 'cycle_x'"), 0);
    } finally { ws.close(); }
    console.log("remote-reset-ok");
} finally { db.close(); }
