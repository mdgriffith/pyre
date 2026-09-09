import type { GeneratedQueryShape, QueryShape, RejectedQueryShape } from '@pyre/core';
import type { PyreClient } from './index';

// Compile-only fixture matching generated metadata; never executes client queries.
export function checkGeneratedQueryModules(client: PyreClient, generatedShape: GeneratedQueryShape): void {
  type Input = { owner: number };
  const normalShape: QueryShape = { posts: { id: true, '@where': { owner: { $var: 'owner' } } } };
  const rejectedShape: RejectedQueryShape = {
    $error: 'Local queries cannot reference Session; use explicit inputs or execute on the server.',
  };
  const normalMetadata = {
    operation: 'query' as const,
    queryShape: normalShape,
    toQueryShape: (_input: Input) => normalShape,
  };
  const rejectedMetadata = {
    operation: 'query' as const,
    queryShape: rejectedShape,
    toQueryShape: (_input: Input) => rejectedShape,
  };
  const generatedMetadata = {
    operation: 'query' as const,
    queryShape: generatedShape,
    toQueryShape: (_input: Input): GeneratedQueryShape => generatedShape,
  };

  void client.run('main', normalMetadata, { owner: 1 }, () => {});
  void client.run('main', rejectedMetadata, { owner: 1 }, () => {});
  void client.run('main', generatedMetadata, { owner: 1 }, () => {});
}
