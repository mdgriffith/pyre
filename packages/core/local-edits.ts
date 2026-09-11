/** Browser-independent, immutable compiled-operation plans. */
export interface EditPrediction {
  safe: true;
  kind: 'create' | 'update' | 'delete';
  table: string;
  id: string | number;
  fields?: Record<string, unknown>;
  writableFields?: string[];
  materializedFields?: string[];
}
export interface EditOperation<I = any, R = any> {
  id: string;
  parseInput(input: unknown): I;
  decodeResult(value: unknown): R;
  predict?(input: I): EditPrediction | null;
}
export const planKey: unique symbol = Symbol('Pyre edit');
declare const namespaceKey: unique symbol;
declare const editKey: unique symbol;
declare const batchKey: unique symbol;
export interface Namespace<N> { readonly name: string; readonly manifest: string; readonly [namespaceKey]: N }
export function namespace<N>(name: string, manifest: string): Namespace<N> {
  return Object.freeze({ name, manifest }) as Namespace<N>;
}
export interface EditPlan<R> {
  readonly [planKey]: {
    readonly operations: readonly { definition: EditOperation; input: unknown; named?: true }[];
    readonly result: (values: unknown[]) => R;
    readonly namespace?: { readonly name: string; readonly manifest: string };
  };
}
export interface Edit<N, R> extends EditPlan<R> { readonly [editKey]: N }
export interface Batch<N, R> extends EditPlan<R> { readonly [batchKey]: N }
export type Created<Id> = { readonly id: Id };
export type Updated<Id> = { readonly id: Id };
export type Deleted<Id> = { readonly id: Id };
/** Descriptors capture JSON values, not mutable Date/class instances. */
export type JsonInput<T> = T extends string | number | boolean | null | undefined ? T
  : T extends Date ? string | number
  : T extends readonly (infer U)[] ? readonly JsonInput<U>[]
  : T extends object ? { readonly [K in keyof T]: JsonInput<T[K]> } : T;
export function edit<I, R>(definition: EditOperation<I, R>, input: I): EditPlan<R> {
  return Object.freeze({ [planKey]: Object.freeze({
    operations: Object.freeze([Object.freeze({ definition: Object.freeze({ ...definition }), input: capture(input) })]),
    result: (values: unknown[]) => values[0] as R,
  }) });
}
export function batch<const E extends readonly EditPlan<unknown>[]>(edits: E): EditPlan<{ readonly [K in keyof E]: E[K] extends EditPlan<infer R> ? R : never }> {
  const plans = [...edits].map(e => e[planKey]);
  const scope = plans.find(p => p.namespace)?.namespace;
  if (scope && plans.some(p => p.namespace?.name !== scope.name || p.namespace?.manifest !== scope.manifest)) throw new Error('Namespace mismatch');
  return Object.freeze({ [planKey]: Object.freeze({ ...(scope ? { namespace: scope } : {}), operations: Object.freeze(plans.flatMap(p => [...p.operations])), result: (values: unknown[]) => {
    let offset = 0;
    return plans.map(p => { const result = p.result(values.slice(offset, offset + p.operations.length)); offset += p.operations.length; return result; }) as any;
  } }) });
}
export function scopedEdit<N, I, R>(scope: Namespace<N>, definition: EditOperation<I, R>, input: I): Edit<N, R> {
  return Object.freeze({ [planKey]: Object.freeze({ ...edit(definition, input)[planKey], namespace: capture({ name: scope.name, manifest: scope.manifest }) }) }) as unknown as Edit<N, R>;
}
export function scopedBatch<N>(scope: Namespace<N>) {
  const capturedScope = capture({ name: scope.name, manifest: scope.manifest });
  return <const E extends readonly Edit<N, unknown>[]>(edits: E): Batch<N, { readonly [K in keyof E]: E[K] extends Edit<N, infer R> ? R : never }> => {
    for (const item of edits) if (item[planKey].namespace?.name !== capturedScope.name || item[planKey].namespace?.manifest !== capturedScope.manifest) throw new Error('Namespace mismatch');
    return Object.freeze({ [planKey]: Object.freeze({ ...batch(edits)[planKey], namespace: capturedScope }) }) as any;
  };
}
/** Strict JSON snapshot, including cross-realm plain objects. */
export function capture<T>(value: T): T {
  const seen = new Set<object>();
  const copy = (v: any): any => {
    if (v === null || typeof v === 'string' || typeof v === 'boolean' || (typeof v === 'number' && Number.isFinite(v))) return v;
    if (typeof v !== 'object' || seen.has(v)) throw new Error('Invalid JSON input');
    const prototype = Object.getPrototypeOf(v);
    if (!Array.isArray(v) && prototype !== null && (Object.getPrototypeOf(prototype) !== null || prototype.constructor?.name !== 'Object')) throw new Error('Invalid JSON input');
    seen.add(v);
    const result = Array.isArray(v) ? Array.from(v, copy) : Object.fromEntries(Object.entries(v).map(([k, x]) => [k, copy(x)]));
    seen.delete(v);
    return Object.freeze(result);
  };
  return copy(value);
}
