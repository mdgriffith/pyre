export type JsonValue = null | boolean | number | string | JsonValue[] | JsonObject;
export type JsonObject = { [key: string]: JsonValue };

export interface ContextRequest {
  protocolVersion: 1;
  databaseId: string;
}

export type ContextMessage =
  | {
      type: 'context';
      protocolVersion: 1;
      databaseId: string;
      contextId: string;
      schemaId: string;
      cacheScope: string;
      authorityRevision: string;
      databaseEpoch: string;
      session: JsonObject;
      expiresAt: number;
    }
  | {
      type: 'context_invalidated';
      protocolVersion: 1;
      databaseId: string;
      contextId: string;
      reason: 'authority_changed' | 'expired' | 'revoked';
    }
  | {
      type: 'context_error';
      protocolVersion: 1;
      databaseId: string;
      code: 'unauthenticated' | 'denied' | 'unavailable' | 'context_mismatch';
    };

// Validate the JSON boundary without silently dropping or coercing JS values.
function assertJson(value: unknown, ancestors = new Set<object>()): asserts value is JsonValue {
  if (typeof value === 'string') {
    if (/[\uD800-\uDFFF]/u.test(value)) throw new Error('Expected a Unicode scalar string');
    return;
  }
  if (value === null || typeof value === 'boolean') return;
  if (typeof value === 'number' && Number.isFinite(value) && (!Number.isInteger(value) || Number.isSafeInteger(value))) return;
  if (typeof value !== 'object' || value === null) throw new Error('Expected a JSON value');
  const array = Array.isArray(value);
  const prototype = Object.getPrototypeOf(value);
  if (!array && prototype !== Object.prototype && prototype !== null) {
    throw new Error('Expected a plain JSON object');
  }
  if (ancestors.has(value)) throw new Error('Cyclic JSON value');
  ancestors.add(value);
  const keys = Reflect.ownKeys(value).filter(key => !(array && key === 'length'));
  if (array && keys.length !== value.length) throw new Error('Expected a dense JSON array');
  for (const key of keys) {
    if (typeof key !== 'string') throw new Error('Expected a JSON property name');
    assertJson(key);
    if (array && (!/^(0|[1-9][0-9]*)$/.test(key) || Number(key) >= value.length)) {
      throw new Error('Unexpected JSON array property');
    }
    const property = Object.getOwnPropertyDescriptor(value, key)!;
    if (!property.enumerable || !('value' in property)) throw new Error('Expected a JSON data property');
    assertJson(property.value, ancestors);
  }
  ancestors.delete(value);
}

function envelope(value: unknown): JsonObject {
  assertJson(value);
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('Expected a context envelope object');
  }
  if (value.protocolVersion !== 1) throw new Error('Unsupported context protocolVersion');
  identifier(value.databaseId);
  return value;
}

function identifier(value: unknown): void {
  // ECMAScript whitespace includes U+FEFF but omits Unicode White_Space U+0085.
  if (typeof value !== 'string' || /^[\s\u0085]*$/u.test(value)) {
    throw new Error('Expected a nonblank context identifier');
  }
}

function fields(value: JsonObject, expected: string[]): void {
  const keys = Object.keys(value);
  if (keys.length !== expected.length || expected.some(key => !Object.prototype.hasOwnProperty.call(value, key))) {
    throw new Error('Unexpected or missing context envelope fields');
  }
}

/** Validate protocol shape only; this does not authenticate or authorize a request. */
export function parseContextRequest(value: unknown): ContextRequest {
  const request = envelope(value);
  fields(request, ['protocolVersion', 'databaseId']);
  return request as unknown as ContextRequest;
}

/** Validate protocol shape only, not session schema, authority, or current-time expiry. */
export function parseContextMessage(value: unknown): ContextMessage {
  const message = envelope(value);
  switch (message.type) {
    case 'context':
      fields(message, [
        'type', 'protocolVersion', 'databaseId', 'contextId', 'schemaId',
        'cacheScope', 'authorityRevision', 'databaseEpoch', 'session', 'expiresAt',
      ]);
      for (const key of ['contextId', 'schemaId', 'cacheScope', 'authorityRevision', 'databaseEpoch']) {
        identifier(message[key]);
      }
      contextId(message.contextId);
      if (message.session === null || typeof message.session !== 'object' || Array.isArray(message.session)) {
        throw new Error('Expected a session JSON object');
      }
      if (typeof message.expiresAt !== 'number' || !Number.isSafeInteger(message.expiresAt) || message.expiresAt < 0) {
        throw new Error('Expected a nonnegative safe integer expiresAt');
      }
      break;
    case 'context_invalidated':
      fields(message, ['type', 'protocolVersion', 'databaseId', 'contextId', 'reason']);
      contextId(message.contextId);
      if (message.reason !== 'authority_changed' && message.reason !== 'expired' && message.reason !== 'revoked') {
        throw new Error('Unknown context invalidation reason');
      }
      break;
    case 'context_error':
      fields(message, ['type', 'protocolVersion', 'databaseId', 'code']);
      if (message.code !== 'unauthenticated' && message.code !== 'denied' && message.code !== 'unavailable' && message.code !== 'context_mismatch') {
        throw new Error('Unknown context error code');
      }
      break;
    default:
      throw new Error('Unknown context message type');
  }
  return message as unknown as ContextMessage;
}

function contextId(value: unknown): void {
  if (typeof value !== 'string' || !/^[A-Za-z0-9_-]+$/.test(value)) {
    throw new Error('Expected a header-safe contextId');
  }
}
