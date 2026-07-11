export type SqlInfo = {
  include: boolean;
  params: string[];
  sql: string;
};

export type SqlStatement = { sql: string; args: Record<string, any> };

export function toSessionArgs(sessionArgs: string[], session: Record<string, unknown>): Record<string, unknown> {
  const result: Record<string, unknown> = {};

  if (session == null) {
    return result;
  }

  for (const key of sessionArgs) {
    if (key in session) {
      result[`session_${key}`] = normalizeSqlArg(session[key]);
    }
  }

  return result;
}

function normalizeSqlArg(value: unknown): unknown {
  if (value instanceof Date) {
    return Math.floor(value.getTime() / 1000);
  }

  return value;
}

export function buildArgs(
  input: Record<string, unknown> | undefined,
  session: Record<string, unknown>,
  sessionArgs: string[],
  optionalInputArgs: string[] = [],
  jsonInputArgs: string[] = []
): Record<string, unknown> {
  const args: Record<string, unknown> = {};
  const jsonInputArgSet = new Set(jsonInputArgs);

  for (const key of optionalInputArgs) {
    args[`${key}__is_set`] = false;
  }

  if (input) {
    for (const [key, value] of Object.entries(input)) {
      if (value !== undefined) {
        args[key] = jsonInputArgSet.has(key) ? JSON.stringify(value) : normalizeSqlArg(value);
        if (optionalInputArgs.includes(key)) {
          args[`${key}__is_set`] = true;
        }
      }
    }
  }

  Object.assign(args, toSessionArgs(sessionArgs, session));

  return args;
}

export function toSqlStatements(sql: SqlInfo[], args: Record<string, unknown>): SqlStatement[] {
  return sql.map(({ sql: statement, params }) => {
    const filtered: Record<string, any> = {};
    for (const key of params) {
      if (key in args) {
        filtered[key] = args[key];
      }
    }

    return { sql: statement, args: filtered };
  });
}

export function formatResultData(sql: SqlInfo[], resultSets: unknown[]): Record<string, unknown> {
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
      if (colName.startsWith('_')) {
        continue;
      }
      if (!(colName in formatted)) {
        formatted[colName] = [];
      }
      for (const row of resultSet.rows || []) {
        if (colName in row && typeof row[colName] === 'string') {
          const parsed: unknown = JSON.parse(row[colName]);
          if (Array.isArray(parsed)) {
            formatted[colName] = parsed;
          } else {
            const existing = formatted[colName];
            formatted[colName] = Array.isArray(existing) ? [...existing, parsed] : [parsed];
          }
        }
      }
    }
  }
  return formatted;
}
