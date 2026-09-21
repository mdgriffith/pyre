/** Browser-independent operation construction. SQL and permissions stay on the server. */
export interface MutationModule<Input> {
  operation: 'query' | 'insert' | 'update' | 'delete' | 'transaction' | 'mutation';
  id?: string;
  optimistic?: unknown | ((input: Input) => unknown);
}

const operationBrand: unique symbol = Symbol('Pyre.Operation');

/** A captured operation. Construction has no network or visible-state effects. */
export interface Operation {
  readonly [operationBrand]: true;
}

interface CapturedOperation {
  queryId: string;
  input: unknown;
  optimistic?: unknown;
}

const capturedOperations = new WeakMap<Operation, CapturedOperation>();

/** Capture values now, so later changes to the caller's input cannot change submission. */
export function operation<Input>(module: MutationModule<Input>, input: Input): Operation {
  if (!module.id || module.operation === 'query') throw new Error('Expected a compiled mutation module');
  const snapshot = JSON.parse(JSON.stringify(input ?? {}));
  const metadata = typeof module.optimistic === 'function' ? module.optimistic(snapshot) : module.optimistic;
  const captured = JSON.parse(JSON.stringify({ queryId: module.id, input: snapshot, optimistic: metadata }));
  const result: Operation = Object.freeze({ [operationBrand]: true as const });
  capturedOperations.set(result, captured);
  return result;
}

/** Return an isolated transport snapshot, also usable by request/response and seed adapters. */
export function captureOperations(operations: readonly Operation[]): CapturedOperation[] {
  return operations.map((item) => {
    const value = capturedOperations.get(item);
    if (!value) throw new Error('Expected an operation created by a generated builder or operation()');
    return JSON.parse(JSON.stringify(value));
  });
}
