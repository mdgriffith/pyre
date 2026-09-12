import { batch, edit, type EditOperation, type EditPlan, type EditReceipt, type LocalEditsRuntime } from './index';
import type { PyreClient, Namespace, Database, Edit, Batch } from './index';

export async function checkNamespaceHandle(
  client: PyreClient,
  scope: Namespace<'Main'>,
  plan: Edit<'Main', { id: string }>,
  plans: Batch<'Main', readonly [{ id: string }, string]>,
  other: Edit<'Other', { id: string }>,
) {
  const db: Database<'Main'> = await client.localEdits('main', scope);
  const one: EditReceipt<{ id: string }> = db.submit(plan);
  const many: EditReceipt<readonly [{ id: string }, string]> = db.submit(plans);
  // @ts-expect-error A database handle cannot submit another namespace's plan.
  db.submit(other);
  const runtime: LocalEditsRuntime = await client.localEdits('main');
  return { one, many, runtime };
}

// Generated adapters preserve heterogeneous tuples without widening to a union array.
export function checkLocalEditTypes(db: LocalEditsRuntime) {
  const rename: EditOperation<{ name: string }, { id: number }> = {
    id: 'rename',
    parseInput: input => input as { name: string },
    decodeResult: result => result as { id: number },
  };
  const close: EditOperation<{ sprint: number }, string> = {
    id: 'close',
    parseInput: input => input as { sprint: number },
    decodeResult: result => result as string,
  };
  const plan: EditPlan<readonly [{ id: number }, string]> = batch([
    edit(rename, { name: 'ready' }), edit(close, { sprint: 1 }),
  ] as const);
  const receipt: EditReceipt<readonly [{ id: number }, string]> = db.submit(plan);
  receipt.confirmed.then(outcome => {
    if (outcome.kind === 'confirmed') {
      const id: number = outcome.result[0].id;
      const result: string = outcome.result[1];
      // @ts-expect-error Tuple position zero is not a named-command string.
      const wrong: string = outcome.result[0];
      return { id, result, wrong };
    }
  });
  // @ts-expect-error Generated input does not accept a numeric name.
  edit(rename, { name: 1 });
  const empty: EditPlan<readonly []> = batch([] as const);
  return { receipt, empty };
}
