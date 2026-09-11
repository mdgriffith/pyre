import type { Client } from "@libsql/client";
import { capture, planKey, type Batch, type Edit, type Namespace } from "@pyre/core/local-edits";
import { requireDatabaseId } from "./database-id";
import type { BatchManifest, BatchResponse, Session } from "./query";
import { runBatchWithSync, type BatchSyncRecipient } from "./query-sync";

export type Outcome<R> =
  | { kind: "confirmed"; result: R; commitRevision?: number }
  | { kind: "rejected"; code: string; index?: number }
  | { kind: "acceptedUnreconciled"; code: "InvalidResult"; commitRevision: number; index?: number }
  | { kind: "outcomeUnknown"; code: "OutcomeUnknown" };

export interface BindOptions<N> {
  /** Trusted, already-authorized target. Never resolve this from an edit's input. */
  database: Client;
  databaseId: string;
  namespace: Namespace<N>;
  manifest: BatchManifest;
  /** Actual application session, including an explicit {} for sessionless schemas. */
  session: Session;
  /** Live registrations are intentionally consulted after commit. */
  connectedSessions?: Map<string, BatchSyncRecipient>;
  sendToSession?: (sessionId: string, message: unknown) => void | Promise<void>;
}

export interface LocalEdits<N> {
  submit<R>(edit: Edit<N, R> | Batch<N, R>): Promise<Outcome<R>>;
}

/** Bind only after application authorization (for example inside context.run).
 * Invalid configuration/session throws synchronously. Submission never retries writes.
 */
export function bind<N>(options: BindOptions<N>): LocalEdits<N> {
  const { database, connectedSessions, sendToSession } = options;
  const databaseId = requireDatabaseId(options.databaseId);
  const scope = capture({ name: options.namespace.name, manifest: options.namespace.manifest });
  if (!scope.name || options.manifest.version !== 1 || scope.manifest !== options.manifest.manifestVersion)
    throw new Error("InvalidRequest");
  // Retain immutable codecs, but snapshot all mutable compiled execution metadata.
  const manifest: BatchManifest = { ...options.manifest, queries: Object.fromEntries(
    Object.entries(options.manifest.queries).map(([id, query]) => [id, {
      ...query, sql: structuredClone(query.sql), syncSql: structuredClone(query.syncSql),
      generatedEdit: structuredClone(query.generatedEdit), session_args: [...query.session_args],
      optional_input_args: [...query.optional_input_args], json_input_args: [...query.json_input_args],
      attached_dbs: query.attached_dbs && [...query.attached_dbs],
      json_session_args: query.json_session_args && [...query.json_session_args],
      json_session_validators: query.json_session_validators && { ...query.json_session_validators },
    }]),
  ) };
  let session: Session;
  try {
    if (!options.session || typeof options.session !== "object" || Array.isArray(options.session)) throw new Error();
    session = structuredClone(options.session);
    if (!manifest.SessionValidator.safeParse(structuredClone(session)).success) throw new Error();
  } catch { throw new Error("InvalidSession"); }
  const authority = { databaseId, namespace: scope.name, manifest: scope.manifest,
    instance: crypto.randomUUID(), authGeneration: 0 };

  return Object.freeze({
    async submit<R>(edit: Edit<N, R> | Batch<N, R>): Promise<Outcome<R>> {
      const failure = (code: string, index?: number): Outcome<R> => ({ kind: "rejected", code,
        ...(index === undefined ? {} : { index }) });
      let operations;
      let decoders;
      let result;
      let requestId: string;
      let index: number | undefined;
      try {
        const plan = edit[planKey];
        if (plan.namespace?.name !== scope.name || plan.namespace.manifest !== scope.manifest) return failure("InvalidRequest");
        result = plan.result;
        decoders = plan.operations.map(member => member.definition.decodeResult);
        operations = plan.operations.map((member, i) => {
          index = i;
          const input = capture(member.input);
          member.definition.parseInput(input);
          return { operation: member.definition.id, input };
        });
        if (operations.length === 0) return { kind: "confirmed", result: result([]) };
        index = undefined;
        requestId = crypto.randomUUID();
      } catch (error) { return failure(error instanceof Error && error.message === "InvalidEdit" ? "InvalidEdit" : "InvalidRequest", index); }

      let response: BatchResponse;
      let commitRevision: number;
      try {
        const executed = await runBatchWithSync(database, manifest, authority, {
          version: 1, ...authority, databaseEpoch: "", requestId, sequence: 1, operations,
        }, session, connectedSessions, sendToSession, true);
        if (executed.kind === "unknown") return { kind: "outcomeUnknown", code: "OutcomeUnknown" };
        if (executed.kind === "error") return failure(executed.error.errorType, executed.error.index);
        response = executed.response;
        if (response.requestId !== requestId || typeof response.databaseEpoch !== "string" || !response.databaseEpoch
          || response.databaseId !== authority.databaseId || response.namespace !== authority.namespace
          || response.manifest !== authority.manifest || response.instance !== authority.instance
          || response.authGeneration !== authority.authGeneration || response.status !== "accepted"
          || typeof response.commitRevision !== "number" || !Number.isSafeInteger(response.commitRevision)
          || response.commitRevision < 1) throw new Error("InvalidResponse");
        commitRevision = response.commitRevision;
      } catch {
        // Once execution is entered, an unexpected failure cannot prove noncommit.
        return { kind: "outcomeUnknown", code: "OutcomeUnknown" };
      }
      index = undefined;
      try {
        if (!Array.isArray(response.results) || response.results.length !== operations.length) throw new Error();
        const values = operations.map((operation, i) => {
          index = i;
          const entry = response.results[i];
          if (entry.index !== i || entry.operation !== operation.operation) throw new Error();
          return decoders[i](entry.value);
        });
        index = undefined;
        return { kind: "confirmed", result: result(values), commitRevision };
      } catch {
        // The database has already committed. A codec failure must never invite replay.
        return { kind: "acceptedUnreconciled", code: "InvalidResult", commitRevision,
          ...(index === undefined ? {} : { index }) };
      }
    },
  });
}

export const localEdits = Object.freeze({ bind });
