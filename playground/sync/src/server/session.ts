import type { Context } from "hono";

// Session and request validation helpers for the playground server. These are
// app-owned concerns: Pyre receives the resulting session object and databaseId,
// but this file decides how requests are authenticated/validated for the demo.

export type Session = Record<string, any>;

const sessionsById = new Map<string, Session>();
let nextSessionId = 1;

class RequestValidationError extends Error {
    constructor(public status: 400 | 404, message: string) {
        super(message);
    }
}

export function createSession(userId: string): { sessionId: string; session: Session } {
    const sessionId = `session_${nextSessionId++}`;
    const session = {
        userId,
        role: "user",
    };
    sessionsById.set(sessionId, session);
    return { sessionId, session };
}

export function getSession(sessionId: string | undefined | null): Session | null {
    if (!sessionId) {
        return null;
    }

    return sessionsById.get(sessionId) ?? null;
}

function requireDatabaseId(c: Context): string {
    const databaseId = c.req.query("databaseId");
    if (!databaseId) {
        throw new RequestValidationError(400, "databaseId query parameter is required");
    }

    return databaseId;
}

function requireSessionId(c: Context): string {
    const sessionId = c.req.query("sessionId");
    if (!sessionId) {
        throw new RequestValidationError(400, "sessionId query parameter is required");
    }

    return sessionId;
}

function requireSession(sessionId: string): Session {
    const session = getSession(sessionId);
    if (!session) {
        throw new RequestValidationError(404, "Session not found");
    }

    return session;
}

export function readLoginRequest(c: Context): { userId: string } {
    const userId = c.req.query("userId");
    if (!userId) {
        throw new RequestValidationError(400, "userId query parameter is required");
    }

    if (!/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(userId)) {
        throw new RequestValidationError(400, "userId must be a canonical lowercase UUIDv7");
    }

    return { userId };
}

export function readSyncCursor(c: Context): unknown {
    const syncCursorParam = c.req.query("syncCursor");
    if (!syncCursorParam) {
        return { tables: {} };
    }

    try {
        return JSON.parse(syncCursorParam);
    } catch {
        throw new RequestValidationError(400, "Invalid syncCursor format. Must be valid JSON.");
    }
}

export function readPyreRequestSession(
    c: Context
): { databaseId: string; sessionId: string; session: Session } {
    const databaseId = requireDatabaseId(c);
    const sessionId = requireSessionId(c);
    const session = requireSession(sessionId);

    return { databaseId, sessionId, session };
}

export async function withValidation<T>(c: Context, handler: () => T | Promise<T>): Promise<T | Response> {
    try {
        return await handler();
    } catch (error) {
        return validationErrorResponse(c, error);
    }
}

function validationErrorResponse(c: Context, error: unknown): Response {
    if (error instanceof RequestValidationError) {
        c.status(error.status);
        return c.json({ error: error.message });
    }

    throw error;
}
