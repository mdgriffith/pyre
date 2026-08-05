import * as LibSql from "@libsql/client";
import * as Env from "./db/env";
import * as Watched from "./watched";

export type ExecuteResult = SuccessResult | ErrorResult;

export interface SuccessResult {
  kind: "success";
  metadata: {
    outOfDate: boolean;
    watched: Watched.Watched[];
  };
  data: LibSql.ResultSet[];
}

export interface ErrorResult {
  kind: "error";
  errorType: ErrorType;
  message: string;
}

export enum ErrorType {
  NotFound,
  Unauthorized,
  InvalidInput,
  InvalidSession,
  UnknownError,
  UnknownQuery,
  NoDatabase,
}

export interface Runner<session, input, output> {
  id: string;
  primary_db: Env.DatabaseKey;
  attached_dbs: Env.DatabaseKey[];
  session: {
    safeParse(
      data: unknown,
    ): { success: true; data: session } | { success: false; error: unknown };
  };
  session_args: string[];
  input: {
    safeParse(
      data: unknown,
    ): { success: true; data: input } | { success: false; error: unknown };
  };
  output: {
    safeParse(
      data: unknown,
    ): { success: true; data: output } | { success: false; error: unknown };
  };
  run: (
    env: Env.Config,
    session: session,
    args: unknown,
  ) => Promise<ExecuteResult>;
}

export type SqlInfo = {
  include: boolean;
  params: Array<string>;
  sql: string;
};

export type ToRunnerArgs<session, input, output> = {
  id: string;
  primary_db: Env.DatabaseKey;
  attached_dbs: Env.DatabaseKey[];
  session: {
    safeParse(
      data: unknown,
    ): { success: true; data: session } | { success: false; error: unknown };
  };
  session_args: string[];
  input: {
    safeParse(
      data: unknown,
    ): { success: true; data: input } | { success: false; error: unknown };
  };
  output: {
    safeParse(
      data: unknown,
    ): { success: true; data: output } | { success: false; error: unknown };
  };
  sql: Array<SqlInfo>;
  watch_triggers: Watched.Watched[];
};

export const to_runner = <Session, Input, Output>(
  options: ToRunnerArgs<Session, Input, Output>,
): Runner<Session, Input, Output> => {
  return {
    id: options.id,
    primary_db: options.primary_db,
    attached_dbs: options.attached_dbs,
    session: options.session,
    session_args: options.session_args,
    input: options.input,
    output: options.output,
    run: async (
      env: Env.Config,
      session: Session,
      input: unknown,
    ): Promise<ExecuteResult> => {
      // Validate session
      const validated_input = options.input.safeParse(input);

      if (!validated_input.success) {
        let errorMessage = "Invalid input";
        try {
          const errors = validated_input.error;
          if (errors && typeof errors === "object") {
            const errorStr = JSON.stringify(errors, null, 2);
            errorMessage = `Validation failed: ${errorStr}`;
          } else if (typeof errors === "string") {
            errorMessage = `Validation failed: ${errors}`;
          }
        } catch (e) {
          // Fallback to a generic message if we can't stringify
          errorMessage =
            "Input validation failed. Check that all required fields are present and have correct types.";
        }
        return {
          kind: "error",
          errorType: ErrorType.InvalidInput,
          message: errorMessage,
        };
      }

      // Validate session
      const validated_session = options.session.safeParse(session);
      if (!validated_session.success) {
        return {
          kind: "error",
          errorType: ErrorType.InvalidSession,
          message: "Expected object",
        };
      }

      // Validate that we have
      for (const db of options.attached_dbs) {
        if (db in env) {
          continue;
        }
        return {
          kind: "error",
          errorType: ErrorType.NoDatabase,
          message: `No instance of ${db} provided`,
        };
      }

      const valid_session_args = to_session_args(
        options.session_args,
        validated_session.data as Record<string, unknown>,
      );
      const attached_database_args = to_database_args(
        options.attached_dbs,
        env,
      );

      const valid_args = {
        ...validated_input.data,
        ...valid_session_args,
        ...attached_database_args,
      };

      // Finished validation, let's prepare the statements.

      const sql_arg_list: LibSql.InStatement[] = options.sql.map(
        ({ params, sql }) => {
          const filtered_args: Record<string, any> = {};
          for (const key of params) {
            if (key in valid_args) {
              filtered_args[key] = valid_args[key];
            }
          }

          return { sql: sql, args: filtered_args };
        },
      );

      const lib_sql_config = Env.to_libSql_config(env, options.primary_db);
      if (lib_sql_config == undefined) {
        return {
          kind: "error",
          errorType: ErrorType.NoDatabase,
          message: `${options.primary_db} database was not provided!`,
        };
      }

      // Done validating, let's talk to the db.
      const client = LibSql.createClient(lib_sql_config);
      try {
        const res = await client.batch(sql_arg_list);

        const formatted_return_data: any = {};

        for (const result_set of only_included(options.sql, res)) {
          // the generated sql formats the data as JSON
          // But it comes over as string encoded JSON
          // So we need to parse it

          if (result_set.columns.length < 1) {
            continue;
          }
          const col_name = result_set.columns[0];

          // SQL now returns arrays directly (not wrapped in objects)
          // Parse the JSON string and use it directly
          for (const row of result_set.rows) {
            console.log(row);
            if (col_name in row && typeof row[col_name] == "string") {
              const parsed: unknown = JSON.parse(row[col_name]);
              // The parsed value should be an array
              if (Array.isArray(parsed)) {
                formatted_return_data[col_name] = parsed;
              } else {
                // Fallback: if it's not an array, wrap it (shouldn't happen with new format)
                formatted_return_data[col_name] = [parsed];
              }
              break; // Only process first row (queries/mutations return single row)
            }
          }
        }

        return {
          kind: "success",
          metadata: { outOfDate: false, watched: options.watch_triggers },
          data: formatted_return_data,
        };
      } catch (error) {
        console.log(error);
        return {
          kind: "error",
          errorType: ErrorType.InvalidInput,
          message: "Database error",
        };
      }
    },
  };
};

type KeyValues = { [key: string]: unknown };

const normalize_sql_arg = (value: unknown): unknown => {
  if (value instanceof Date) {
    if (Number.isNaN(value.getTime())) {
      throw new Error("Invalid Date SQL argument");
    }
    return Math.floor(value.getTime() / 1000);
  }
  if (Array.isArray(value)) {
    return value.map(normalize_sql_arg);
  }
  if (value !== null && typeof value === "object" && !(value instanceof Uint8Array)) {
    return Object.fromEntries(
      Object.entries(value).map(([key, nested]) => [key, normalize_sql_arg(nested)]),
    );
  }
  return value;
};

const to_session_args = (
  allowed_keys: string[],
  session: Record<string, unknown>,
): KeyValues => {
  if (session == null) {
    return {};
  }

  const session_args: KeyValues = {};
  for (const key of allowed_keys) {
    let value: unknown = key in session ? session[key] : session;
    if (!(key in session)) {
      for (const part of key.split("__")) {
        if (value === null || typeof value !== "object" || !(part in value)) {
          value = null;
          break;
        }
        value = (value as Record<string, unknown>)[part];
      }
    }
    if (value !== null && typeof value === "object" && "_type" in value) {
      value = (value as Record<string, unknown>)._type;
    }
    const normalized = normalize_sql_arg(value);
    session_args["session_" + key] =
      normalized !== null && typeof normalized === "object"
        ? JSON.stringify(normalized)
        : normalized;
  }
  return session_args;
};

const to_database_args = (
  attached_databases: Env.DatabaseKey[],
  env: Env.Config,
): KeyValues => {
  const db_args: KeyValues = {};
  for (const db_key of attached_databases) {
    if (db_key in env && env[db_key] != undefined) {
      db_args["db_" + db_key] = env[db_key].id;
    }
  }
  return db_args;
};

function only_included(sqlItems: SqlInfo[], values: any[]): any[] {
  return values.filter((_, index) => sqlItems[index]?.include);
}
