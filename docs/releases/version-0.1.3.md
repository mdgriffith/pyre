# Pyre - 0.1.3

`0.1.3` fixes SQL generation for nested linked selections when record-level permissions are applied inside aliased CTEs.

- Fixed `@allow` predicates in aliased nested CTE scopes to reference the active SQL table alias, avoiding invalid SQLite such as `"users"."id"` after `from users t`.
- Added regression coverage for nested `AuthSession -> User -> Membership` query generation.
