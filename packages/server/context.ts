export type ContextErrorCode = "invalid_configuration" | "denied" | "capacity"
    | "stale_context" | "stale_execution";

export class ContextError extends Error {
    constructor(readonly code: ContextErrorCode) {
        super(code === "stale_execution"
            ? "stale_execution: operation outcome unknown; mutation may have occurred; do not automatically replay"
            : code);
        this.name = "ContextError";
    }
}

export interface ContextConfig<GlobalSession, DatabaseSession, Database> {
    /** Trusted existing authenticated login/session ID, never a client-supplied key.
     * Session authority must be immutable per key; invalidate when it changes. */
    getSessionKey(globalSession: GlobalSession): string;
    /** Return null to deny access. Sessions are not cloned or frozen: applications
     * must keep global and resolved sessions immutable, including in operations. */
    resolveSession(globalSession: GlobalSession, databaseId: string): Promise<DatabaseSession | null>;
    /** Server-owned lookup, called on each run after authorization. The manager
     * never opens, caches, wraps, closes, or otherwise manages connections. */
    getDatabase(databaseId: string): Promise<Database>;
    /** Required finite positive lifetime, starting before session resolution. */
    maxAgeMs: number;
}

export interface Context<DatabaseSession, Database> {
    /** Trusted application callback. No validation or replay is performed.
     * Once invoked, invalidation/expiry means an unknown execution outcome,
     * including if the callback rejects. This is not a delivery barrier. */
    run<Input, Result>(
        operation: (database: Database, dbSession: DatabaseSession, input: Input) => Result,
        input: Input,
    ): Promise<Awaited<Result>>;
}

export interface ContextManager<GlobalSession, DatabaseSession, Database> {
    get(globalSession: GlobalSession, databaseId: string): Promise<Context<DatabaseSession, Database>>;
    invalidateSession(sessionKey: string): void;
    invalidateDatabase(databaseId: string): void;
    invalidate(sessionKey: string, databaseId: string): void;
}

type Allocation<S, D> = {
    active: boolean;
    startedAt: number;
    promise: Promise<Context<S, D>>;
};

class ManagedContext<S, D> implements Context<S, D> {
    #session: S;
    #getDatabase: () => Promise<D>;
    #current: () => boolean;

    constructor(session: S, getDatabase: () => Promise<D>, current: () => boolean) {
        this.#session = session;
        this.#getDatabase = getDatabase;
        this.#current = current;
    }

    async run<Input, Result>(operation: (database: D, dbSession: S, input: Input) => Result, input: Input): Promise<Awaited<Result>> {
        if (!this.#current()) throw new ContextError("stale_context");
        let dispatched = false;
        try {
            const database = await this.#getDatabase();
            if (!this.#current()) throw new ContextError("stale_context");
            dispatched = true;
            const result = await operation(database, this.#session, input);
            if (!this.#current()) throw new ContextError("stale_execution");
            return result;
        } catch (error) {
            if (!this.#current()) throw new ContextError(dispatched ? "stale_execution" : "stale_context");
            throw error;
        }
    }
}

/** Local, lazy cache: at most 1,024 allocations and 64 unsettled resolutions.
 * Invalidated pending work remains counted until settled; it cannot resurrect.
 * No timers, transport, query maps, schema, or session serialization are owned. */
export function createContextManager<G, S, D>(config: ContextConfig<G, S, D>): ContextManager<G, S, D> {
    const { getSessionKey, resolveSession, getDatabase, maxAgeMs } = config;
    if (!Number.isFinite(maxAgeMs) || maxAgeMs <= 0) throw new ContextError("invalid_configuration");
    const sessions = new Map<string, Map<string, Allocation<S, D>>>();
    let size = 0;
    let pending = 0;
    const current = (entry: Allocation<S, D>) => entry.active && performance.now() - entry.startedAt < maxAgeMs;
    const remove = (key: string, id: string, entry: Allocation<S, D>) => {
        entry.active = false;
        const databases = sessions.get(key);
        if (databases?.get(id) !== entry) return;
        databases.delete(id);
        size--;
        if (databases.size === 0) sessions.delete(key);
    };

    return {
        async get(globalSession, databaseId) {
            const key = getSessionKey(globalSession);
            const cached = sessions.get(key)?.get(databaseId);
            if (cached && current(cached)) return cached.promise;
            // Cache hits stay O(1); reclaim expired allocations only on misses.
            for (const [sessionKey, databases] of sessions) {
                for (const [id, entry] of databases) {
                    if (!current(entry)) remove(sessionKey, id, entry);
                }
            }
            if (size >= 1024 || pending >= 64) throw new ContextError("capacity");
            // Install the unique allocation before invoking application code, so
            // even reentrant invalidation fences this pending resolution.
            const entry: Allocation<S, D> = {
                active: true,
                startedAt: performance.now(),
                promise: Promise.resolve().then(async () => {
                    try {
                        if (!current(entry)) throw new ContextError("stale_context");
                        const session = await resolveSession(globalSession, databaseId);
                        if (!current(entry)) throw new ContextError("stale_context");
                        if (session === null) throw new ContextError("denied");
                        return new ManagedContext(session, () => getDatabase(databaseId), () => current(entry));
                    } catch (error) {
                        const stale = !current(entry);
                        remove(key, databaseId, entry);
                        if (stale) throw new ContextError("stale_context");
                        throw error;
                    } finally {
                        pending--;
                    }
                }),
            };
            let databases = sessions.get(key);
            if (!databases) sessions.set(key, databases = new Map());
            databases.set(databaseId, entry);
            size++;
            pending++;
            return entry.promise;
        },
        invalidateSession(key) {
            const databases = sessions.get(key);
            if (databases) for (const [id, entry] of databases) remove(key, id, entry);
        },
        invalidateDatabase(databaseId) {
            for (const [key, databases] of sessions) {
                const entry = databases.get(databaseId);
                if (entry) remove(key, databaseId, entry);
            }
        },
        invalidate(key, databaseId) {
            const entry = sessions.get(key)?.get(databaseId);
            if (entry) remove(key, databaseId, entry);
        },
    };
}
