import type { QueryUpdate } from './query-client';

export interface EditFence {
  databaseId: string;
  instance: string;
  authGeneration: number;
  namespace: string;
  manifest: string;
  databaseEpoch: string;
}
export type VisibleTables = Record<string, Array<Record<string, unknown>>>;
export interface EditPrediction {
  safe: true;
  kind: 'create' | 'update' | 'delete';
  table: string;
  id: string | number;
  fields?: Record<string, unknown>;
  writableFields?: string[];
  materializedFields?: string[];
}
/** Trusted generated manifest entry. Codecs throw on invalid values. Named commands omit predict. */
export interface EditOperation<I = any, R = any> {
  id: string;
  parseInput(input: unknown): I;
  decodeResult(value: unknown): R;
  predict?(input: I): EditPrediction | null;
}
const planKey = Symbol('Pyre edit');
export interface EditPlan<R> {
  readonly [planKey]: { operations: readonly { definition: EditOperation; input: unknown; named?: true }[]; result: (values: unknown[]) => R };
}
export function edit<I, R>(definition: EditOperation<I, R>, input: I): EditPlan<R> {
  return Object.freeze({ [planKey]: Object.freeze({
    operations: Object.freeze([Object.freeze({ definition: Object.freeze({ ...definition }), input: capture(input) })]),
    result: (values: unknown[]) => values[0] as R,
  }) });
}
export function batch<const E extends readonly EditPlan<unknown>[]>(edits: E): EditPlan<{ readonly [K in keyof E]: E[K] extends EditPlan<infer R> ? R : never }> {
  const plans = [...edits].map(e => e[planKey]);
  return { [planKey]: { operations: plans.flatMap(p => [...p.operations]), result: values => {
    let offset = 0;
    return plans.map(p => { const result = p.result(values.slice(offset, offset + p.operations.length)); offset += p.operations.length; return result; }) as any;
  } } };
}
export type EditState = 'queued' | 'locallyApplied' | 'sent' | 'accepted' | 'confirmed' | 'rejected' | 'outcomeUnknown' | 'acceptedUnreconciled';
export type EditOutcome<R> = { kind: 'confirmed'; result: R; commitRevision?: number }
  | { kind: 'rejected' | 'outcomeUnknown'; code?: string }
  | { kind: 'acceptedUnreconciled'; result: R; commitRevision: number };
export interface EditLifecycle<R = unknown> extends EditFence {
  requestId: string;
  state: EditState;
  sequence?: number;
  commitRevision?: number;
  result?: R;
  code?: string;
  quarantined?: boolean;
}
export interface EditFailure extends EditFence {
  requestId: string;
  phase: string;
  code: string;
  certainty: 'rejected' | 'unknown' | 'acceptedUnreconciled';
  operationIndex?: number;
}
/** Read/cache failures do not assert a write outcome or commit certainty. */
export type LocalEditsFailure = EditFailure | (EditFence & {
  requestId: string;
  phase: 'reconciliation' | 'persistence';
  code: string;
  certainty?: never;
  operationIndex?: never;
});
export interface EditReceipt<R> {
  readonly requestId: string;
  readonly confirmed: Promise<EditOutcome<R>>;
  subscribe(callback: (event: EditLifecycle<R>) => void): () => void;
  cancel(): void;
}
export interface EditRequest extends EditFence {
  version: 1;
  requestId: string;
  sequence: number;
  operations: readonly { operation: string; input: unknown }[];
}
export interface ReplacementRequest extends EditFence { version: 1; requestId: string; target: number }
export interface PreparedEditTransport {
  /** Only invoked after the worker authorizes dispatch. Never retry writes. */
  dispatch(signal: AbortSignal): Promise<unknown>;
  dispose?(): void;
}
export interface LocalEditsConfig {
  fence: EditFence;
  minimumSafeRevision: number;
  operations: readonly EditOperation[];
  /** Credential lookup/encoding only, no mutation network I/O. */
  prepare(request: EditRequest, signal: AbortSignal): Promise<PreparedEditTransport>;
  replacement(request: ReplacementRequest, signal: AbortSignal): Promise<unknown>;
  timeoutMs?: number;
  connected?: boolean;
  /** Optional authenticated hint subscription; return its cleanup. */
  subscribeHints?(receive: (message: unknown) => void): () => void;
}
export interface LocalEditsPublication {
  tables: VisibleTables;
  coveredRevision: number;
  requiredRevision: number;
  invalid: boolean;
}
export interface LocalEditsHost {
  send(message: unknown): void;
  /** Install all reader state now; return notification work for after installation. */
  install(publication: LocalEditsPublication, queries: QueryUpdate[]): () => void;
  persist(tables: VisibleTables | null, revision: number | null, fence: EditFence): Promise<void>;
  end?(): void;
  syncState?(state: 'catching_up' | 'live'): void;
}
interface WorkerEvent extends EditFence {
  type: string;
  requestId: string;
  dispatchId: number;
  sequence: number;
  state: EditState;
  code?: string;
  phase: string;
  certainty: EditFailure['certainty'];
  operationIndex?: number;
  commitRevision?: number;
  target: number;
  serverRevision: number;
  tables: VisibleTables;
  coveredRevision: number;
  requiredRevision: number;
  invalid: boolean;
}
interface Pending {
  operations: { operation: string; input: unknown; prediction?: EditPrediction | null }[];
  result(values: unknown[]): unknown;
  resolve(outcome: EditOutcome<unknown>): void;
  listeners: Set<(event: EditLifecycle<any>) => void>;
  latest?: EditLifecycle;
}
interface Flight { controller: AbortController; timer?: ReturnType<typeof setTimeout>; prepared?: PreparedEditTransport; dispatched: boolean; dispatchId?: number }

/** One worker-owned lifetime. No row replay, revision inference, or write retry lives here. */
export class LocalEditsRuntime {
  readonly fence: EditFence;
  private resolveEnded!: () => void;
  readonly ended = new Promise<void>(resolve => { this.resolveEnded = resolve; });
  private active = true;
  private closing = false;
  private connected: boolean;
  private started = false;
  private counter = 0;
  private pending = new Map<string, Pending>();
  private flights = new Map<string, Flight>();
  private failures = new Set<(event: LocalEditsFailure) => void>();
  private lifecycle = new Set<(event: EditLifecycle) => void>();
  private hintsCleanup?: () => void;
  private persistence = Promise.resolve();
  private envelopes: Array<{ events: WorkerEvent[]; queries: QueryUpdate[] }> = [];
  private consuming = false;
  private manifest: Map<string, EditOperation>;

  constructor(private config: LocalEditsConfig, private host: LocalEditsHost) {
    this.config = { ...config };
    this.fence = capture(Object.fromEntries(['databaseId', 'instance', 'authGeneration', 'namespace', 'manifest', 'databaseEpoch']
      .map(key => [key, config.fence[key as keyof EditFence]])) as unknown as EditFence);
    for (const [key, value] of Object.entries(this.fence)) {
      if (key === 'authGeneration' ? !revision(value) : typeof value !== 'string' || !value) throw new Error('Invalid edit fence');
    }
    if (!revision(config.minimumSafeRevision) || (config.timeoutMs !== undefined && (!Number.isFinite(config.timeoutMs) || config.timeoutMs <= 0 || config.timeoutMs > 2147483647))) throw new Error('Invalid edit configuration');
    this.manifest = new Map(config.operations.map(op => [op.id, { ...op }]));
    if (this.manifest.size !== config.operations.length || config.operations.some(op => !op.id || typeof op.parseInput !== 'function' || typeof op.decodeResult !== 'function')) throw new Error('Invalid edit manifest');
    this.connected = config.connected ?? (typeof navigator === 'undefined' || navigator.onLine !== false);
  }
  start(): void {
    if (this.started || !this.active || this.closing) return;
    this.started = true;
    this.send({ type: 'configure', minimumSafeRevision: this.config.minimumSafeRevision });
    this.setConnected(this.connected);
    if (!this.active || this.closing) return;
    const cleanup = this.config.subscribeHints?.(message => this.receiveHint(message));
    if (!this.active || this.closing) { notify(() => cleanup?.()); return; }
    this.hintsCleanup = cleanup;
    globalThis.addEventListener?.('online', this.online);
    globalThis.addEventListener?.('offline', this.offline);
  }
  private online = () => this.setConnected(true);
  private offline = () => this.setConnected(false);
  onEditFailure(callback: (event: LocalEditsFailure) => void): () => void { this.failures.add(callback); return () => { this.failures.delete(callback); }; }
  onLifecycle(callback: (event: EditLifecycle) => void): () => void { this.lifecycle.add(callback); return () => { this.lifecycle.delete(callback); }; }
  submit<R>(plan: EditPlan<R>): EditReceipt<R> {
    const requestId = `${this.fence.instance}:edit:${++this.counter}`;
    let resolve!: (outcome: EditOutcome<any>) => void;
    const confirmed = new Promise<EditOutcome<R>>(r => { resolve = r; });
    const pending: Pending = { operations: [], result: values => values, resolve, listeners: new Set() };
    this.pending.set(requestId, pending);
    const receipt: EditReceipt<R> = { requestId, confirmed, subscribe: callback => {
      pending.listeners.add(callback);
      if (pending.latest) notify(() => callback(pending.latest as EditLifecycle<R>));
      return () => { pending.listeners.delete(callback); };
    }, cancel: () => this.send({ type: 'cancel', requestId }) };
    try {
      if (!this.started || !this.active || this.closing) throw new Error('Disposed');
      const data = plan[planKey];
      pending.result = data.result;
      pending.operations = data.operations.map(({ definition, input, named }) => {
        const op = this.manifest.get(definition.id);
        if (!op || op.parseInput !== definition.parseInput || op.decodeResult !== definition.decodeResult || op.predict !== definition.predict) throw new Error('InvalidOperation');
        const captured = capture(op.parseInput(capture(input)));
        return { operation: op.id, input: captured, ...(op.predict && !named ? { prediction: capture(op.predict(captured)) } : {}) };
      });
      if (typeof navigator !== 'undefined' && navigator.onLine === false) this.setConnected(false);
      this.send({ type: 'connection', connected: this.connected });
      this.send({ type: 'submit', requestId, operations: pending.operations });
    } catch {
      this.publishLifecycle({ ...this.fence, requestId, state: 'rejected', code: this.closing || !this.active ? 'Fenced' : 'InvalidEdit' });
      this.emitFailure({ ...this.fence, requestId, phase: 'validation', code: this.closing || !this.active ? 'Fenced' : 'InvalidEdit', certainty: 'rejected' });
    }
    return receipt;
  }
  submitNamed(operation: string, input: unknown): EditReceipt<unknown> {
    // Named operations deliberately cannot borrow generated prediction metadata.
    const definition = this.manifest.get(operation) ?? { id: '', parseInput: (x: unknown) => x, decodeResult: (x: unknown) => x };
    return this.submit({ [planKey]: { operations: [{ definition, input, named: true }], result: values => values[0] } });
  }
  setConnected(connected: boolean): void {
    const wasConnected = this.connected;
    this.connected = connected;
    this.send({ type: 'connection', connected });
    if (connected && !wasConnected) this.retryCatchup();
    if (!connected) for (const [key, flight] of this.flights) {
      if (key.startsWith('write:')) this.send(flight.dispatched
        ? { type: 'unknown', requestId: key.slice(6) }
        : { type: 'notDispatched', requestId: key.slice(6), dispatchId: flight.dispatchId });
      else this.send({ type: 'catchupFailed', requestId: key.slice(5) });
      this.release(key);
    }
  }
  retryCatchup(): void { this.send({ type: 'retryCatchup' }); }
  receiveHint(message: unknown): void {
    if (!this.active || this.closing) return;
    if (!record(message) || message.type !== 'syncRequired') return;
    if (this.sameFence(message)) {
      try { this.host.send({ type: 'localEdits', message: capture(message) }); }
      catch { this.send({ type: 'syncRequired' }); }
    }
    else this.checkEpoch(message);
  }
  /** Also accepts late definitive evidence after a transport timed out. */
  receiveResponse(requestId: string, response: unknown): void {
    if (!this.active || this.closing) return;
    try { response = capture(response); } catch { this.send({ type: 'unknown', requestId }); return; }
    if (!record(response) || Object.keys(this.fence).some(key => response[key] === undefined) || response.requestId === undefined) { this.send({ type: 'unknown', requestId }); return; }
    if (response.requestId !== requestId) return;
    if (!this.sameFence(response)) { this.checkEpoch(response); return; }
    const pending = this.pending.get(requestId);
    if (!pending || !['sent', 'outcomeUnknown'].includes(pending.latest?.state ?? '')) return;
    if (response.status === 'accepted') {
      try {
        const results = response.results;
        if (!Array.isArray(results) || results.length !== pending.operations.length) throw new Error();
        results.forEach((result, index) => {
          const op = pending.operations[index];
          if (!record(result) || result.index !== index || result.operation !== op.operation) throw new Error();
          this.manifest.get(op.operation)!.decodeResult(capture(result.value));
        });
      } catch {
        // Result decoding cannot discard independent permission-removal evidence.
        this.receiveHint({ ...response, type: 'syncRequired' });
        this.send({ type: 'unknown', requestId });
        return;
      }
    }
    this.send({ type: 'response', requestId, response: capture(response) });
  }
  /** Called only by the existing query-manager bridge. */
  receiveEnvelope(envelope: { events: WorkerEvent[]; queries: QueryUpdate[] }): void {
    this.envelopes.push(capture(envelope));
    if (this.consuming) return;
    this.consuming = true;
    try {
      while (this.envelopes.length) {
        const next = this.envelopes.shift()!;
        const events = next.events.filter(e => this.active && this.sameFence(e));
        if (!events.length) continue;
        const visible = events.find(e => e.type === 'visible');
        const publish = visible ? this.host.install(visible, next.queries) : () => {};
        for (const event of events) if (event.type === 'invalidate' || event.type === 'replacementInstalled' || event.type === 'lifetimeEnded') {
          const tables = event.type === 'replacementInstalled' ? capture(event.tables) : null;
          this.persistence = this.persistence.then(() => this.host.persist(tables, tables ? event.serverRevision : null, this.fence)).catch(() => {
            this.emitFailure({ ...this.fence, requestId: event.requestId ?? '', phase: 'persistence', code: 'PersistenceFailed' });
          });
        }
        notify(publish);
        if (events.some(event => event.type === 'catchup' || event.type === 'invalidate')) notify(() => this.host.syncState?.('catching_up'));
        else if (events.some(event => event.type === 'replacementInstalled')) notify(() => this.host.syncState?.('live'));
        for (const event of events) this.effect(event);
      }
    } finally { this.consuming = false; }
  }
  private effect(event: WorkerEvent): void {
    const requestId = event.requestId;
    switch (event.type) {
      case 'lifecycle': this.publishLifecycle(event); break;
      case 'quarantined': {
        const latest = this.pending.get(requestId)?.latest;
        if (latest) this.publishLifecycle({ ...latest, quarantined: true });
        break;
      }
      case 'failure': this.emitFailure({ ...event, code: event.code ?? 'EditFailed' }); break;
      case 'reconciliationFailure': this.emitFailure({ ...this.fence, requestId, phase: 'reconciliation', code: event.code ?? 'CatchupFailed' }); break;
      case 'prepare': this.prepare(event); break;
      case 'dispatch': {
        const flight = this.flights.get(`write:${requestId}`);
        if (!flight?.prepared || flight.dispatchId !== event.dispatchId || flight.controller.signal.aborted || this.closing) { this.send({ type: 'unknown', requestId }); break; }
        flight.dispatched = true;
        clearTimeout(flight.timer);
        flight.timer = setTimeout(() => this.send({ type: 'unknown', requestId }), this.config.timeoutMs ?? 30000);
        Promise.resolve().then(() => {
          if (flight.controller.signal.aborted || this.closing) throw new Error('Disposed');
          return flight.prepared!.dispatch(flight.controller.signal);
        }).then(response => {
          this.receiveResponse(requestId, response);
        }, () => { clearTimeout(flight.timer); this.send({ type: 'unknown', requestId }); });
        break;
      }
      case 'preparationCancelled': this.release(`write:${requestId}`); break;
      case 'catchup': this.catchup(event); break;
      case 'lifetimeEnded': this.detach(); break;
    }
  }
  private prepare(event: WorkerEvent): void {
    const { requestId, dispatchId, sequence } = event;
    const key = `write:${requestId}`;
    if (this.closing) return;
    const flight: Flight = { controller: new AbortController(), dispatched: false, dispatchId };
    this.flights.set(key, flight);
    const request: EditRequest = capture({ ...this.fence, version: 1, requestId, sequence, operations: this.pending.get(requestId)!.operations.map(({ operation, input }) => ({ operation, input })) });
    flight.timer = setTimeout(() => {
      this.release(key);
      this.send({ type: 'preparationFailed', requestId, dispatchId });
    }, this.config.timeoutMs ?? 30000);
    Promise.resolve().then(() => {
      if (flight.controller.signal.aborted || this.closing) throw new Error('Disposed');
      return this.config.prepare(request, flight.controller.signal);
    }).then(prepared => {
      if (this.flights.get(key) !== flight || flight.controller.signal.aborted || this.closing) { notify(() => prepared.dispose?.()); return; }
      flight.prepared = prepared;
      this.send({ type: this.connected ? 'prepared' : 'notDispatched', requestId, dispatchId });
    }, () => {
      if (this.flights.get(key) !== flight) return;
      this.release(key);
      this.send({ type: 'preparationFailed', requestId, dispatchId });
    });
  }
  private catchup(event: WorkerEvent): void {
    if (this.closing) return;
    const { requestId, target } = event;
    const key = `read:${requestId}`;
    const flight: Flight = { controller: new AbortController(), dispatched: false };
    this.flights.set(key, flight);
    const failed = () => { this.release(key); this.send({ type: 'catchupFailed', requestId }); };
    flight.timer = setTimeout(failed, this.config.timeoutMs ?? 30000);
    Promise.resolve().then(() => {
      if (flight.controller.signal.aborted || this.closing || !this.connected) throw new Error('Disconnected');
      return this.config.replacement(capture({ ...this.fence, version: 1, requestId, target }), flight.controller.signal);
    }).then(response => {
      if (this.flights.get(key) !== flight) return;
      this.release(key);
      if (!record(response) || !this.sameFence(response) || response.type !== 'replacement' || response.requestId !== requestId || response.target !== target) {
        if (record(response) && response.requestId === requestId && response.target === target && response.type === 'replacement') this.checkEpoch(response);
        failed(); return;
      }
      try { this.host.send({ type: 'localEdits', message: capture(response) }); } catch { failed(); }
    }, failed);
  }
  private publishLifecycle(event: EditLifecycle & { results?: Array<{ value: unknown }> }): void {
    const pending = this.pending.get(event.requestId);
    if (!pending) return;
    const hasResult = ['accepted', 'confirmed', 'acceptedUnreconciled'].includes(event.state);
    // Decode the worker's accepted evidence, not a mutable host-side last-response slot.
    const result = hasResult ? capture(pending.result((event.results ?? []).map((item, index) =>
      this.manifest.get(pending.operations[index].operation)!.decodeResult(item.value)))) : undefined;
    const published: EditLifecycle = Object.freeze({ ...this.fence, requestId: event.requestId, state: event.state, sequence: event.sequence, commitRevision: event.commitRevision, code: event.code, ...(event.quarantined ? { quarantined: true } : {}), ...(hasResult ? { result } : {}) });
    pending.latest = published;
    if (event.state === 'confirmed') pending.resolve({ kind: 'confirmed', result, commitRevision: event.commitRevision });
    if (event.state === 'acceptedUnreconciled') pending.resolve({ kind: 'acceptedUnreconciled', result, commitRevision: event.commitRevision! });
    if (event.state === 'rejected' || event.state === 'outcomeUnknown') pending.resolve({ kind: event.state, code: event.code });
    if (['accepted', 'confirmed', 'rejected', 'outcomeUnknown', 'acceptedUnreconciled'].includes(event.state)) this.release(`write:${event.requestId}`);
    for (const listener of [...pending.listeners]) notify(() => listener(published));
    for (const listener of [...this.lifecycle]) notify(() => listener(published));
    if (['confirmed', 'rejected', 'acceptedUnreconciled'].includes(event.state)) { this.pending.delete(event.requestId); pending.listeners.clear(); }
  }
  private emitFailure(event: LocalEditsFailure): void {
    const safe = Object.freeze({ ...this.fence, requestId: event.requestId, phase: event.phase, code: event.code,
      ...(event.certainty === undefined ? {} : { certainty: event.certainty }),
      ...(event.operationIndex === undefined ? {} : { operationIndex: event.operationIndex }),
    }) as LocalEditsFailure;
    for (const listener of [...this.failures]) notify(() => listener(safe));
  }
  private sameFence(value: object): boolean { return Object.entries(this.fence).every(([key, v]) => (value as any)[key] === v); }
  private checkEpoch(value: Record<string, unknown>): void {
    if (typeof value.databaseEpoch === 'string' && value.databaseEpoch && value.databaseEpoch !== this.fence.databaseEpoch
      && Object.entries(this.fence).every(([key, v]) => key === 'databaseEpoch' || value[key] === v)) {
      this.closing = true;
      this.send({ type: 'reset' });
    }
  }
  private send(message: object): void { if (this.active) this.host.send({ type: 'localEdits', message: { ...this.fence, ...message } }); }
  private release(key: string): void {
    const flight = this.flights.get(key);
    if (!flight) return;
    this.flights.delete(key);
    clearTimeout(flight.timer);
    flight.controller.abort();
    notify(() => flight.prepared?.dispose?.());
  }
  dispose(): Promise<void> {
    if (!this.active || this.closing) return this.ended;
    this.closing = true;
    if (!this.started) this.detach();
    else this.send({ type: 'dispose' });
    return this.ended;
  }
  private detach(): void {
    this.active = false;
    for (const key of this.flights.keys()) this.release(key);
    notify(() => this.hintsCleanup?.());
    globalThis.removeEventListener?.('online', this.online);
    globalThis.removeEventListener?.('offline', this.offline);
    this.pending.forEach(p => p.listeners.clear());
    this.pending.clear();
    this.failures.clear();
    this.lifecycle.clear();
    notify(() => this.host.end?.());
    this.resolveEnded();
  }
}
function record(value: unknown): value is Record<string, any> { return value !== null && typeof value === 'object' && !Array.isArray(value); }
function revision(value: unknown): boolean { return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0; }
function notify(callback: () => void): void { try { callback(); } catch (error) { console.error('[PyreClient] Local edit listener failed', error); } }
/** Strict JSON snapshot: do not silently erase undefined, nonfinite numbers, or class instances. */
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
