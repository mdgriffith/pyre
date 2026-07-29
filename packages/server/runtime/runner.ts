import type { Client } from "@libsql/client";
import type { ZodType } from "zod";
import { buildArgs, formatResultData, toSqlStatements, type SqlInfo } from "./sql";

type Validator<T> = ZodType<T>;

type RunnerMeta = {
  session_args: string[];
  optional_input_args: string[];
  json_input_args: string[];
  InputValidator: Validator<any>;
  SessionValidator: Validator<any>;
  ReturnData: Validator<any>;
};

function decodeOrThrow<T>(validator: Validator<T>, data: unknown, label: string = "data"): T {
  const parsed = validator.safeParse(data);
  if (!parsed.success) {
    throw new Error(`Failed to decode ${label}: ${String(parsed.error)}`);
  }
  return parsed.data;
}

export function toRunner<Input, Result>(meta: RunnerMeta, sql: SqlInfo[]) {
  return async (
    db: Client,
    inputOrSession?: Input | Record<string, any>,
    maybeInput?: Input
  ): Promise<Result> => {
    const input =
      maybeInput === undefined
        ? (inputOrSession as Input | undefined)
        : maybeInput;
    const session =
      maybeInput === undefined ? {} : (inputOrSession as Record<string, any>);

    const validatedInput = decodeOrThrow(
      meta.InputValidator,
      input ?? {},
      "input"
    ) as Record<string, unknown>;
    const validatedSession = decodeOrThrow(
      meta.SessionValidator,
      session,
      "session"
    ) as Record<string, unknown>;

    const args = buildArgs(
      validatedInput,
      validatedSession,
      meta.session_args,
      meta.optional_input_args,
      meta.json_input_args,
    );
    const results = await db.batch(toSqlStatements(sql, args));
    const data = formatResultData(sql, results);
    return decodeOrThrow(meta.ReturnData, data, "return data");
  };
}
