import type { Client } from "@libsql/client";
import type { ZodType } from "zod";
import { assertSchemaContracts } from "../schema";
import { assertPersistentTransaction } from "./libsql";
import { buildArgs, executeStatements, formatResultData, toSqlStatements, type GeneratedEdit, type JsonSessionValidators, type SqlInfo } from "./sql";

type Validator<T> = ZodType<T>;

type RunnerMeta = {
  operation: "query" | "insert" | "update" | "delete" | "transaction";
  primary_db: string;
  attached_dbs: string[];
  schemaContracts: Readonly<Record<string, string>>;
  generatedEdit?: GeneratedEdit;
  session_args: string[];
  json_session_args?: string[];
  json_session_validators?: JsonSessionValidators;
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
    const primaryNamespace = meta.primary_db;
    const contracts = { ...meta.schemaContracts };
    if (!primaryNamespace || !Array.isArray(meta.attached_dbs) ||
        [primaryNamespace, ...meta.attached_dbs].some(namespace =>
          !Object.hasOwn(contracts, namespace) || typeof contracts[namespace] !== "string" || !contracts[namespace])) {
      throw new Error("Missing compiled schema contracts");
    }
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
      meta.json_session_args,
      meta.json_session_validators,
    );
    await assertPersistentTransaction(db);
    const tx = await db.transaction(meta.operation === "query" ? "read" : "write");
    try {
      await assertSchemaContracts(tx, contracts, primaryNamespace);
      const results = await executeStatements({ execute: tx.execute.bind(tx) }, toSqlStatements(sql, args), meta.generatedEdit);
      const data = decodeOrThrow<Result>(meta.ReturnData, formatResultData(sql, results), "return data");
      await tx.commit();
      return data;
    } catch (error) {
      if (!tx.closed) await tx.rollback();
      throw error;
    } finally {
      tx.close();
    }
  };
}
