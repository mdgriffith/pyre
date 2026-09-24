export type EphemeralFreshness =
  | { status: 'disconnected'; stale: true }
  | { status: 'connecting'; stale: true }
  | { status: 'live'; stale: false }
  | { status: 'resyncing'; stale: true }
  | { status: 'error'; stale: true; error: string };

export interface EphemeralStateTypeBundle {
  connection: object;
  connectionPatch: object;
  shared: object;
  sharedPatch: object;
}

export interface EphemeralAuthority<Connection, Shared> {
  shared: Shared | null;
  connections: Readonly<Record<string, Connection>>;
  epoch: string | null;
  revision: number | null;
  connectionId: string | null;
  freshness: EphemeralFreshness;
}

export interface EphemeralAccepted {
  status: 'accepted';
  operation: string;
  clientRequestSequence: number;
  ephemeralEpoch: string;
  revision: number;
}

export interface EphemeralRejected {
  status: 'rejected';
  clientRequestSequence: number | null;
  error: string;
}

export interface EphemeralUnknown {
  status: 'unknown';
  clientRequestSequence: number | null;
  error: string;
}

export type EphemeralUpdateOutcome = EphemeralAccepted | EphemeralRejected | EphemeralUnknown;

export interface EphemeralStateSnapshot<Connection, ConnectionPatch, Shared, SharedPatch> {
  authoritative: EphemeralAuthority<Connection, Shared>;
  desired: {
    connection: Readonly<ConnectionPatch>;
    shared: Readonly<SharedPatch>;
  };
  latestOutcome: EphemeralUpdateOutcome | null;
}

export type BoundEphemeralStateSnapshot<Types extends EphemeralStateTypeBundle> = EphemeralStateSnapshot<
  Types['connection'],
  Types['connectionPatch'],
  Types['shared'],
  Types['sharedPatch']
>;

export class EphemeralUpdateError extends Error {
  readonly outcome: EphemeralRejected | EphemeralUnknown;

  constructor(outcome: EphemeralRejected | EphemeralUnknown) {
    super(outcome.error);
    this.name = 'EphemeralUpdateError';
    this.outcome = outcome;
  }
}

interface EphemeralSnapshotMessage {
  databaseId: string;
  epoch: string;
  revision: number;
  shared?: unknown;
  connections: Record<string, unknown>;
}

interface EphemeralChangesMessage {
  databaseId: string;
  epoch: string;
  revision: number;
  shared?: unknown;
  connections?: Record<string, unknown>;
  removedConnections?: string[];
}

interface Completion {
  resolve: (accepted: EphemeralAccepted) => void;
  reject: (error: EphemeralUpdateError) => void;
}

interface Channel {
  desired: Record<string, unknown>;
  fieldVersions: Record<string, number>;
  queued: Record<string, unknown>;
  queuedFieldVersions: Record<string, number>;
  queuedCompletions: Completion[];
  timer: ReturnType<typeof setTimeout> | null;
  lastSentAt: number | null;
}

interface InFlight {
  controller: AbortController;
  completions: Completion[];
  generation: number;
  sequence: number;
}

export interface EphemeralStateServiceConfig {
  databaseId: string;
  baseUrl: string;
  endpoints: {
    connection: string;
    shared: string;
    lease: string;
    resnapshot: string;
  };
  ephemeralWrite?: boolean;
  maxUpdateCadenceMs?: number;
  leaseCadenceMs?: number;
  credentials?: RequestCredentials;
  resolveHeaders?: () => Promise<Record<string, string>>;
  fetch?: typeof fetch;
  now?: () => number;
  setTimeout?: typeof setTimeout;
  clearTimeout?: typeof clearTimeout;
}

type ChannelName = 'connection' | 'shared';

const MAX_SAFE_SEQUENCE = Number.MAX_SAFE_INTEGER;

export class EphemeralStateService<
  Connection extends object = Record<string, unknown>,
  ConnectionPatch extends object = Partial<Connection>,
  Shared extends object = Record<string, unknown>,
  SharedPatch extends object = Partial<Shared>,
> {
  private readonly config: Required<Pick<EphemeralStateServiceConfig,
    'ephemeralWrite' | 'maxUpdateCadenceMs' | 'leaseCadenceMs' | 'credentials'>>
    & EphemeralStateServiceConfig;
  private readonly fetchImpl: typeof fetch;
  private readonly now: () => number;
  private readonly setTimer: typeof setTimeout;
  private readonly clearTimer: typeof clearTimeout;
  private readonly callbacks = new Set<(snapshot: EphemeralStateSnapshot<Connection, ConnectionPatch, Shared, SharedPatch>) => void>();
  private readonly channels: Record<ChannelName, Channel> = {
    connection: this.newChannel(),
    shared: this.newChannel(),
  };
  private readonly inFlight = new Set<InFlight>();
  private authority: EphemeralAuthority<Connection, Shared> = {
    shared: null,
    connections: {},
    epoch: null,
    revision: null,
    connectionId: null,
    freshness: { status: 'disconnected', stale: true },
  };
  private latestOutcome: EphemeralUpdateOutcome | null = null;
  private latestOutcomeSequence = -1;
  private generation = 0;
  private sequence = 0;
  private leaseTimer: ReturnType<typeof setTimeout> | null = null;
  private resnapshotInFlight = false;
  private disposed = false;

  constructor(config: EphemeralStateServiceConfig) {
    validateConfig(config);
    this.config = {
      ...config,
      ephemeralWrite: config.ephemeralWrite ?? true,
      maxUpdateCadenceMs: config.maxUpdateCadenceMs ?? 50,
      leaseCadenceMs: config.leaseCadenceMs ?? 10_000,
      credentials: config.credentials ?? 'same-origin',
    };
    const fetchImpl = config.fetch ?? globalThis.fetch;
    this.fetchImpl = (...args) => fetchImpl(...args);
    this.now = config.now ?? Date.now;
    const setTimer = config.setTimeout ?? globalThis.setTimeout;
    const clearTimer = config.clearTimeout ?? globalThis.clearTimeout;
    // Browser timer functions require a valid host call boundary in Chromium.
    this.setTimer = setTimer.bind(globalThis);
    this.clearTimer = clearTimer.bind(globalThis);
  }

  getSnapshot(): EphemeralStateSnapshot<Connection, ConnectionPatch, Shared, SharedPatch> {
    return {
      authoritative: {
        ...this.authority,
        connections: { ...this.authority.connections },
        freshness: { ...this.authority.freshness },
      },
      desired: {
        connection: captureJsonValue(this.channels.connection.desired, 'Ephemeral desired state', new Set()) as ConnectionPatch,
        shared: captureJsonValue(this.channels.shared.desired, 'Ephemeral desired state', new Set()) as SharedPatch,
      },
      latestOutcome: this.latestOutcome ? { ...this.latestOutcome } : null,
    };
  }

  subscribe(callback: (snapshot: EphemeralStateSnapshot<Connection, ConnectionPatch, Shared, SharedPatch>) => void): () => void {
    this.callbacks.add(callback);
    callback(this.getSnapshot());
    return () => this.callbacks.delete(callback);
  }

  updateConnection(patch: ConnectionPatch): Promise<EphemeralAccepted> {
    return this.update('connection', patch);
  }

  updateShared(patch: SharedPatch): Promise<EphemeralAccepted> {
    return this.update('shared', patch);
  }

  transportConnecting(): void {
    if (this.disposed) return;
    if (this.authority.freshness.status === 'connecting') return;
    this.setFreshness({ status: 'connecting', stale: true });
  }

  transportDisconnected(error = 'Ephemeral transport disconnected'): void {
    if (this.disposed) return;
    if (this.authority.freshness.status === 'disconnected') return;
    this.endGeneration(error);
    this.authority = {
      ...this.authority,
      connectionId: null,
      freshness: { status: 'disconnected', stale: true },
    };
    this.emit();
  }

  handleMessage(message: unknown): void {
    if (this.disposed || !isRecord(message) || typeof message.type !== 'string') return;
    if (message.type === 'connected') {
      this.handleConnected(message);
    } else if (message.type === 'ephemeralSnapshot') {
      const snapshot = parseSnapshot(message.ephemeralSnapshot);
      if (snapshot) this.installSnapshot(snapshot, false);
    } else if (message.type === 'ephemeralChanges') {
      const changes = parseChanges(message.ephemeralChanges);
      if (changes) this.installChanges(changes);
    } else if (message.type === 'ephemeralResyncRequired') {
      this.handleResyncRequired(message.ephemeralResyncRequired);
    }
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.endGeneration('Ephemeral state service disposed');
    this.callbacks.clear();
  }

  private newChannel(): Channel {
    return {
      desired: emptyRecord(), fieldVersions: emptyRecord(), queued: emptyRecord(), queuedFieldVersions: emptyRecord(),
      queuedCompletions: [], timer: null, lastSentAt: null,
    };
  }

  private update(channelName: ChannelName, patch: object): Promise<EphemeralAccepted> {
    if (this.disposed) {
      return Promise.reject(new EphemeralUpdateError(this.unknown(null, 'Ephemeral state service is disposed')));
    }
    if (!this.config.ephemeralWrite) {
      const outcome = this.rejected(null, 'Ephemeral state is read-only');
      this.publishOutcome(outcome);
      return Promise.reject(new EphemeralUpdateError(outcome));
    }
    let captured: Record<string, unknown>;
    try {
      captured = captureJsonObject(patch, 'Ephemeral patch');
    } catch (error) {
      return Promise.reject(error);
    }

    const channel = this.channels[channelName];
    const fieldVersions: Record<string, number> = {};
    Object.assign(channel.desired, captured);
    Object.keys(captured).forEach((field) => {
      const version = (channel.fieldVersions[field] ?? 0) + 1;
      channel.fieldVersions[field] = version;
      fieldVersions[field] = version;
    });
    this.emit();

    if (!this.identity()
      || this.authority.freshness.status === 'connecting'
      || this.authority.freshness.status === 'disconnected'
      || this.authority.freshness.status === 'error') {
      const outcome = this.unknown(null, 'Ephemeral update was not sent because the transport is not live');
      this.publishOutcome(outcome);
      return Promise.reject(new EphemeralUpdateError(outcome));
    }

    Object.assign(channel.queued, captured);
    Object.assign(channel.queuedFieldVersions, fieldVersions);
    const promise = new Promise<EphemeralAccepted>((resolve, reject) => {
      channel.queuedCompletions.push({ resolve, reject });
    });
    this.schedule(channelName);
    return promise;
  }

  private handleConnected(message: Record<string, unknown>): void {
    if (message.databaseId !== this.config.databaseId
      || typeof message.connectionId !== 'string'
      || typeof message.ephemeralEpoch !== 'string') return;

    this.endGeneration('Ephemeral connection identity changed');
    this.authority = {
      shared: null,
      connections: {},
      epoch: message.ephemeralEpoch,
      revision: null,
      connectionId: message.connectionId,
      freshness: { status: 'connecting', stale: true },
    };
    this.emit();
  }

  private installSnapshot(snapshot: EphemeralSnapshotMessage, recovery: boolean): void {
    if (!this.matchesCurrentIdentity(snapshot)
      || (!recovery && this.authority.freshness.status !== 'connecting')
      || (recovery && this.authority.freshness.status !== 'resyncing')) return;
    this.authority = {
      shared: (snapshot.shared ?? null) as Shared | null,
      connections: { ...snapshot.connections } as Record<string, Connection>,
      epoch: snapshot.epoch,
      revision: snapshot.revision,
      connectionId: this.authority.connectionId,
      freshness: { status: 'live', stale: false },
    };
    this.resnapshotInFlight = false;
    this.scheduleLease();
    this.emit();

    if (!recovery && this.config.ephemeralWrite && Object.keys(this.channels.connection.desired).length > 0) {
      Object.assign(this.channels.connection.queued, this.channels.connection.desired);
      Object.assign(this.channels.connection.queuedFieldVersions, this.channels.connection.fieldVersions);
    }
    this.schedule('connection');
    this.schedule('shared');
  }

  private installChanges(changes: EphemeralChangesMessage): void {
    if (!this.matchesCurrentIdentity(changes)
      || this.authority.revision === null
      || changes.revision <= this.authority.revision
      || this.authority.freshness.status === 'resyncing') return;

    const connections = { ...this.authority.connections } as Record<string, Connection>;
    for (const [connectionId, value] of Object.entries(changes.connections ?? {})) {
      connections[connectionId] = value as Connection;
    }
    for (const connectionId of changes.removedConnections ?? []) {
      delete connections[connectionId];
    }
    this.authority = {
      ...this.authority,
      shared: Object.prototype.hasOwnProperty.call(changes, 'shared')
        ? (changes.shared as Shared)
        : this.authority.shared,
      connections,
      revision: changes.revision,
    };
    this.emit();
  }

  private handleResyncRequired(value: unknown): void {
    if (!isRecord(value)
      || value.databaseId !== this.config.databaseId
      || value.epoch !== this.authority.epoch
      || !isSafeRevision(value.revision)
      || this.authority.revision === null
      || value.revision <= this.authority.revision
      || this.resnapshotInFlight) return;

    this.resnapshotInFlight = true;
    this.setFreshness({ status: 'resyncing', stale: true });
    void this.requestResnapshot();
  }

  private async requestResnapshot(): Promise<void> {
    const generation = this.generation;
    const result = await this.request('resnapshot', this.config.endpoints.resnapshot, undefined, generation);
    if (generation !== this.generation || this.disposed) return;
    if (result.status === 'accepted' && isRecord(result.raw.ephemeralSnapshot)) {
      const snapshot = parseSnapshot(result.raw.ephemeralSnapshot);
      if (snapshot) {
        this.installSnapshot(snapshot, true);
        return;
      }
    }
    this.resnapshotInFlight = false;
    if (result.outcome) this.publishOutcome(result.outcome);
    this.failLive(result.outcome?.error ?? 'Ephemeral resnapshot returned an invalid response');
  }

  private schedule(channelName: ChannelName): void {
    const channel = this.channels[channelName];
    if (channel.timer || Object.keys(channel.queued).length === 0 || !this.isLive()) return;
    const elapsed = channel.lastSentAt === null ? Infinity : this.now() - channel.lastSentAt;
    const delay = Math.max(0, this.config.maxUpdateCadenceMs - elapsed);
    channel.timer = this.setTimer(() => {
      channel.timer = null;
      void this.flush(channelName);
    }, delay);
  }

  private async flush(channelName: ChannelName): Promise<void> {
    const channel = this.channels[channelName];
    if (!this.isLive() || Object.keys(channel.queued).length === 0) return;
    const patch = channel.queued;
    const fieldVersions = channel.queuedFieldVersions;
    const completions = channel.queuedCompletions;
    channel.queued = emptyRecord();
    channel.queuedFieldVersions = emptyRecord();
    channel.queuedCompletions = [];
    channel.lastSentAt = this.now();
    const generation = this.generation;
    const endpoint = channelName === 'connection' ? this.config.endpoints.connection : this.config.endpoints.shared;
    const result = await this.request(`${channelName}Patch`, endpoint, patch, generation, completions);
    if (generation !== this.generation || this.disposed) return;
    if (result.status === 'accepted') {
      completions.forEach(({ resolve }) => resolve(result.accepted));
      if (this.versionsAreCurrent(channel, fieldVersions)) this.publishOutcome(result.accepted);
    } else if (result.outcome) {
      completions.forEach(({ reject }) => reject(new EphemeralUpdateError(result.outcome!)));
      if (this.versionsAreCurrent(channel, fieldVersions)) this.publishOutcome(result.outcome);
    }
    this.schedule(channelName);
  }

  private async request(
    operation: string,
    path: string,
    patch: Record<string, unknown> | undefined,
    generation: number,
    completions: Completion[] = [],
  ): Promise<
    | { status: 'accepted'; accepted: EphemeralAccepted; raw: Record<string, unknown>; outcome?: never }
    | { status: 'failed'; outcome: EphemeralRejected | EphemeralUnknown; raw?: never }
  > {
    const identity = this.identity();
    if (!identity) return { status: 'failed', outcome: this.unknown(null, `${operation} was not sent without a live identity`) };
    const sequence = this.nextSequence();
    if (sequence === null) return { status: 'failed', outcome: this.unknown(null, 'Ephemeral request sequence exhausted') };
    const controller = new AbortController();
    const tracked: InFlight = { controller, completions, generation, sequence };
    this.inFlight.add(tracked);
    try {
      const resolvedHeaders = await this.config.resolveHeaders?.() ?? {};
      if (generation !== this.generation || this.disposed) {
        return { status: 'failed', outcome: this.unknown(sequence, `${operation} outcome is unknown after connection change`) };
      }
      const headers = new Headers(resolvedHeaders);
      headers.set('content-type', 'application/json');
      const response = await this.fetchImpl(resolveUrl(this.config.baseUrl, path), {
        method: patch === undefined ? 'POST' : 'PATCH',
        headers,
        credentials: this.config.credentials,
        signal: controller.signal,
        body: JSON.stringify({
          databaseId: this.config.databaseId,
          ephemeralEpoch: identity.epoch,
          connectionId: identity.connectionId,
          clientRequestSequence: sequence,
          ...(patch === undefined ? {} : { patch }),
        }),
      });
      let raw: unknown;
      try {
        raw = await response.json();
      } catch (error) {
        return {
          status: 'failed',
          outcome: this.unknown(sequence, `${operation} returned a non-JSON response (HTTP ${response.status}): ${errorMessage(error)}`),
        };
      }
      if (generation !== this.generation || this.disposed) {
        return { status: 'failed', outcome: this.unknown(sequence, `${operation} outcome is unknown after connection change`) };
      }
      if (isRecord(raw) && raw.type === 'ephemeralRejected'
        && raw.clientRequestSequence === sequence && typeof raw.error === 'string') {
        return { status: 'failed', outcome: this.rejected(sequence, raw.error) };
      }
      if (response.ok && isRecord(raw) && raw.type === 'ephemeralAccepted'
        && raw.clientRequestSequence === sequence
        && raw.ephemeralEpoch === identity.epoch
        && raw.operation === operation
        && isSafeRevision(raw.revision)) {
        return {
          status: 'accepted',
          accepted: {
            status: 'accepted', operation: raw.operation, clientRequestSequence: sequence,
            ephemeralEpoch: raw.ephemeralEpoch, revision: raw.revision,
          },
          raw,
        };
      }
      return {
        status: 'failed',
        outcome: this.unknown(sequence, `${operation} returned an invalid response (HTTP ${response.status})`),
      };
    } catch (error) {
      return { status: 'failed', outcome: this.unknown(sequence, `${operation} outcome is unknown: ${errorMessage(error)}`) };
    } finally {
      this.inFlight.delete(tracked);
    }
  }

  private scheduleLease(): void {
    if (this.leaseTimer) this.clearTimer(this.leaseTimer);
    if (!this.isLive()) return;
    const generation = this.generation;
    this.leaseTimer = this.setTimer(() => {
      this.leaseTimer = null;
      void this.renewLease(generation);
    }, this.config.leaseCadenceMs);
  }

  private async renewLease(generation: number): Promise<void> {
    if (generation !== this.generation || !this.isLive()) return;
    const result = await this.request('leaseRefresh', this.config.endpoints.lease, undefined, generation);
    if (generation !== this.generation || this.disposed) return;
    if (result.status === 'accepted') {
      this.scheduleLease();
      return;
    }
    this.publishOutcome(result.outcome);
    this.failLive(`Ephemeral lease lost: ${result.outcome.error}`);
  }

  private failLive(error: string): void {
    this.endGeneration(error);
    this.authority = {
      ...this.authority,
      connectionId: null,
      freshness: { status: 'error', stale: true, error },
    };
    this.emit();
  }

  private endGeneration(reason: string): void {
    this.generation += 1;
    if (this.leaseTimer) this.clearTimer(this.leaseTimer);
    this.leaseTimer = null;
    this.resnapshotInFlight = false;
    let hadPending = false;
    for (const channel of Object.values(this.channels)) {
      if (channel.timer) this.clearTimer(channel.timer);
      channel.timer = null;
      channel.queued = emptyRecord();
      channel.queuedFieldVersions = emptyRecord();
      const outcome = this.unknown(null, reason);
      hadPending ||= channel.queuedCompletions.length > 0;
      channel.queuedCompletions.forEach(({ reject }) => reject(new EphemeralUpdateError(outcome)));
      channel.queuedCompletions = [];
      channel.lastSentAt = null;
    }
    for (const request of this.inFlight) {
      request.controller.abort();
      const outcome = this.unknown(request.sequence, reason);
      request.completions.forEach(({ reject }) => reject(new EphemeralUpdateError(outcome)));
      hadPending ||= request.completions.length > 0;
    }
    this.inFlight.clear();
    if (hadPending) this.latestOutcome = this.unknown(null, reason);
  }

  private matchesCurrentIdentity(value: { databaseId: string; epoch: string }): boolean {
    return value.databaseId === this.config.databaseId
      && value.epoch === this.authority.epoch
      && this.authority.connectionId !== null;
  }

  private identity(): { epoch: string; connectionId: string } | null {
    return this.authority.epoch && this.authority.connectionId
      ? { epoch: this.authority.epoch, connectionId: this.authority.connectionId }
      : null;
  }

  private isLive(): boolean {
    return this.authority.freshness.status === 'live' && this.identity() !== null;
  }

  private versionsAreCurrent(channel: Channel, versions: Record<string, number>): boolean {
    return Object.entries(versions).every(([field, version]) => channel.fieldVersions[field] === version);
  }

  private nextSequence(): number | null {
    if (this.sequence >= MAX_SAFE_SEQUENCE) return null;
    this.sequence += 1;
    return this.sequence;
  }

  private setFreshness(freshness: EphemeralFreshness): void {
    this.authority = { ...this.authority, freshness };
    this.emit();
  }

  private publishOutcome(outcome: EphemeralUpdateOutcome): void {
    if (outcome.clientRequestSequence !== null
      && outcome.clientRequestSequence < this.latestOutcomeSequence) return;
    if (outcome.clientRequestSequence !== null) {
      this.latestOutcomeSequence = outcome.clientRequestSequence;
    }
    this.latestOutcome = outcome;
    this.emit();
  }

  private rejected(sequence: number | null, error: string): EphemeralRejected {
    return { status: 'rejected', clientRequestSequence: sequence, error };
  }

  private unknown(sequence: number | null, error: string): EphemeralUnknown {
    return { status: 'unknown', clientRequestSequence: sequence, error };
  }

  private emit(): void {
    if (this.callbacks.size === 0) return;
    const snapshot = this.getSnapshot();
    this.callbacks.forEach((callback) => callback(snapshot));
  }
}

function parseSnapshot(value: unknown): EphemeralSnapshotMessage | null {
  if (!isRecord(value) || typeof value.databaseId !== 'string' || typeof value.epoch !== 'string'
    || !isSafeRevision(value.revision) || !isRecord(value.connections)) return null;
  return value as unknown as EphemeralSnapshotMessage;
}

function parseChanges(value: unknown): EphemeralChangesMessage | null {
  if (!isRecord(value) || typeof value.databaseId !== 'string' || typeof value.epoch !== 'string'
    || !isSafeRevision(value.revision)
    || (value.connections !== undefined && !isRecord(value.connections))
    || (value.removedConnections !== undefined
      && (!Array.isArray(value.removedConnections) || value.removedConnections.some((id) => typeof id !== 'string')))) return null;
  return value as unknown as EphemeralChangesMessage;
}

function isSafeRevision(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function resolveUrl(baseUrl: string, path: string): string {
  const base = baseUrl.endsWith('/') ? baseUrl : `${baseUrl}/`;
  return new URL(path.replace(/^\//, ''), base).toString();
}

function validateConfig(config: EphemeralStateServiceConfig): void {
  if (typeof config.databaseId !== 'string' || config.databaseId.trim() === '') {
    throw new TypeError('Ephemeral state databaseId must be a non-empty string');
  }
  let base: URL;
  try {
    base = new URL(config.baseUrl);
  } catch {
    throw new TypeError('Ephemeral state baseUrl must be a valid absolute HTTP(S) URL');
  }
  if ((base.protocol !== 'http:' && base.protocol !== 'https:') || !base.host) {
    throw new TypeError('Ephemeral state baseUrl must be a valid absolute HTTP(S) URL');
  }
  for (const [name, path] of Object.entries(config.endpoints)) {
    if (typeof path !== 'string' || path.trim() === '') {
      throw new TypeError(`Ephemeral state ${name} endpoint must be a non-empty HTTP(S) path or URL`);
    }
    try {
      const endpoint = new URL(path.replace(/^\//, ''), `${base.toString().replace(/\/?$/, '/')}`);
      if (endpoint.protocol !== 'http:' && endpoint.protocol !== 'https:') throw new Error();
    } catch {
      throw new TypeError(`Ephemeral state ${name} endpoint must be a valid HTTP(S) path or URL`);
    }
  }
  validateCadence(config.maxUpdateCadenceMs, 'maxUpdateCadenceMs', true);
  validateCadence(config.leaseCadenceMs, 'leaseCadenceMs', false);
}

function validateCadence(value: number | undefined, name: string, allowZero: boolean): void {
  if (value === undefined) return;
  if (!Number.isFinite(value) || (allowZero ? value < 0 : value <= 0)) {
    throw new TypeError(`Ephemeral state ${name} must be a finite ${allowZero ? 'non-negative' : 'positive'} number`);
  }
}

function captureJsonObject(value: unknown, label: string): Record<string, unknown> {
  if (!isRecord(value) || Object.keys(value).length === 0) {
    throw new TypeError('Ephemeral patches must contain at least one top-level field');
  }
  return captureJsonValue(value, label, new Set()) as Record<string, unknown>;
}

function captureJsonValue(value: unknown, path: string, seen: Set<object>): unknown {
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return value;
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) throw new TypeError(`${path} must contain only JSON-safe values`);
    return value;
  }
  if (typeof value === 'bigint') {
    const normalized = Number(value);
    if (!Number.isFinite(normalized)) throw new TypeError(`${path} must contain only JSON-safe values`);
    return normalized;
  }
  if (value instanceof Date) {
    if (Number.isNaN(value.getTime())) throw new TypeError(`${path} contains an invalid Date`);
    return value.toISOString();
  }
  if (ArrayBuffer.isView(value)) {
    return Array.from(new Uint8Array(value.buffer, value.byteOffset, value.byteLength));
  }
  if (value instanceof ArrayBuffer) return Array.from(new Uint8Array(value));
  if (typeof value !== 'object' || value === undefined) {
    throw new TypeError(`${path} must contain only JSON-safe values`);
  }
  if (seen.has(value)) throw new TypeError(`${path} must not contain circular references`);
  seen.add(value);
  try {
    if (Array.isArray(value)) {
      return value.map((entry, index) => captureJsonValue(entry, `${path}[${index}]`, seen));
    }
    const output: Record<string, unknown> = {};
    for (const key of Reflect.ownKeys(value)) {
      if (!Object.prototype.propertyIsEnumerable.call(value, key)) continue;
      if (typeof key !== 'string') throw new TypeError(`${path} must not contain enumerable symbol keys`);
      Object.defineProperty(output, key, {
        configurable: true,
        enumerable: true,
        writable: true,
        value: captureJsonValue((value as Record<string, unknown>)[key], `${path}.${key}`, seen),
      });
    }
    return output;
  } finally {
    seen.delete(value);
  }
}

function emptyRecord<T = unknown>(): Record<string, T> {
  return Object.create(null) as Record<string, T>;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
