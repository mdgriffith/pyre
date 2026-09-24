import { expect, test } from "bun:test";
import {
  createDatabaseRuntime,
  DatabaseRuntimeError,
  type DatabaseRuntimeBridge,
} from "./ephemeral";

function mockBridge(overrides: Partial<DatabaseRuntimeBridge> = {}): DatabaseRuntimeBridge {
  const ok = (value: unknown) => ({ ok: true, value });
  return {
    join: () => ok({ connectionId: "c1", snapshot: snapshot() }),
    subscribe: () => ok({ connectionId: "s1", snapshot: snapshot() }),
    patch_connection: () => ok(null),
    patch_shared: () => ok(null),
    patch_shared_from_participant: () => ok(null),
    patch_shared_from_subscription: () => ok(null),
    refresh_and_renew: () => ok({ change: null, lease: { deadlineMillis: 30_000 } }),
    renew: () => ok({ deadlineMillis: 30_000 }),
    renew_subscription: () => ok({ deadlineMillis: 30_000 }),
    resubscribe: () => ok({ connectionId: "c1", snapshot: snapshot() }),
    poll: () => ok(null),
    leave: () => ok(null),
    unsubscribe: () => ok(null),
    expire: () => ok({ change: null, connectionIds: [], subscriptionIds: [] }),
    snapshot: () => ok(snapshot()),
    close: () => ok(null),
    ...overrides,
  };
}

function snapshot() {
  return { databaseId: "db", epoch: "epoch", revision: 0, shared: { count: 0 }, connections: {} };
}

test("composes and retains the application database without owning lifecycle machinery", () => {
  const database = { execute: () => undefined };
  const runtime = createDatabaseRuntime({
    databaseId: "db",
    database,
    contract: {},
    bridge: mockBridge(),
  });

  expect(runtime.database).toBe(database);
  expect(runtime.join({ ownerId: "owner", trustedSession: {} })).toMatchObject({ connectionId: "c1" });
  expect(runtime.poll("c1")).toBeNull();
  expect(runtime.expire()).toEqual({ change: null, connectionIds: [], subscriptionIds: [] });
  expect(runtime.close()).toBeNull();
});

test("maps structured bridge failures to a stable error class", () => {
  const bridge = mockBridge({
    patch_connection: () => ({
      ok: false,
      error: {
        code: "validation",
        message: "ephemeral state validation failed: expected string",
        validationErrors: [{ code: "invalid_type", path: ["cursor"], message: "expected String" }],
      },
    }),
  });
  const runtime = createDatabaseRuntime({ databaseId: "db", database: {}, contract: {}, bridge });

  expect(() => runtime.patchConnection("c1", "owner", { cursor: 3 }))
    .toThrow(DatabaseRuntimeError);
  try {
    runtime.patchConnection("c1", "owner", { cursor: 3 });
  } catch (error) {
    expect(error).toMatchObject({
      code: "validation",
      message: "ephemeral state validation failed: expected string",
      validationErrors: [{ path: ["cursor"] }],
    });
  }
});

test("wraps malformed and thrown bridge failures without string-only ambiguity", () => {
  const malformed = createDatabaseRuntime({
    databaseId: "db",
    database: {},
    contract: {},
    bridge: mockBridge({ poll: () => "broken" }),
  });
  expect(() => malformed.poll("c1")).toThrow(expect.objectContaining({
    code: "invalid_wasm_response",
  }));

  const throwing = createDatabaseRuntime({
    databaseId: "db",
    database: {},
    contract: {},
    bridge: mockBridge({ poll: () => { throw "bridge panic"; } }),
  });
  expect(() => throwing.poll("c1")).toThrow(expect.objectContaining({
    code: "runtime_call_failed",
    message: "bridge panic",
  }));
});

test("normalizes JS values only at the bridge boundary", () => {
  let received: unknown;
  const bridge = mockBridge({
    patch_shared: (patch) => {
      received = patch;
      return { ok: true, value: null };
    },
  });
  const runtime = createDatabaseRuntime({ databaseId: "db", database: {}, contract: {}, bridge });
  runtime.patchShared({ when: new Date("2025-01-02T03:04:05Z"), count: 4n } as never);
  expect(received).toEqual({ when: "2025-01-02T03:04:05.000Z", count: 4 });
});
