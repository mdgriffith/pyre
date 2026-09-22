import type { Client } from '@libsql/client';
import { captureOperations, decodeOperationResults, type DatabaseTarget, type Operation, type OperationResults, type SubmissionTarget } from '@pyre/client/operations';
import { run, type QueryMap, type QueryResult, type Session, type SessionValue } from './query';
import { runWithSync } from './query-sync';

export type OperationExecution<Items extends readonly Operation[]> =
  | { ok: true; value: OperationResults<Items> }
  | { ok: false; error: NonNullable<QueryResult['error']> };

export type OperationExecutionMode =
  | { mode: 'normal' }
  | { mode: 'sync'; sessions?: Map<string, { session: Record<string, SessionValue> }>; publish: (sessionId: string, message: unknown) => void };

/** Execute captured builders under an explicit application-authorized database and session.
 * This uses compiled permissions and one transaction, unlike the import-oriented seed API.
 * The caller resolves target.databaseId to db; a target is not an authorization token.
 */
export async function executeOperations<const Items extends readonly Operation[]>(
  db: Client,
  queries: QueryMap,
  target: SubmissionTarget<Items> & DatabaseTarget<string>,
  operations: Items,
  session: Session,
  options: OperationExecutionMode,
): Promise<OperationExecution<Items>> {
  if (!session || typeof session !== 'object' || Array.isArray(session)) throw new Error('An explicit execution session is required');
  const items = Object.freeze([...operations]) as unknown as Items;
  const captured = captureOperations(items, target);
  for (const item of captured) {
    const query = queries[item.queryId];
    if (query && query.primary_db !== target.namespace) throw new Error('Operation database namespace mismatch');
  }
  const wire = captured.map(({ queryId, input }) => ({ queryId, input }));
  const execution = options.mode === 'sync'
    ? await runWithSync(db, queries, wire, undefined, session, options.sessions, target.databaseId)
    : await run(db, queries, wire, undefined, session);
  if (execution.kind === 'error') return { ok: false, error: execution.error! };
  // Preserve responses before publication wraps the existing HTTP envelope.
  const response = execution.response;
  if (options.mode === 'sync') await execution.sync(options.publish);
  return { ok: true, value: decodeOperationResults(items, response) };
}
