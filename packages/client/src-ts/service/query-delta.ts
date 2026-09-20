export type QueryDeltaEnvelope =
  | {
      type: 'full';
      queryId: string;
      revision: number;
      result: unknown;
    }
  | {
      type: 'delta';
      queryId: string;
      revision: number;
      delta: QueryDelta;
    };

export interface QueryDelta {
  ops: QueryDeltaOp[];
}

export type QueryDeltaOp =
  | {
      op: 'set-row';
      path: string;
      row: Record<string, unknown>;
    }
  | {
      op: 'remove-row';
      path: string;
    }
  | {
      op: 'insert-row';
      path: string;
      index: number;
      row: Record<string, unknown>;
    }
  | {
      op: 'move-row';
      path: string;
      from: number;
      to: number;
    }
  | {
      op: 'remove-row-by-index';
      path: string;
      index: number;
    };

type PathSegment =
  | { kind: 'field'; name: string }
  | { kind: 'index'; index: number }
  | { kind: 'id'; id: string | number };

export interface QueryDeltaError {
  message: string;
  queryId: string;
  op: QueryDeltaOp['op'];
  path?: string;
  details?: string;
}

interface PathParseResult {
  ok: boolean;
  segments?: PathSegment[];
  error?: string;
}

interface UpdateResult {
  value?: unknown;
  error?: string;
}

export function applyQueryDelta(
  queryId: string,
  baseResult: unknown,
  delta: QueryDelta
): { result: unknown; errors: QueryDeltaError[] } {
  let nextResult = baseResult ?? {};
  const errors: QueryDeltaError[] = [];

  for (const op of delta.ops) {
    const parsed = parsePath(op.path);
    if (!parsed.ok || !parsed.segments) {
      errors.push({
        message: 'QueryDelta op failed: invalid path',
        queryId,
        op: op.op,
        path: op.path,
        details: parsed.error,
      });
      continue;
    }

    let update: UpdateResult;
    switch (op.op) {
      case 'set-row':
        if (!isPlainObject(op.row)) {
          errors.push({
            message: 'QueryDelta op failed: row is not an object',
            queryId,
            op: op.op,
            path: op.path,
          });
          continue;
        }
        update = updateAtPath(nextResult, parsed.segments, (current) => {
          if (!isPlainObject(current)) {
            return { error: 'Path did not resolve to a row object' };
          }
          return { value: op.row };
        });
        break;

      case 'remove-row':
        update = updateAtPath(nextResult, parsed.segments, (current) => {
          if (!isPlainObject(current)) {
            return { error: 'Path did not resolve to a row object' };
          }
          return { value: null };
        });
        break;

      case 'insert-row':
        if (!isPlainObject(op.row)) {
          errors.push({
            message: 'QueryDelta op failed: row is not an object',
            queryId,
            op: op.op,
            path: op.path,
          });
          continue;
        }
        update = updateAtPath(nextResult, parsed.segments, (current) => {
          if (!Array.isArray(current)) {
            return { error: 'Path did not resolve to a list' };
          }
          if (!Number.isInteger(op.index) || op.index < 0 || op.index > current.length) {
            return { error: `Index ${op.index} out of bounds` };
          }
          const updated = current.slice();
          updated.splice(op.index, 0, op.row);
          return { value: updated };
        });
        break;

      case 'move-row':
        update = updateAtPath(nextResult, parsed.segments, (current) => {
          if (!Array.isArray(current)) {
            return { error: 'Path did not resolve to a list' };
          }
          if (!Number.isInteger(op.from) || !Number.isInteger(op.to)) {
            return { error: 'Indices must be integers' };
          }
          if (op.from < 0 || op.from >= current.length) {
            return { error: `From index ${op.from} out of bounds` };
          }
          if (op.to < 0 || op.to >= current.length) {
            return { error: `To index ${op.to} out of bounds` };
          }
          const updated = current.slice();
          const [row] = updated.splice(op.from, 1);
          updated.splice(op.to, 0, row);
          return { value: updated };
        });
        break;

      case 'remove-row-by-index':
        update = updateAtPath(nextResult, parsed.segments, (current) => {
          if (!Array.isArray(current)) {
            return { error: 'Path did not resolve to a list' };
          }
          if (!Number.isInteger(op.index) || op.index < 0 || op.index >= current.length) {
            return { error: `Index ${op.index} out of bounds` };
          }
          const updated = current.slice();
          updated.splice(op.index, 1);
          return { value: updated };
        });
        break;

      default:
        update = { error: 'Unknown op' };
    }

    if (update.error) {
      errors.push({
        message: 'QueryDelta op failed',
        queryId,
        op: op.op,
        path: op.path,
        details: update.error,
      });
      continue;
    }

    nextResult = update.value ?? nextResult;
  }

  return { result: nextResult, errors };
}

export function parsePath(path: string): PathParseResult {
  if (!path.startsWith('.')) {
    return { ok: false, error: 'Path must start with .' };
  }
  if (path === '.' || path.endsWith('.')) return { ok: false, error: 'Missing field segment' };

  const segments: PathSegment[] = [];
  let index = 1;

  while (index < path.length) {
    if (path[index] === '.') {
      index += 1;
    }

    const fieldStart = index;
    while (index < path.length) {
      const char = path[index];
      if (char === '.' || char === '[' || (char === '#' && path[index + 1] === '(')) {
        break;
      }
      index += 1;
    }

    if (fieldStart === index) {
      return { ok: false, error: 'Missing field segment' };
    }

    const fieldName = path.slice(fieldStart, index);
    segments.push({ kind: 'field', name: fieldName });

    while (index < path.length) {
      const char = path[index];
      if (char === '.') {
        break;
      }

      if (char === '[') {
        const end = path.indexOf(']', index);
        if (end === -1) {
          return { ok: false, error: 'Unclosed index selector' };
        }
        const indexText = path.slice(index + 1, end);
        if (!/^-?\d+$/.test(indexText)) {
          return { ok: false, error: `Invalid index selector: ${indexText}` };
        }
        if (!Number.isSafeInteger(Number(indexText)) || Number(indexText) < 0) {
          return { ok: false, error: 'Invalid index selector' };
        }
        segments.push({ kind: 'index', index: Number(indexText) });
        index = end + 1;
        continue;
      }

      if (char === '#' && path[index + 1] === '(') {
        const parsedId = parseIdSelector(path, index + 2);
        if (!parsedId.ok || parsedId.id === undefined || parsedId.nextIndex === undefined) {
          return { ok: false, error: parsedId.error ?? 'Invalid id selector' };
        }
        segments.push({ kind: 'id', id: parsedId.id });
        index = parsedId.nextIndex;
        continue;
      }

      return { ok: false, error: `Unexpected path character: ${char}` };
    }
  }

  return { ok: true, segments };
}

function parseIdSelector(path: string, startIndex: number): { ok: boolean; id?: string | number; nextIndex?: number; error?: string } {
  // Quoted JSON strings disambiguate string "1" from integer 1. Legacy bare
  // nonnumeric strings remain accepted for existing query delta producers.
  if (path[startIndex] === '"') {
    let end = startIndex + 1;
    while (end < path.length) {
      if (path[end] === '\\') { end += 2; continue; }
      if (path[end] === '"') {
        if (path[end + 1] !== ')') return { ok: false, error: 'Invalid string selector' };
        try {
          return { ok: true, id: JSON.parse(path.slice(startIndex, end + 1)), nextIndex: end + 2 };
        } catch {
          return { ok: false, error: 'Invalid string selector' };
        }
      }
      end += 1;
    }
    return { ok: false, error: 'Unclosed string selector' };
  }
  let index = startIndex;
  let raw = '';

  while (index < path.length) {
    const char = path[index];
    if (char === '\\') {
      const next = path[index + 1];
      if (next === undefined) {
        return { ok: false, error: 'Invalid escape sequence in id selector' };
      }
      raw += next;
      index += 2;
      continue;
    }

    if (char === ')') {
      index += 1;
      break;
    }

    raw += char;
    index += 1;
  }

  if (index > path.length || path[index - 1] !== ')') {
    return { ok: false, error: 'Unclosed id selector' };
  }

  const id = /^-?\d+$/.test(raw) ? Number(raw) : raw;
  if (raw.length === 0 || (typeof id === 'number' && !Number.isSafeInteger(id))) {
    return { ok: false, error: 'Invalid id selector' };
  }
  return { ok: true, id, nextIndex: index };
}

function updateAtPath(
  value: unknown,
  segments: PathSegment[],
  updater: (current: unknown) => UpdateResult
): UpdateResult {
  if (segments.length === 0) {
    return updater(value);
  }

  const [segment, ...rest] = segments;

  if (segment.kind === 'field') {
    if (!isPlainObject(value)) {
      return { error: `Expected object for field "${segment.name}"` };
    }
    if (!(segment.name in value)) {
      return { error: `Missing field "${segment.name}"` };
    }
    const current = value[segment.name];
    const updated = updateAtPath(current, rest, updater);
    if (updated.error) {
      return updated;
    }
    if (updated.value === current) {
      return { value };
    }
    return { value: { ...value, [segment.name]: updated.value } };
  }

  if (!Array.isArray(value)) {
    return { error: 'Expected list for selector' };
  }

  const list = value;
  const index = segment.kind === 'index' ? segment.index : findRowIndexById(list, segment.id);
  if (index === null) {
    return { error: 'Row id not found in list' };
  }
  if (index < 0 || index >= list.length) {
    return { error: `Index ${index} out of bounds` };
  }

  const current = list[index];
  const updated = updateAtPath(current, rest, updater);
  if (updated.error) {
    return updated;
  }
  if (updated.value === current) {
    return { value };
  }

  const nextList = list.slice();
  nextList[index] = updated.value;
  return { value: nextList };
}

function findRowIndexById(list: unknown[], id: string | number): number | null {
  for (let i = 0; i < list.length; i += 1) {
    const row = list[i];
    if (!isPlainObject(row)) {
      continue;
    }
    const rowId = (row as Record<string, unknown>).id;
    if (id === rowId) {
      return i;
    }
  }
  return null;
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
