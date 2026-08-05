# Permission Enforcement Handoff

## Current Language Behavior

Fine-grained permissions must explicitly cover `query`, `insert`, `update`, and
`delete`. `@public` and `@allow(*)` are complete alternatives.

```pyre
@allow(query) { True }
@allow(insert, update, delete) { False }
```

- `True` is unrestricted and adds no permission predicate to SQL.
- `False` renders as `WHERE 0`, preserving empty top-level and nested selection
  shapes.
- `@allow(*) { False }` explicitly denies every operation.
- Omitted fine-grained operations are a typecheck error.

## Enforcement Status

| Operation | Current behavior |
| --- | --- |
| Query | Enforced in SQL for top-level and nested selections |
| Delete | Enforced against the existing row |
| Update | Enforced against the existing row only |
| Insert | Enforced against the proposed row for top-level, nested, dynamic, and generated CRUD inserts |
| Sync visibility | Query permission is applied to catch-up and live deltas |

## Priority Gaps

1. **Update has no proposed-row authorization.** A policy such as
   `ownerId == Session.userId` authorizes the existing row but does not prevent
   changing `ownerId` to another user.
2. **Sync removal semantics need separate work.** Deletes and permission
   revocations do not reliably remove previously cached client rows.

Restricted inserts are rendered as `INSERT ... SELECT` from an internal
proposed-row projection. The insert predicate is evaluated against that
projection before SQLite receives a row. Nested inserts apply the same check at
every inserted table, and denied parents do not populate the temporary row-id
table used by child inserts.

## Recommended Order

1. Define and implement proposed-row update semantics.
2. Address sync tombstones and permission-revocation cleanup separately.

Relational insert permissions remain intentionally unsupported and need a
separate design for authorization against both proposed values and existing
related rows.
