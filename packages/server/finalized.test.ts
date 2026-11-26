import { expect, test } from "bun:test";
import { createClient, type Client } from "@libsql/client";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { initSync } from "./wasm/pyre_wasm.js";
import {
    applyFinalizedMigrations, auditDatabaseSchema, baselineDatabase, ensureDatabase,
    initDisposableDatabase, initPreservedDatabase, inspectDatabasePolicy,
    inspectMigrationOutcome, setDatabasePreserved,
} from "./schema";

const namespace = "Notes";
const schema = "record Note {\n    id Id.Uuid @id\n    title String\n    @public\n}\n";
const frame = (text: string) => {
    const bytes = Buffer.from(text);
    const size = Buffer.alloc(8);
    size.writeBigUInt64BE(BigInt(bytes.length));
    return Buffer.concat([size, bytes]);
};
function bundle(predecessor: string | null, sql: string, assertions: Record<string, string> = {}, targetSchema = schema, targetApiVersion = "0.0.0") {
    const manifest = { format: 1, namespace, predecessor, targetApiVersion };
    const files = { "migration.sql": sql, "schema.pyre": targetSchema, ...assertions };
    const paths = Object.keys(files).sort() as (keyof typeof files)[];
    const count = Buffer.alloc(8);
    count.writeBigUInt64BE(BigInt(paths.length));
    const digest = createHash("sha256").update("pyre-migration-bundle\0v1\0")
        .update(frame(JSON.stringify(manifest))).update(count);
    for (const path of paths) digest.update(frame(path)).update(frame(files[path]));
    const hash = digest.digest("hex");
    return { hash, manifest: JSON.stringify({ ...manifest, hash }), files };
}

const root = bundle(null, `
    create table notes (id TEXT primary key not null, title TEXT not null, updatedAt INTEGER not null default (unixepoch()));
    create index if not exists idx_notes_updatedAt_id on notes (updatedAt asc, id asc);
`);
const repair = bundle(root.hash, "update notes set title = upper(title);", { "assertions/title.sql": "select id from notes where title <> upper(title);" });
const rejected = bundle(root.hash, "update notes set title = 'discarded';", { "assertions/failed.sql": "select id from notes;" });

function target() {
    initSync({ module: readFileSync(new URL("./wasm/pyre_wasm_bg.wasm", import.meta.url)) });
    const directory = mkdtempSync(join(tmpdir(), "pyre-finalized-"));
    const connect = () => createClient({ url: `file:${join(directory, "notes.db")}` });
    const db = connect();
    return { db, connect, close() { db.close(); rmSync(directory, { recursive: true, force: true }); } };
}

test("finalized execution rolls back failed assertions and resolves a lost commit acknowledgement on reconnect", async () => {
    const { db, connect, close } = target();
    try {
        await expect(ensureDatabase(db, namespace, schema)).rejects.toThrow("Finalized migration provenance required");
        await initPreservedDatabase(db, namespace, [root]);
        await expect(initPreservedDatabase(db, namespace, [root])).rejects.toMatchObject({ outcome: "not-applied" });
        await db.execute("insert into notes (id, title) values ('01900000-0000-7000-8000-000000000001', 'keep me')");
        const epoch = async () => (await db.execute("select database_epoch from _pyre_sync")).rows[0].database_epoch;
        const before = await epoch();
        await expect(applyFinalizedMigrations(db, namespace, { bundles: [root, rejected] }))
            .rejects.toMatchObject({ outcome: "rolled-back", hash: rejected.hash });
        expect(await epoch()).toBe(before);
        expect((await db.execute("select title from notes")).rows[0].title).toBe("keep me");

        const lostAck = {
            protocol: db.protocol, execute: db.execute.bind(db),
            async transaction(mode: "write") {
                const tx = await db.transaction(mode);
                return {
                    execute: tx.execute.bind(tx), rollback: tx.rollback.bind(tx), close: tx.close.bind(tx),
                    async commit() { await tx.commit(); throw new Error("acknowledgement lost"); },
                    get closed() { return tx.closed; },
                };
            },
        } as Client;
        await expect(applyFinalizedMigrations(lostAck, namespace, { bundles: [repair, root] }))
            .rejects.toMatchObject({ outcome: "outcome-unknown", hash: repair.hash });
        const fresh = connect();
        try {
            expect(await inspectMigrationOutcome(fresh, namespace, repair)).toMatchObject({ outcome: "applied", receipt: { updateBy: null } });
            expect((await applyFinalizedMigrations(fresh, namespace, { bundles: [root, repair] })).map(result => result.outcome))
                .toEqual(["already-applied", "already-applied"]);
            expect(await ensureDatabase(fresh, namespace, schema)).toBe("up-to-date");
        } finally { fresh.close(); }
        expect(await epoch()).not.toBe(before);
        expect((await db.execute("select title from notes")).rows[0].title).toBe("KEEP ME");
    } finally { close(); }
});

test("explicit baseline and promotion preserve application rows and do not fabricate receipts", async () => {
    const { db, close } = target();
    try {
        await initDisposableDatabase(db, namespace, schema);
        await db.execute("insert into notes (id, title) values ('01900000-0000-7000-8000-000000000001', 'existing')");
        await expect(setDatabasePreserved(db, namespace, { bundles: [root] })).rejects.toThrow("Promotion requires");
        await baselineDatabase(db, namespace, root.hash, { bundles: [root] });
        expect(await inspectDatabasePolicy(db)).toBe("disposable");
        await auditDatabaseSchema(db);
        expect(await setDatabasePreserved(db, namespace, { bundles: [root] })).toBe("preserved");
        expect(await inspectDatabasePolicy(db)).toBe("preserved");
        expect((await db.execute("select title from notes")).rows[0].title).toBe("existing");
        expect((await db.execute("select * from _pyre_migration_receipts")).rows).toEqual([]);
        expect((await applyFinalizedMigrations(db, namespace, { bundles: [root] }))[0].outcome).toBe("covered-by-baseline");
    } finally { close(); }
});

test("database catalog binding is established with the root and has no filesystem fallback", async () => {
    const { db, close } = target();
    try {
        await initPreservedDatabase(db, namespace, [root, repair], "database");
        await expect(applyFinalizedMigrations(db, namespace, { bundles: [root, repair] })).rejects.toThrow("stored binding");
        expect((await applyFinalizedMigrations(db, namespace, { catalog: "database" })).map(result => result.outcome))
            .toEqual(["already-applied", "already-applied"]);
    } finally { close(); }
});

const remoteUrl = process.env.PYRE_TEST_MIGRATION_SQLD_URL;
for (const remote of [false, true]) {
    test.skipIf(remote && !remoteUrl)(`retirement preserves values and history on ${remote ? "remote libSQL" : "local TypeScript"}`, async () => {
        const local = target();
        const db = remote ? createClient({ url: remoteUrl! }) : local.db;
        const deprecatedSchema = schema.replace("title String", "title String @deprecated");
        const retiredSchema = schema.replace("title String", "title String @retired");
        const deprecated = bundle(root.hash, "", {}, deprecatedSchema, "0.1.0");
        const retired = bundle(deprecated.hash, `
            alter table notes add column preserved_title text;
            update notes set preserved_title = title;
            alter table notes drop column title;
            alter table notes rename column preserved_title to title;
        `, {}, retiredSchema, "1.0.0");
        const reactivated = bundle(retired.hash, "", {}, schema, "2.0.0");
        try {
            await initPreservedDatabase(db, namespace, [root]);
            await db.execute("insert into notes (id, title) values ('01900000-0000-7000-8000-000000000001', 'historical')");
            await expect(applyFinalizedMigrations(db, namespace, { bundles: [root, deprecated, retired, reactivated] })).rejects.toThrow("retired declaration Note.title");
            expect(await ensureDatabase(db, namespace, schema)).toBe("up-to-date");
            await applyFinalizedMigrations(db, namespace, { bundles: [root, deprecated, retired] });
            await db.execute("insert into notes (id) values ('01900000-0000-7000-8000-000000000002')");
            await auditDatabaseSchema(db);
            expect((await db.execute("select title from notes order by id")).rows.map(row => row.title)).toEqual(["historical", null]);
        } finally {
            if (remote) db.close();
            local.close();
        }
    });
}
