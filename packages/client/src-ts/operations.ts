/** Browser-independent operation construction. SQL and permissions stay on the server. */
export interface MutationModule<Input> {
  operation: 'query' | 'insert' | 'update' | 'delete' | 'transaction' | 'mutation';
  id?: string;
  primary_db?: string;
  optimistic?: unknown | ((input: Input) => unknown);
  ReturnData?: { parse(value: unknown): unknown };
}

const operationBrand: unique symbol = Symbol('Pyre.Operation');

/** A captured operation. Construction has no network or visible-state effects. */
export interface Operation<Namespace extends string = string, Result = unknown> {
  readonly [operationBrand]: true;
  /** Type-only namespace and result evidence; captured data remains private. */
  readonly __namespace?: Namespace;
  readonly __result?: Result;
}

interface CapturedOperation {
  queryId: string;
  input: unknown;
  optimistic?: unknown;
  namespace?: string;
}

const capturedOperations = new WeakMap<Operation, CapturedOperation>();
const resultDecoders = new WeakMap<Operation, (value: unknown) => unknown>();

/** Explicit namespace evidence at the database-instance boundary. */
export interface DatabaseTarget<Namespace extends string> {
  readonly namespace: Namespace;
  readonly databaseId: string;
}

export function database<const Namespace extends string>(namespace: Namespace, databaseId: string): DatabaseTarget<Namespace> {
  if (!namespace.trim() || !databaseId.trim()) throw new Error('Database namespace and databaseId are required');
  return Object.freeze({ namespace, databaseId });
}

export type OperationResults<Items extends readonly Operation[]> = {
  readonly [Index in keyof Items]: { readonly index: number; readonly queryId: string; readonly result: Items[Index] extends Operation<string, infer Result> ? Result : never };
};

export type SubmissionResult<Value> = { ok: true; value: Value } | { ok: false; error: string };

/** Unscoped legacy commands accept string IDs; generated edits require evidence. */
export type SubmissionTarget<Items extends readonly Operation[]> =
  Items extends readonly [] ? string | DatabaseTarget<string>
    : string extends NonNullable<Items[number]['__namespace']> ? string | DatabaseTarget<string>
    : DatabaseTarget<NonNullable<Items[number]['__namespace']>>;

/** Decode each compiled result instead of asserting wire JSON has application types. */
export function decodeOperationResults<const Items extends readonly Operation[]>(items: Items, value: unknown): OperationResults<Items> {
  if (!Array.isArray(value) || value.length !== items.length) throw new Error('Invalid operation result count');
  return items.map((item, index) => {
    const captured = capturedOperations.get(item);
    if (!captured) throw new Error('Expected a captured operation');
    const entry = value[index];
    if (!entry || entry.index !== index || entry.queryId !== captured.queryId) throw new Error('Invalid operation result identity');
    const decode = resultDecoders.get(item);
    return { index, queryId: captured.queryId, result: decode ? decode(entry.result) : entry.result };
  }) as OperationResults<Items>;
}

/** Capture values now, so later changes to the caller's input cannot change submission. */
export function operation<Input>(module: MutationModule<Input>, input: Input): Operation {
  if (!module.id || module.operation === 'query') throw new Error('Expected a compiled mutation module');
  const snapshot = JSON.parse(JSON.stringify(input ?? {}));
  const metadata = typeof module.optimistic === 'function' ? module.optimistic(snapshot) : module.optimistic;
  const captured = JSON.parse(JSON.stringify({ queryId: module.id, input: snapshot, optimistic: metadata, namespace: module.primary_db }));
  const result: Operation = Object.freeze({ [operationBrand]: true as const });
  capturedOperations.set(result, captured);
  if (module.ReturnData) {
    const validator = module.ReturnData;
    resultDecoders.set(result, value => validator.parse(value));
  }
  return result;
}

/** Return an isolated transport snapshot, also usable by request/response and seed adapters. */
export function captureOperations(operations: readonly Operation[], target?: DatabaseTarget<string>): CapturedOperation[] {
  let namespace: string | undefined = target?.namespace;
  return operations.map((item) => {
    const value = capturedOperations.get(item);
    if (!value) throw new Error('Expected an operation created by a generated builder or operation()');
    if (value.namespace !== undefined) {
      namespace ??= value.namespace;
      if (namespace !== value.namespace) throw new Error('Operations must target one database namespace');
    }
    return JSON.parse(JSON.stringify(value));
  });
}

/** Allocate one canonical UUIDv7 when a create is constructed, before submission. */
export function createId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  let timestamp = Date.now();
  for (let index = 5; index >= 0; index--) {
    bytes[index] = timestamp % 256;
    timestamp = Math.floor(timestamp / 256);
  }
  bytes[6] = (bytes[6] & 15) | 112;
  bytes[8] = (bytes[8] & 63) | 128;
  const hex = Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

/** Pure ordered composition. Validation also rejects forged descriptors. */
export function batch<const Items extends readonly Operation[]>(items: Items): Readonly<Items> {
  captureOperations(items);
  return Object.freeze([...items]) as unknown as Readonly<Items>;
}
