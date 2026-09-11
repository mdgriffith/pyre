import {
  batch,
  capture,
  edit,
  type EditOperation,
  type EditLifecycle,
  type LocalEditsRuntime,
} from './local-edits';

interface Binding {
  runtime: LocalEditsRuntime;
  /** The same generated operation objects passed to LocalEditsRuntime. */
  operations: readonly EditOperation[];
}

interface Submission {
  databaseId: string;
  requestId: string;
  operations: Array<{
    namespace: string;
    manifest: string;
    operation: string;
    input: unknown;
  }>;
}

/** Install once per Elm application/port lifetime, not once per message or database.
 * Call forward synchronously in the existing outbound port callback, before its
 * normal query routing. Do not await credential preparation or use Cmd.batch for
 * independently ordered sends. Returning true means the effect was consumed.
 * Feed receive through the existing Pyre.decodeIncomingDelta port. Runtime validates/fences all worker
 * traffic and owns predictions, queue order, dispatch and result codecs.
 * Schedule must reserve a slot synchronously in the same queue as named mutations,
 * then supply that lifetime's binding (or undefined on initialization failure).
 */
export function elmLocalEdits(
  resolve: (databaseId: string) => Binding | undefined,
  receive: (event: unknown) => void,
  schedule: (databaseId: string, submit: (binding: Binding | undefined) => void) => void =
    (databaseId, submit) => submit(resolve(databaseId)),
): { forward(value: unknown): boolean; stop(): void; dispose(): void } {
  const seen = new Set<string>();
  const cleanups = new Set<() => void>();
  const scheduled = new Set<() => void>();
  let accepting = true;
  let active = true;
  const deliver = (event: unknown) => {
    try {
      receive(event);
    } catch {
      /* Port observers cannot change edit outcomes. */
    }
  };
  const stop = () => {
    accepting = false;
    for (const cancel of [...scheduled]) cancel();
  };
  return {
    forward(value) {
      if (
        !value ||
        typeof value !== 'object' ||
        (value as { type?: unknown }).type !== 'elm-local-edits'
      )
        return false;
      const { databaseId, requestId } = value as Submission;
      if (typeof databaseId !== 'string' || typeof requestId !== 'string')
        return true;
      const key = JSON.stringify([databaseId, requestId]);
      if (seen.has(key)) return true;
      seen.add(key);
      let snapshot: unknown;
      try {
        snapshot = capture(value);
      } catch {
        snapshot = null;
      }
      let consumed = false;
      const consume = (binding: Binding | undefined) => {
        if (consumed) return;
        consumed = true;
        scheduled.delete(cancel);
        const reject = (code: string) => {
          const fence = binding?.runtime.fence ?? {
            databaseId,
            instance: '',
            authGeneration: 0,
            namespace: '',
            manifest: '',
            databaseEpoch: '',
          };
          deliver({
            ...fence,
            requestId,
            type: 'lifecycle',
            state: 'rejected',
            code,
          });
          deliver({
            ...fence,
            requestId,
            type: 'failure',
            phase: 'validation',
            certainty: 'rejected',
            code,
          });
        };
        if (!accepting || !active || !binding) {
          reject('Disposed');
          return true;
        }
        const { runtime, operations } = binding;
        if (runtime.fence.databaseId !== databaseId) {
          reject('InvalidDatabase');
          return true;
        }
        const manifest = new Map(
          operations.map((operation) => [operation.id, operation]),
        );
        try {
          let plan: ReturnType<typeof batch> | undefined;
          try {
            const submission = snapshot as Submission;
            if (!Array.isArray(submission.operations))
              throw new Error('InvalidEdit');
            plan = batch(submission.operations.map((operation) => {
              const definition = manifest.get(operation.operation);
              if (
                !definition ||
                operation.namespace !== runtime.fence.namespace ||
                operation.manifest !== runtime.fence.manifest
              )
                throw new Error('InvalidOperation');
              return edit(definition, operation.input);
            }));
          } catch {
            // Submit invalid plans through runtime validation too, so database
            // failure observers see the rejection independently of Elm receipts.
          }
          // Capture synchronous queued/rejected/failure events before submit returns.
          let runtimeId: string | undefined;
          let cleanup = () => {};
          const early: Array<{ type: 'lifecycle' | 'failure'; event: any }> =
            [];
          const forward = (type: 'lifecycle' | 'failure', event: any) => {
            if (runtimeId === undefined) {
              early.push({ type, event });
              return;
            }
            if (event.requestId !== runtimeId || !active) return;
            // Elm decodes the validated wire representation, not TS codec output.
            const { result: _decoded, ...wire } = event;
            deliver({
              ...wire,
              type,
              requestId,
            });
            if (
              type === 'lifecycle' &&
              ['confirmed', 'rejected', 'acceptedUnreconciled'].includes(
                event.state,
              )
            ) {
              // A rejection's failure event follows its lifecycle in the same turn.
              queueMicrotask(() => {
                cleanup();
                cleanups.delete(cleanup);
              });
            }
          };
          const stopLifecycle = runtime.onLifecycle((event) =>
            forward('lifecycle', event as EditLifecycle),
          );
          const stopFailures = runtime.onEditFailure((event) =>
            forward('failure', event),
          );
          cleanup = () => {
            stopLifecycle();
            stopFailures();
          };
          cleanups.add(cleanup);
          const receipt = runtime.submit(plan!);
          runtimeId = receipt.requestId;
          for (const event of early) forward(event.type, event.event);
        } catch {
          reject('InvalidEdit');
        }
      };
      const cancel = () => consume(undefined);
      scheduled.add(cancel);
      try {
        if (accepting) schedule(databaseId, consume);
        else cancel();
      } catch {
        consume(undefined);
      }
      return true;
    },
    stop,
    dispose() {
      // Dispose the database runtimes first so final lifecycle events are delivered.
      stop();
      active = false;
      for (const cleanup of cleanups) cleanup();
      cleanups.clear();
    },
  };
}
