export type PyreLogActor = 'APP' | 'IDB' | 'SERVER';
export type PyreLogDirection = '->' | '<-';
export type PyreLogLevel = 'info' | 'error' | 'trace';

export interface PyreLogDescription {
  actor: PyreLogActor;
  direction?: PyreLogDirection;
  operation: string;
  level: PyreLogLevel;
  summary: string;
  payload?: unknown;
}

interface EventDescription {
  actor: PyreLogActor;
  direction?: PyreLogDirection;
  operation: string;
  level?: PyreLogLevel;
}

const debugDescriptions: Record<string, EventDescription> = {
  '[PyreClient] Entity stream initial load started': { actor: 'APP', operation: 'initial-state.started' },
  '[PyreClient] Entity stream initial load finished': { actor: 'APP', operation: 'initial-state.loaded' },
  '[PyreClient] Entity stream emitting initial batch': { actor: 'APP', operation: 'initial-state.applied' },
  '[PyreClient] Entity stream buffering batch during initial load': { actor: 'APP', operation: 'delta.buffered' },
  '[PyreClient] Entity stream draining buffered batch': { actor: 'APP', operation: 'delta.applied' },
  '[PyreClient] Entity stream forwarding batch': { actor: 'APP', operation: 'delta.applied' },
  '[PyreClient] Entity stream IndexedDB snapshot scan started': { actor: 'IDB', operation: 'initial-load.started' },
  '[PyreClient] Entity stream IndexedDB snapshot table loaded': { actor: 'IDB', operation: 'table-load.completed' },
  '[PyreClient] Entity stream IndexedDB snapshot scan finished': { actor: 'IDB', operation: 'initial-load.completed' },
  '[PyreClient] Live sync delta accepted': { actor: 'APP', operation: 'live-delta.accepted' },
  '[PyreClient] IndexedDB initial data request started': { actor: 'IDB', direction: '<-', operation: 'initial-load' },
  '[PyreClient] IndexedDB initial data loaded': { actor: 'IDB', direction: '->', operation: 'initial-load.completed' },
  '[PyreClient] IndexedDB writeDelta table groups received': { actor: 'IDB', direction: '<-', operation: 'delta.persist' },
  '[PyreClient] IndexedDB writeDelta table written': { actor: 'IDB', operation: 'delta.persisted' },
  '[PyreClient] IndexedDB sync cursor written': { actor: 'IDB', operation: 'cursor.persisted' },
  '[PyreClient] IndexedDB server revision written': { actor: 'IDB', operation: 'revision.persisted' },
  '[PyreClient] Elm bridge register entity stream': { actor: 'APP', direction: '<-', operation: 'entity-stream.subscribe' },
  '[PyreClient] Elm bridge forward entity stream batch': { actor: 'APP', direction: '->', operation: 'entity-stream.batch' },
  '[PyreClient] SSE connect requested': { actor: 'APP', direction: '->', operation: 'live.connect' },
  '[PyreClient] SSE connection opened': { actor: 'APP', direction: '<-', operation: 'live.connected' },
  '[PyreClient] SSE connected': { actor: 'APP', direction: '<-', operation: 'live.session' },
  '[PyreClient] SSE disconnect requested': { actor: 'APP', direction: '->', operation: 'live.disconnect' },
};

export function describePyreEvent(type: string, payload?: unknown, databaseId?: string): PyreLogDescription {
  const data = asRecord(payload);
  const payloadDatabaseId = stringValue(data?.databaseId) ?? databaseId;
  const semantic = describeSemanticEvent(type, data);
  const normalizedPayload = payloadDatabaseId && data?.databaseId === undefined
    ? { ...(data ?? {}), databaseId: payloadDatabaseId }
    : payload;

  return {
    ...semantic,
    level: semantic.level ?? 'info',
    summary: formatPyreSummary(semantic, normalizedPayload, payloadDatabaseId),
    payload: normalizedPayload,
  };
}

export function describeDebugCall(args: unknown[], databaseId?: string): PyreLogDescription {
  const label = typeof args[0] === 'string' ? args[0] : 'Debug';
  const detail = args.length === 2 ? args[1] : args.slice(1);
  const description = debugDescriptions[label] ?? {
    actor: label.includes('IndexedDB') ? 'IDB' as const : 'APP' as const,
    operation: debugOperation(label),
    level: 'trace' as const,
  };
  const data = asRecord(detail);
  const payloadDatabaseId = stringValue(data?.databaseId) ?? databaseId;
  const payload = payloadDatabaseId && data?.databaseId === undefined
    ? { ...(data ?? {}), databaseId: payloadDatabaseId }
    : detail;

  return {
    ...description,
    level: description.level ?? 'info',
    summary: formatPyreSummary(description, payload, payloadDatabaseId),
    payload,
  };
}

export function withDatabase(event: PyreLogDescription, databaseId: string): PyreLogDescription {
  const data = asRecord(event.payload);
  const payload = data ? { ...data, databaseId } : { databaseId, detail: event.payload };
  return {
    ...event,
    payload,
    summary: formatPyreSummary(event, payload, databaseId),
  };
}

function describeSemanticEvent(type: string, data: Record<string, unknown> | null): EventDescription {
  if (type.startsWith('sync:')) {
    const messageType = type.slice('sync:'.length);
    if (messageType === 'state') {
      return { actor: 'APP', operation: `sync.${stringValue(data?.status) ?? 'state'}` };
    }
    return { actor: 'APP', direction: '<-', operation: wireOperation(messageType) };
  }
  if (type.startsWith('elm:')) {
    return describeElmEvent(type.slice('elm:'.length));
  }
  if (type.startsWith('query:')) {
    const phase = type.slice('query:'.length).replace('update-input', 'updated');
    return { actor: 'APP', operation: `query.${phase}` };
  }
  if (type.startsWith('mutation:')) {
    return { actor: 'APP', operation: `mutation.${type.slice('mutation:'.length)}` };
  }

  const descriptions: Record<string, EventDescription> = {
    'sync.databases': { actor: 'APP', operation: 'sync.databases' },
    'sync.scheduler': { actor: 'APP', operation: 'sync.scheduled' },
    'sync.database_state': { actor: 'APP', operation: `sync.${stringValue(asRecord(data?.syncState)?.status) ?? 'state'}` },
    'mutation.started': { actor: 'APP', operation: 'mutation.started' },
    'mutation.completed': { actor: 'APP', operation: 'mutation.completed' },
    'mutation.failed': { actor: 'APP', operation: 'mutation.failed', level: 'error' },
    'mutation.custom_dispatched': { actor: 'APP', direction: '->', operation: 'mutation.dispatched' },
    'database.known': { actor: 'APP', operation: 'database.known' },
    'indexeddb:delete': { actor: 'IDB', operation: 'database.deleted' },
    'session:update': { actor: 'APP', operation: 'session.updated' },
    'debug:value': { actor: 'APP', operation: 'debug-value.updated', level: 'trace' },
  };
  return descriptions[type] ?? { actor: 'APP', operation: normalizeOperation(type), level: 'trace' };
}

function describeElmEvent(event: string): EventDescription {
  const descriptions: Record<string, EventDescription> = {
    init: { actor: 'APP', operation: 'runtime.initialized' },
    'sync-control-start': { actor: 'APP', operation: 'sync.started' },
    'live-sync-received': { actor: 'APP', direction: '<-', operation: 'live.message', level: 'trace' },
    'catchup-update': { actor: 'APP', operation: 'catchup.updated' },
    'live-sync-connect': { actor: 'APP', direction: '->', operation: 'live.connect' },
    'live-sync-not-ready': { actor: 'APP', operation: 'live.waiting' },
    'database-epoch-change': { actor: 'APP', operation: 'database.reset' },
  };
  return descriptions[event] ?? { actor: 'APP', operation: normalizeOperation(event), level: 'trace' };
}

function wireOperation(messageType: string): string {
  const operations: Record<string, string> = {
    delta: 'live-delta',
    connected: 'live.connected',
    syncProgress: 'catchup.progress',
    syncComplete: 'sync.completed',
    syncRequired: 'catchup.required',
    catchupRequired: 'catchup.required',
    error: 'live.error',
  };
  return operations[messageType] ?? normalizeOperation(messageType);
}

function formatPyreSummary(description: Pick<EventDescription, 'actor' | 'direction' | 'operation'>, payload: unknown, databaseId?: string): string {
  const parts = ['[pyre]', description.actor];
  if (description.direction) {
    parts.push(description.direction);
  }
  parts.push(description.operation);
  if (databaseId) {
    parts.push(shortDatabaseId(databaseId));
  }
  parts.push(...inlineFields(payload));
  return parts.join(' ');
}

function inlineFields(payload: unknown): string[] {
  const data = asRecord(payload);
  if (!data) {
    return payload === undefined ? [] : [`detail=${formatScalar(payload)}`];
  }

  const liveTableGroups = Array.isArray(data.data) ? data.data.map(asRecord).filter((group): group is Record<string, unknown> => group !== null) : [];
  const derived = liveTableGroups.length > 0
    ? {
      ...data,
      tableGroupCount: data.tableGroupCount ?? liveTableGroups.length,
      rowCount: data.rowCount ?? liveTableGroups.reduce((sum, group) => sum + (Array.isArray(group.rows) ? group.rows.length : 0), 0),
    }
    : data;
  const fields: Array<[string, string[]]> = [
    ['table', ['tableName']],
    ['query', ['queryName', 'queryId']],
    ['mutation', ['mutationName', 'mutationId']],
    ['stream', ['streamId']],
    ['rev', ['serverRevision', 'revision']],
    ['currentRev', ['currentServerRevision', 'currentRevision']],
    ['epoch', ['databaseEpoch', 'toEpoch']],
    ['page', ['page']],
    ['sequence', ['sequence', 'initialSequence']],
    ['tables', ['tableCount', 'tableGroupCount', 'cursorTables', 'touchedTables']],
    ['rows', ['rowCount', 'totalRowCount']],
    ['changes', ['changeCount', 'initialChangeCount']],
    ['received', ['received']],
    ['written', ['written']],
    ['skipped', ['skippedOlder']],
    ['pages', ['pageCount']],
    ['buffered', ['bufferedBatchCount']],
    ['commands', ['dbCmdCount']],
    ['duration', ['elapsedMs']],
    ['more', ['hasMore', 'more']],
    ['status', ['status']],
    ['reason', ['reason']],
    ['error', ['error']],
  ];

  const rendered: string[] = [];
  for (const [label, keys] of fields) {
    const key = keys.find((candidate) => derived[candidate] !== undefined);
    if (!key) {
      continue;
    }
    const value = derived[key];
    if (label === 'duration' && typeof value === 'number') {
      rendered.push(`duration=${value}ms`);
    } else {
      rendered.push(`${label}=${formatScalar(value)}`);
    }
  }
  return rendered;
}

function shortDatabaseId(databaseId: string): string {
  const separator = databaseId.indexOf(':');
  if (separator === -1) {
    return databaseId;
  }
  return `${databaseId.slice(0, separator + 1)}${databaseId.slice(separator + 1, separator + 9)}`;
}

function formatScalar(value: unknown): string {
  if (typeof value === 'string') {
    return /\s/.test(value) ? JSON.stringify(value) : value;
  }
  if (typeof value === 'number' || typeof value === 'boolean' || value === null) {
    return String(value);
  }
  if (Array.isArray(value)) {
    return String(value.length);
  }
  return '{...}';
}

function debugOperation(label: string): string {
  return normalizeOperation(label
    .replace(/^\[(?:PyreClient|QueryClient)\]\s*/, '')
    .replace(/:$/, ''));
}

function normalizeOperation(value: string): string {
  return value
    .replace(/([a-z0-9])([A-Z])/g, '$1-$2')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '.')
    .replace(/^\.+|\.+$/g, '');
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function stringValue(value: unknown): string | undefined {
  return typeof value === 'string' && value !== '' ? value : undefined;
}
