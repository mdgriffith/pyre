# Pyre - 0.1.8

`0.1.8` improves tagged-union and typed JSON support in generated TypeScript and SQL. Dictionary validators now use the Zod 4 `z.record(key, value)` form, direct payload-bearing tagged-union parameters are serialized correctly by the TypeScript runner, and nested typed JSON values round-trip through those parameters.

Generic create and update operations now reuse compatible fields shared across tagged-union variants without generating duplicate columns, assignments, or affected-row headers. Literal tagged-union inserts also bind omitted nullable variant fields as `NULL` so generated columns and values remain aligned.
