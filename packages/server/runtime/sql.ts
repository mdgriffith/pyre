import type { Client, ResultSet, Transaction } from "@libsql/client";

export type SqlInfo = {
  include: boolean;
  params: string[];
  sql: string;
};

export type SqlStatement = { sql: string; args: Record<string, any> };

export type JsonSessionValidators = Record<string, { parse(value: unknown): unknown }>;

export interface GeneratedEdit {
  kind: "create" | "update" | "delete";
  writeStatementIndices: number[];
  writableInputs: string[];
}

export class TargetNotWritable extends Error {
  constructor() { super("TargetNotWritable"); }
}

/** Capture direct write cardinality before any later statement can overwrite changes(). */
export async function executeStatements(
  db: Pick<Client, "batch"> | Pick<Transaction, "execute">,
  statements: SqlStatement[],
  generatedEdit?: GeneratedEdit,
): Promise<ResultSet[]> {
  if ("batch" in db) {
    if (generatedEdit) throw new Error("Generated edits require an explicit transaction");
    return db.batch(statements);
  }
  const results: ResultSet[] = [];
  for (let index = 0; index < statements.length; index++) {
    const result = await db.execute(statements[index]);
    if (generatedEdit?.writeStatementIndices.includes(index) && result.columns.length > 0) {
      // The local libsql adapter hardcodes rowsAffected=0 for all result-producing SQL.
      // SQLite changes() counts only this direct write, excluding triggers and cascades.
      const count = await db.execute("select changes() as affected");
      const affected = Number(count.rows[0]?.affected);
      if (!Number.isSafeInteger(affected) || affected < 0) throw new Error("Invalid write count");
      result.rowsAffected = affected;
    }
    results.push(result);
  }
  if (generatedEdit && generatedEdit.writeStatementIndices.reduce((sum, index) => sum + (results[index]?.rowsAffected ?? NaN), 0) !== 1)
    throw new TargetNotWritable();
  return results;
}

export function toSessionArgs(
  sessionArgs: string[],
  session: Record<string, unknown>,
  jsonSessionArgs: string[] = [],
  jsonSessionValidators: JsonSessionValidators = {},
): Record<string, unknown> {
  const result: Record<string, unknown> = {};

  if (session == null) {
    return result;
  }

  for (const key of sessionArgs) {
    const isJson = jsonSessionArgs.includes(key);
    const resolved = resolveSessionArg(key, session, isJson);
    const value = isJson && resolved.value != null && Object.hasOwn(jsonSessionValidators, key)
      ? jsonSessionValidators[key].parse(resolved.value)
      : resolved.value;
    const normalized = normalizeSqlArg(value);
    result[`session_${key}`] =
      normalized !== null && (isJson || typeof normalized === "object")
        ? JSON.stringify(normalized)
        : normalized;
  }

  return result;
}

function resolveSessionArg(
  key: string,
  session: Record<string, unknown>,
  isJson: boolean,
): { value: unknown } {
  let value: unknown = key in session ? session[key] : session;

  if (!(key in session)) {
    for (const part of key.split("__")) {
      if (value === null || typeof value !== "object" || !(part in value)) {
        return { value: null };
      }
      value = (value as Record<string, unknown>)[part];
    }
  }

  if (!isJson && value !== null && typeof value === "object" && "_type" in value) {
    value = (value as Record<string, unknown>)._type;
  }

  return { value };
}

function normalizeSqlArg(value: unknown): unknown {
  if (value instanceof Date) {
    if (Number.isNaN(value.getTime())) {
      throw new Error("Invalid Date SQL argument");
    }
    return Math.floor(value.getTime() / 1000);
  }

  if (Array.isArray(value)) {
    return value.map(normalizeSqlArg);
  }

  if (
    value !== null &&
    typeof value === "object" &&
    !(value instanceof Uint8Array)
  ) {
    return Object.fromEntries(
      Object.entries(value).map(([key, nested]) => [
        key,
        normalizeSqlArg(nested),
      ]),
    );
  }

  return value;
}

export function buildArgs(
  input: Record<string, unknown> | undefined,
  session: Record<string, unknown>,
  sessionArgs: string[],
  optionalInputArgs: string[] = [],
  jsonInputArgs: string[] = [],
  jsonSessionArgs: string[] = [],
  jsonSessionValidators: JsonSessionValidators = {},
): Record<string, unknown> {
  const args: Record<string, unknown> = {};
  const jsonInputArgSet = new Set(jsonInputArgs);

  for (const key of optionalInputArgs) {
    args[`${key}__is_set`] = false;
  }

  if (input) {
    for (const [key, value] of Object.entries(input)) {
      if (value !== undefined) {
        args[key] = jsonInputArgSet.has(key)
          ? value === null
            ? null
            : JSON.stringify(normalizeSqlArg(value))
          : normalizeSqlArg(value);
        if (optionalInputArgs.includes(key)) {
          args[`${key}__is_set`] = true;
        }
      }
    }
  }

  Object.assign(args, toSessionArgs(sessionArgs, session, jsonSessionArgs, jsonSessionValidators));

  return args;
}

export function toSqlStatements(
  sql: SqlInfo[],
  args: Record<string, unknown>,
): SqlStatement[] {
  return sql.map(({ sql: statement, params }) => {
    const filtered: Record<string, any> = {};
    for (const key of params) {
      filtered[key] = key in args ? args[key] : null;
    }

    return { sql: statement, args: filtered };
  });
}

export function formatResultData(
  sql: SqlInfo[],
  resultSets: unknown[],
): Record<string, unknown> {
  const formatted: Record<string, unknown> = {};
  const values = resultSets.filter((_, index) => sql[index]?.include) as Array<{
    columns?: string[];
    rows?: Array<Record<string, unknown>>;
  }>;

  for (const resultSet of values) {
    if (!resultSet?.columns?.length) {
      continue;
    }
    for (const colName of resultSet.columns) {
      if (colName.startsWith("_")) {
        continue;
      }
      if (!(colName in formatted)) {
        formatted[colName] = [];
      }
      for (const row of resultSet.rows || []) {
        if (colName in row && typeof row[colName] === "string") {
          const parsed: unknown = JSON.parse(row[colName]);
          if (Array.isArray(parsed)) {
            formatted[colName] = parsed;
          } else {
            const existing = formatted[colName];
            formatted[colName] = Array.isArray(existing)
              ? [...existing, parsed]
              : [parsed];
          }
        }
      }
    }
  }
  return formatted;
}
