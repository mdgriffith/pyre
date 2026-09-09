import { expect, mock, test } from "bun:test";
import { createClient, type Client } from "@libsql/client";
import { z } from "zod";
import { createContextManager, type ContextConfig } from "./context";
import { run, type QueryMap } from "./query";

class Login {
    constructor(readonly id: string) {}
    identity() { return this.id; }
}
class Session {
    constructor(readonly role: string) {}
    label() { return this.role; }
}
const login = new Login("authenticated-login");
const sleep = () => new Promise(resolve => setTimeout(resolve, 30));
function deferred<T>() {
    let resolve!: (value: T) => void;
    let reject!: (error: unknown) => void;
    const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}
function setup(overrides: Partial<ContextConfig<Login, Session, { id: string }>> = {}) {
    const resolveSession = mock(async (global: Login, id: string) => {
        expect(global.identity()).toBe(global.id);
        return new Session(id === "a" ? "admin" : "reader");
    });
    const getDatabase = mock(async (id: string) => ({ id }));
    const config = { getSessionKey: (global: Login) => global.identity(), resolveSession, getDatabase, maxAgeMs: 60_000, ...overrides };
    return { manager: createContextManager(config), resolveSession, getDatabase };
}

test("cheap exact-pair reuse, isolated database roles, private arbitrary sessions, typed callbacks", async () => {
    const { manager, resolveSession, getDatabase } = setup();
    const a = await manager.get(login, "a");
    expect(await manager.get(new Login(login.id), "a")).toBe(a);
    const b = await manager.get(login, "b");
    const operation = (db: { id: string }, session: Session, input: number) => `${db.id}:${session.label()}:${input}`;
    const result: string = await a.run(operation, 7);
    expect(result).toBe("a:admin:7");
    expect(await b.run(operation, 8)).toBe("b:reader:8");
    expect(await a.run(async () => 9, undefined)).toBe(9);
    expect(resolveSession).toHaveBeenCalledTimes(2);
    expect(getDatabase).toHaveBeenCalledTimes(3);
    expect(login).toEqual(new Login("authenticated-login"));
    expect(Reflect.ownKeys(a)).toEqual([]);
    expect(JSON.stringify(a)).toBe("{}");
    if (false) {
        // @ts-expect-error Callback input is preserved.
        await a.run(operation, "wrong");
        // @ts-expect-error Callback result is preserved.
        const wrong: number = await a.run(operation, 1);
        // @ts-expect-error No public session accessor.
        a.session;
    }
    expect(await manager.get(new Login("a:b"), "c")).not.toBe(await manager.get(new Login("a"), "b:c"));
    expect(await manager.get(new Login("other-login"), "a")).not.toBe(a);
});

test("denial and resolver failure never fetch a database and are not cached", async () => {
    const resolver = mock(async (): Promise<Session | null> => null);
    const { manager, getDatabase } = setup({ resolveSession: resolver });
    await expect(manager.get(login, "a")).rejects.toMatchObject({ code: "denied" });
    const failure = new Error("resolver failed");
    resolver.mockImplementation(async () => { throw failure; });
    await expect(manager.get(login, "a")).rejects.toBe(failure);
    resolver.mockImplementation(async () => new Session("reader"));
    await manager.get(login, "a");
    expect(resolver).toHaveBeenCalledTimes(3);
    expect(getDatabase).not.toHaveBeenCalled();
});

test("successful concurrent gets share one resolution and context", async () => {
    const gate = deferred<Session>();
    const resolver = mock(() => gate.promise);
    const { manager } = setup({ resolveSession: resolver });
    const first = manager.get(login, "a");
    const second = manager.get(login, "a");
    await Promise.resolve();
    expect(resolver).toHaveBeenCalledTimes(1);
    gate.resolve(new Session("reader"));
    expect(await first).toBe(await second);
});

test("database lookup failures propagate unless the context became stale while waiting", async () => {
    for (const invalidate of [false, true]) {
        const gate = deferred<{ id: string }>();
        const { manager } = setup({ getDatabase: () => gate.promise });
        const context = await manager.get(login, "a");
        const operation = mock(() => 1);
        const waiting = context.run(operation, undefined);
        const failure = new Error("lookup failed");
        if (invalidate) manager.invalidate(login.id, "a");
        gate.reject(failure);
        if (invalidate) await expect(waiting).rejects.toMatchObject({ code: "stale_context" });
        else await expect(waiting).rejects.toBe(failure);
        expect(operation).not.toHaveBeenCalled();
    }
});

for (const target of ["pair", "session", "database"] as const) {
    test(`${target} invalidation fences coalesced pending work and retained contexts`, async () => {
        const gate = deferred<Session>();
        const resolver = mock(() => gate.promise);
        const { manager, getDatabase } = setup({ resolveSession: resolver });
        const invalidate = () => target === "pair" ? manager.invalidate(login.id, "a")
            : target === "session" ? manager.invalidateSession(login.id) : manager.invalidateDatabase("a");
        const first = manager.get(login, "a");
        const second = manager.get(login, "a");
        await Promise.resolve();
        expect(resolver).toHaveBeenCalledTimes(1);
        invalidate();
        resolver.mockImplementation(async () => new Session("new"));
        const replacement = await manager.get(login, "a");
        const rejected = Promise.allSettled([first, second]);
        gate.resolve(new Session("old"));
        expect(await rejected).toMatchObject([
            { status: "rejected", reason: { code: "stale_context" } },
            { status: "rejected", reason: { code: "stale_context" } },
        ]);
        expect(await manager.get(login, "a")).toBe(replacement);
        invalidate();
        const operation = mock(() => "not dispatched");
        await expect(replacement.run(operation, undefined)).rejects.toMatchObject({ code: "stale_context" });
        expect(operation).not.toHaveBeenCalled();
        expect(getDatabase).not.toHaveBeenCalled();
    });
}

test("allocation is fenced even before resolver invocation and during reentrant resolution", async () => {
    const { manager, resolveSession } = setup();
    const pending = manager.get(login, "a");
    manager.invalidateSession(login.id);
    await expect(pending).rejects.toMatchObject({ code: "stale_context" });
    expect(resolveSession).not.toHaveBeenCalled();
    const reentrant = setup({ resolveSession: async () => {
        reentrant.manager.invalidateDatabase("a");
        return new Session("reader");
    } });
    await expect(reentrant.manager.get(login, "a")).rejects.toMatchObject({ code: "stale_context" });
});

test("targeted invalidation leaves unrelated session/database pairs current", async () => {
    const { manager } = setup();
    const other = new Login("other");
    const a = await manager.get(login, "a");
    const b = await manager.get(login, "b");
    const otherA = await manager.get(other, "a");
    manager.invalidate(login.id, "a");
    await expect(a.run(() => 1, undefined)).rejects.toMatchObject({ code: "stale_context" });
    expect(await manager.get(login, "b")).toBe(b);
    expect(await manager.get(other, "a")).toBe(otherA);
    manager.invalidateSession(login.id);
    expect(await otherA.run(() => 1, undefined)).toBe(1);
    manager.invalidateDatabase("a");
    await expect(otherA.run(() => 1, undefined)).rejects.toMatchObject({ code: "stale_context" });
});

for (const expire of [false, true]) {
    test(`${expire ? "expiry" : "invalidation"} through database and operation awaits`, async () => {
        const database = deferred<{ id: string }>();
        const { manager } = setup({ maxAgeMs: expire ? 10 : 60_000, getDatabase: () => database.promise });
        const context = await manager.get(login, "a");
        const operation = mock(() => 1);
        const waiting = context.run(operation, undefined);
        if (expire) await sleep(); else manager.invalidateSession(login.id);
        database.resolve({ id: "a" });
        await expect(waiting).rejects.toMatchObject({ code: "stale_context" });
        expect(operation).not.toHaveBeenCalled();
        for (const reject of [false, true]) {
            const fresh = await manager.get(login, "a");
            const result = deferred<number>();
            let mutations = 0;
            const dispatched = fresh.run(() => { mutations++; return result.promise; }, undefined);
            await Promise.resolve();
            expect(mutations).toBe(1);
            if (expire) await sleep(); else manager.invalidateDatabase("a");
            if (reject) result.reject(new Error("operation failure")); else result.resolve(1);
            await expect(dispatched).rejects.toMatchObject({ code: "stale_execution" });
            await expect(dispatched).rejects.toThrow("mutation may have occurred; do not automatically replay");
            expect(mutations).toBe(1);
        }
    });
}

test("TTL includes resolution waits and expires retained contexts without a poller", async () => {
    const gate = deferred<Session>();
    const resolver = mock(() => gate.promise);
    const { manager, getDatabase } = setup({ maxAgeMs: 10, resolveSession: resolver });
    const pending = manager.get(login, "a");
    await sleep();
    gate.resolve(new Session("old"));
    await expect(pending).rejects.toMatchObject({ code: "stale_context" });
    const fresh = await manager.get(login, "a");
    await sleep();
    await expect(fresh.run(() => 1, undefined)).rejects.toMatchObject({ code: "stale_context" });
    expect(await manager.get(login, "a")).not.toBe(fresh);
    expect(getDatabase).not.toHaveBeenCalled();
});

test("finite positive lifetime is required", () => {
    for (const maxAgeMs of [undefined, NaN, Infinity, -Infinity, 0, -1]) {
        expect(() => setup({ maxAgeMs: maxAgeMs as number })).toThrow("invalid_configuration");
    }
});

test("bounds include invalidated unsettled resolutions, with capacity recovered after settling", async () => {
    const gate = deferred<Session>();
    const { manager, resolveSession } = setup({ resolveSession: () => gate.promise });
    const pending = Array.from({ length: 64 }, (_, n) => manager.get(login, String(n)));
    await Promise.resolve();
    manager.invalidateSession(login.id);
    await expect(manager.get(login, "new")).rejects.toMatchObject({ code: "capacity" });
    gate.resolve(new Session("reader"));
    expect((await Promise.allSettled(pending)).every(result => result.status === "rejected")).toBe(true);
    await manager.get(login, "new");
    expect(resolveSession).not.toHaveBeenCalled();
    const bounded = setup().manager;
    for (let n = 0; n < 1024; n++) await bounded.get(login, String(n));
    await bounded.get(login, "0");
    await expect(bounded.get(login, "overflow")).rejects.toMatchObject({ code: "capacity" });
    bounded.invalidate(login.id, "0");
    await bounded.get(login, "overflow");
});

test("database lookup is per run, no connection ownership, and ordinary callback errors propagate", async () => {
    const close = mock(() => {});
    let database = { id: "first", close };
    const { manager } = setup({ getDatabase: async () => database });
    const context = await manager.get(login, "a");
    expect(await context.run(db => db.id, undefined)).toBe("first");
    database = { id: "replacement", close };
    expect(await context.run(db => db.id, undefined)).toBe("replacement");
    const failure = new Error("callback failed");
    await expect(context.run(() => { throw failure; }, undefined)).rejects.toBe(failure);
    manager.invalidateDatabase("a");
    expect(close).not.toHaveBeenCalled();
});

test("native libsql ordinary query.run adapter is application-owned", async () => {
    const db = createClient({ url: "file::memory:" });
    try {
        await db.execute("create table notes (body text, role text)");
        const queries: QueryMap = {
            insert: {
                id: "insert", session_args: ["role"], optional_input_args: [], json_input_args: [],
                InputValidator: z.object({ body: z.string() }), SessionValidator: z.object({ role: z.string() }),
                sql: [{ include: false, params: ["body", "session_role"], sql: "insert into notes values ($body, $session_role)" }],
            },
        };
        const resolveSession = mock(async (_global: Login, _id: string) => new Session("admin"));
        // The real adapter uses the actual Client type, not a manager SQL wrapper.
        const native = createContextManager({
            getSessionKey: (global: Login) => global.id,
            resolveSession,
            getDatabase: async () => db,
            maxAgeMs: 60_000,
        });
        const insert = (database: Client, session: Session, input: { body: string }) => run(database, queries, "insert", input, session);
        const context = await native.get(login, "a");
        expect((await context.run(insert, { body: "one" })).kind).toBe("success");
        expect((await (await native.get(login, "a")).run(insert, { body: "two" })).kind).toBe("success");
        expect(resolveSession).toHaveBeenCalledTimes(1);
        native.invalidateDatabase("a");
        expect((await db.execute("select * from notes")).rows.map(row => ({ body: row.body, role: row.role }))).toEqual([
            { body: "one", role: "admin" }, { body: "two", role: "admin" },
        ]);
    } finally {
        db.close();
    }
});
