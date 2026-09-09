# Session-Free Client Guide

The browser has no `Session` configuration, no session in `connect` results, no
`setSession`, and no local `$session` substitution. The effective Pyre session
remains private on the server. Authenticate using the application's existing
transport; server sessions, schema permissions, and ordinary auth cookies remain
unchanged. See `server-contexts` (`pyre://guides/server-contexts`) for custom servers.

## Accessible IDs And Sync Selection

The application supplies its own accessible database ID list through its existing
bootstrap or other app-owned source. Pyre does not enumerate databases or install
sessions in the browser. Accessibility is independent of the active sync set:
receiving the list does not start sync.

For an already configured client and an app-selected ID:

```typescript
import type { PyreClient } from "@pyre/client";

export async function selectDatabase(
  client: PyreClient,
  accessibleDatabaseIds: readonly string[],
  selectedId: string,
): Promise<void> {
  if (!accessibleDatabaseIds.includes(selectedId)) {
    throw new Error("Database is not in the application's accessible list");
  }
  await client.syncDatabase(selectedId);
}
```

`syncDatabase` adds to the active set. Use `await client.setSyncedDatabases(selectedIds)`
instead to replace the whole active set. Neither list membership nor sync selection
grants access: the server must authenticate and authorize each request. The list
check above is UI behavior, not a security boundary. Keep using the existing
authenticated query/catchup/live transport; no new routes or context negotiation
are needed.

## Local Query Boundary

An explicit local query dependency on `Session`, such as
`@where { ownerId == Session.userId }`, is unsupported. Generated local query
sources carry the rejection marker:

```json
{
  "$error": "Local queries cannot reference Session; use explicit inputs or execute on the server."
}
```

Do not remove the marker, substitute a browser session, or silently run an
unfiltered query. Use an ordinary explicit input to filter already-authorized data:

```pyre
query NotesByOwner($ownerId: Int) {
    note {
        @where { ownerId == $ownerId }
        id
        body
    }
}
```

Alternatively, explicitly execute the Session-dependent operation on the
authenticated server. There is no automatic server fallback. Ordinary query
inputs are not permission grants and cannot replace server authorization.

Permission-only schema usage remains valid: a record's
`@allow(query) { ownerId == Session.userId }` is enforced on the server and does
not make an otherwise Session-free local query unsupported. Local queries operate
over authorized synced data without receiving the effective server session.

## Revocation Limits

The application owns login expiration, membership changes, sync selection, and
transport authentication. Server context TTL/invalidation is not browser cache
revocation. Removing browser sessions or deselecting databases does not establish
permission-contraction cache cleanup guarantees; that remains separate work.
