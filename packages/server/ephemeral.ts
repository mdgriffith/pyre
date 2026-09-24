import * as wasm from "./wasm/pyre_wasm.js";
import { normalizeForWasmJson } from "./wasm-json";

export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };

export interface ValueSchema {
  kind: "string" | "int" | "float" | "bool" | "dateTime" | "date" | "json" | "idInt" | "idUuid";
  nullable: boolean;
}

export interface NestedValueSchema {
  kind: "typedJson" | "dict";
  value: ContractValueSchema;
  nullable: boolean;
}

export interface ListValueSchema {
  kind: "list";
  item: ContractValueSchema;
  nullable: boolean;
}

export interface CustomValueSchema {
  kind: "custom";
  name: string;
  nullable: boolean;
}

export type ContractValueSchema = ValueSchema | NestedValueSchema | ListValueSchema | CustomValueSchema;

export interface ContractField {
  schema: ContractValueSchema;
  writable: boolean;
  default?: { kind: "now" } | { kind: "value"; value: JsonValue };
  derivedFrom?: string;
}

export interface ContractState {
  fields: Record<string, ContractField>;
}

export interface ContractTaggedUnion {
  variants: Record<string, Record<string, ContractValueSchema>>;
}

/** The exact `ephemeral` value emitted in Pyre's generated manifest. */
export interface Contract {
  connection?: ContractState;
  shared?: ContractState;
  types?: Record<string, ContractTaggedUnion>;
}

export interface DatabaseRuntimeConfig {
  sharedWritePolicy?: "serverOnly" | "participantWritable";
  maxParticipants?: number;
  leaseDurationMs?: number;
  downstreamDeliveryCadenceMs?: number;
  maxPendingEntries?: number;
  maxPendingControls?: number;
  maxDeliveryBytes?: number;
}

export interface EphemeralStateTypes {
  connection: object;
  connectionPatch: object;
  shared: object;
  sharedPatch: object;
}

type DefaultStateTypes = {
  connection: Record<string, unknown>;
  connectionPatch: Record<string, unknown>;
  shared: Record<string, unknown>;
  sharedPatch: Record<string, unknown>;
};

export interface EphemeralSnapshot<Connection, Shared> {
  databaseId: string;
  epoch: string;
  revision: number;
  shared?: Shared;
  connections: Record<string, Connection>;
}

export interface EphemeralChange<Connection, Shared> {
  databaseId: string;
  epoch: string;
  revision: number;
  shared?: Shared;
  connections?: Record<string, Connection>;
  removedConnections?: string[];
}

export type EphemeralDelivery<Connection, Shared> =
  | { type: "ephemeralSnapshot"; ephemeralSnapshot: EphemeralSnapshot<Connection, Shared> }
  | { type: "ephemeralChanges"; ephemeralChanges: EphemeralChange<Connection, Shared> }
  | {
      type: "ephemeralResyncRequired";
      ephemeralResyncRequired: { databaseId: string; epoch: string; revision: number };
    };

export interface RuntimeValidationError {
  code: string;
  path: string[];
  message: string;
}

export interface RuntimeErrorValue {
  code: string;
  message: string;
  validationErrors?: RuntimeValidationError[];
}

export class DatabaseRuntimeError extends Error {
  readonly code: string;
  readonly validationErrors: readonly RuntimeValidationError[];

  constructor(error: RuntimeErrorValue) {
    super(error.message);
    this.name = "DatabaseRuntimeError";
    this.code = error.code;
    this.validationErrors = error.validationErrors ?? [];
  }
}

declare const participationHandleBrand: unique symbol;
/** Server-private opaque authority for one runtime participation. */
export type ParticipationHandle = string & { readonly [participationHandleBrand]: true };

type BridgeResponse<T> = { ok: true; value: T } | { ok: false; error: RuntimeErrorValue };

export interface DatabaseRuntimeBridge {
  join(ownerId: string, trustedSession: unknown, writable: boolean): unknown;
  subscribe(ownerId: string, writable: boolean): unknown;
  patch_connection(handle: string, ownerId: string, patch: unknown): unknown;
  patch_shared(patch: unknown): unknown;
  patch_shared_from_participant(handle: string, ownerId: string, patch: unknown): unknown;
  patch_shared_from_subscription(handle: string, ownerId: string, patch: unknown): unknown;
  refresh_and_renew(handle: string, ownerId: string, trustedSession: unknown): unknown;
  renew(handle: string, ownerId: string): unknown;
  renew_subscription(handle: string, ownerId: string): unknown;
  resubscribe(handle: string, ownerId: string): unknown;
  poll(handle: string): unknown;
  leave(handle: string): unknown;
  unsubscribe(handle: string): unknown;
  expire(): unknown;
  snapshot(): unknown;
  close(): unknown;
}

interface BridgeConstructor {
  new(databaseId: string, contract: unknown, config: unknown): DatabaseRuntimeBridge;
}

export interface CreateDatabaseRuntimeOptions<Database> {
  databaseId: string;
  database: Database;
  contract: Contract;
  config?: DatabaseRuntimeConfig;
  /** Intended for tests and custom WASM loading only. */
  bridge?: DatabaseRuntimeBridge;
}

export interface Connected<Connection, Shared> {
  connectionId: string;
  handle: ParticipationHandle;
  snapshot: EphemeralSnapshot<Connection, Shared>;
}

export interface Lease {
  deadlineMillis: number;
}

export interface RefreshResult<Connection, Shared> {
  change: EphemeralChange<Connection, Shared> | null;
  lease: Lease;
}

export interface ExpirationResult<Connection, Shared> {
  change: EphemeralChange<Connection, Shared> | null;
  connectionIds: string[];
  subscriptionIds: string[];
}

export interface DatabaseRuntime<Database, Types extends EphemeralStateTypes> {
  readonly databaseId: string;
  readonly database: Database;
  join(input: { ownerId: string; trustedSession: unknown; writable?: boolean }): Connected<Types["connection"], Types["shared"]>;
  subscribe(input: { ownerId: string; writable?: boolean }): Connected<Types["connection"], Types["shared"]>;
  patchConnection(handle: ParticipationHandle, ownerId: string, patch: Types["connectionPatch"]): EphemeralChange<Types["connection"], Types["shared"]> | null;
  patchShared(patch: Types["sharedPatch"]): EphemeralChange<Types["connection"], Types["shared"]> | null;
  patchSharedFromParticipant(handle: ParticipationHandle, ownerId: string, patch: Types["sharedPatch"]): EphemeralChange<Types["connection"], Types["shared"]> | null;
  patchSharedFromSubscription(handle: ParticipationHandle, ownerId: string, patch: Types["sharedPatch"]): EphemeralChange<Types["connection"], Types["shared"]> | null;
  refreshAndRenew(handle: ParticipationHandle, ownerId: string, trustedSession: unknown): RefreshResult<Types["connection"], Types["shared"]>;
  renew(handle: ParticipationHandle, ownerId: string): Lease;
  renewSubscription(handle: ParticipationHandle, ownerId: string): Lease;
  resubscribe(handle: ParticipationHandle, ownerId: string): Connected<Types["connection"], Types["shared"]>;
  poll(handle: ParticipationHandle): EphemeralDelivery<Types["connection"], Types["shared"]> | null;
  leave(handle: ParticipationHandle): EphemeralChange<Types["connection"], Types["shared"]> | null;
  unsubscribe(handle: ParticipationHandle): void;
  expire(): ExpirationResult<Types["connection"], Types["shared"]>;
  snapshot(): EphemeralSnapshot<Types["connection"], Types["shared"]>;
  close(): EphemeralChange<Types["connection"], Types["shared"]> | null;
}

export function createDatabaseRuntime<
  Database,
  Types extends EphemeralStateTypes = DefaultStateTypes,
>(options: CreateDatabaseRuntimeOptions<Database>): DatabaseRuntime<Database, Types> {
  const bridge = options.bridge ?? createWasmBridge(options);
  const invoke = <T>(call: () => unknown): T => {
    try {
      return unwrap<T>(call());
    } catch (error) {
      if (error instanceof DatabaseRuntimeError) throw error;
      throw runtimeError(error, "runtime_call_failed");
    }
  };
  const json = (value: unknown) => normalizeForWasmJson(value);

  return {
    databaseId: options.databaseId,
    database: options.database,
    join: ({ ownerId, trustedSession, writable = true }) =>
      invoke(() => bridge.join(ownerId, json(trustedSession), writable)),
    subscribe: ({ ownerId, writable = true }) =>
      invoke(() => bridge.subscribe(ownerId, writable)),
    patchConnection: (connectionId, ownerId, patch) =>
      invoke(() => bridge.patch_connection(connectionId, ownerId, json(patch))),
    patchShared: (patch) => invoke(() => bridge.patch_shared(json(patch))),
    patchSharedFromParticipant: (connectionId, ownerId, patch) =>
      invoke(() => bridge.patch_shared_from_participant(connectionId, ownerId, json(patch))),
    patchSharedFromSubscription: (connectionId, ownerId, patch) =>
      invoke(() => bridge.patch_shared_from_subscription(connectionId, ownerId, json(patch))),
    refreshAndRenew: (connectionId, ownerId, trustedSession) =>
      invoke(() => bridge.refresh_and_renew(connectionId, ownerId, json(trustedSession))),
    renew: (connectionId, ownerId) => invoke(() => bridge.renew(connectionId, ownerId)),
    renewSubscription: (connectionId, ownerId) =>
      invoke(() => bridge.renew_subscription(connectionId, ownerId)),
    resubscribe: (connectionId, ownerId) =>
      invoke(() => bridge.resubscribe(connectionId, ownerId)),
    poll: (connectionId) => invoke(() => bridge.poll(connectionId)),
    leave: (connectionId) => invoke(() => bridge.leave(connectionId)),
    unsubscribe: (connectionId) => {
      invoke(() => bridge.unsubscribe(connectionId));
    },
    expire: () => invoke(() => bridge.expire()),
    snapshot: () => invoke(() => bridge.snapshot()),
    close: () => invoke(() => bridge.close()),
  };
}

function createWasmBridge<Database>(options: CreateDatabaseRuntimeOptions<Database>): DatabaseRuntimeBridge {
  const Constructor = (wasm as unknown as { WasmDatabaseRuntime?: BridgeConstructor })
    .WasmDatabaseRuntime;
  if (typeof Constructor !== "function") {
    throw new DatabaseRuntimeError({
      code: "wasm_not_rebuilt",
      message: "The packaged Pyre WASM artifact does not include DatabaseRuntime; rebuild pyre-wasm",
    });
  }
  try {
    return new Constructor(
      options.databaseId,
      normalizeForWasmJson(options.contract),
      normalizeForWasmJson(options.config ?? {}),
    );
  } catch (error) {
    throw runtimeError(error, "runtime_initialization_failed");
  }
}

function unwrap<T>(raw: unknown): T {
  if (!isRecord(raw) || typeof raw.ok !== "boolean") {
    throw new DatabaseRuntimeError({
      code: "invalid_wasm_response",
      message: "Pyre WASM returned an invalid DatabaseRuntime response",
    });
  }
  const response = raw as BridgeResponse<T>;
  if (response.ok) return response.value;
  throw runtimeError(response.error, "runtime_error");
}

function runtimeError(value: unknown, fallbackCode: string): DatabaseRuntimeError {
  if (isRecord(value) && typeof value.code === "string" && typeof value.message === "string") {
    return new DatabaseRuntimeError(value as unknown as RuntimeErrorValue);
  }
  const message = value instanceof Error ? value.message : String(value);
  return new DatabaseRuntimeError({ code: fallbackCode, message });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
