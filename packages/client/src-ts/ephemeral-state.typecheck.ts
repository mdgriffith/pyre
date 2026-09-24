import type { BoundEphemeralStateSnapshot, PyreClient } from './index';

interface Connection {
  userId: number;
  cursor: { x: number; y: number } | null;
}

interface ConnectionPatch {
  cursor?: Connection['cursor'];
}

interface Shared {
  count: number;
}

interface SharedPatch {
  count?: number;
}

interface GeneratedStateTypes {
  connection: Connection;
  connectionPatch: ConnectionPatch;
  shared: Shared;
  sharedPatch: SharedPatch;
}

// Compile-only fixture matching generated state.ts; it never executes client calls.
export function checkGeneratedStateBinding(client: PyreClient): void {
  void client.getEphemeralState<GeneratedStateTypes>('main').then((snapshot) => {
    const typed: BoundEphemeralStateSnapshot<GeneratedStateTypes> = snapshot;
    const count: number | undefined = typed.authoritative.shared?.count;
    const cursor = typed.desired.connection.cursor;
    void [count, cursor];
  });
  void client.subscribeEphemeralState<GeneratedStateTypes>('main', (snapshot) => {
    const connection: Connection | undefined = snapshot.authoritative.connections.current;
    void connection;
  });
  void client.updateEphemeralConnection<GeneratedStateTypes>('main', { cursor: { x: 1, y: 2 } });
  void client.updateEphemeralShared<GeneratedStateTypes>('main', { count: 1 });

  // @ts-expect-error Derived fields are absent from the generated patch type.
  void client.updateEphemeralConnection<GeneratedStateTypes>('main', { userId: 1 });
  // @ts-expect-error Nested values are complete, not recursive patches.
  void client.updateEphemeralConnection<GeneratedStateTypes>('main', { cursor: { x: 1 } });
}
