# Bug: Migration Diff Treats Default Trailing Whitespace as Schema Identity

## Summary

Pyre 0.1.6 can report a column as modified when the only difference between the current schema and the schema persisted in `_pyre_schema` is whitespace following a value-based `@default(...)` directive. Because migration errors are fatal, this blocks `pyre migrate ... --push` even though the column type, nullability, and default value are unchanged.

Observed error:

```text
The column published has been modified in the table ClocktowerSeat. This might be causing issues in your query. Consider updating your query to use the new column format.
```

## Reproduction

Use a record containing a value default followed by a blank line:

```pyre
record Example {
    id        Id.Int @id
    published Bool   @default(False)

    parent @link(parentId, Parent.id)
}
```

1. Create or reseed a database with this schema using Pyre 0.1.6.
2. Run `pyre migrate <database> --namespace <namespace> --push` against the same schema again.
3. The migration exits with code 1 and reports `published` as modified.
4. Remove only the blank line after `@default(False)`:

   ```pyre
   published Bool @default(False)
   parent @link(parentId, Parent.id)
   ```

5. Run the same migration again. It succeeds with no schema changes.

This was reproduced with the local CLI and published release both built from commit `6135051412d563b3fab24d4a62c96c5c97b285b0` (`version-0.1.6`). Five Boolean defaults in Lore failed together, and all five passed after removing only their following blank lines.

## Root Cause

`src/parser.rs::parse_default_value` derives `ColumnDirective::Default.id` from text sliced out of the input remaining *after* the parsed value:

```rust
let original_text = &input_after.fragment()[..end_offset - start_offset];
let id = original_text.to_string();
```

That makes the ID depend on characters following the value, including the closing parenthesis and trailing whitespace. The canonical schema serializer removes blank lines between fields before storing schema source in `_pyre_schema`, so reparsing the persisted schema produces a different default ID from reparsing the authored source.

`src/ast/diff.rs::diff_column` then keys default directives by this unstable ID:

```rust
crate::ast::ColumnDirective::Default { id, .. } => id.clone(),
```

The old and new directive maps therefore appear to contain a removed default and an added default. `diff_schema` emits `MigrationColumnModified`, and `migrate --push` exits before checking the physical database diff.

## Expected Behavior

Default directives should be compared by semantic identity and value. Formatting, comments, and whitespace around or after `@default(...)` must not produce a schema migration.

## Suggested Fix

- Do not use source text or parser location as the identity key for a default directive.
- Compare `ColumnDirective::Default.value` semantically, including distinguishing `now` from literal values.
- Emit a modification only when the default is added, removed, or its semantic value changes.
- Consider correcting `parse_default_value` separately if `id` is intended to contain the original value text; its current slice is taken from the remaining input rather than the consumed input.

## Regression Tests

Add migration/AST-diff tests covering:

1. Identical Boolean defaults with different trailing whitespace produce no diff.
2. A blank line added or removed after `@default(False)` produces no diff.
3. Formatting differences after string, integer, float, and null defaults produce no diff.
4. Changing `@default(False)` to `@default(True)` produces a modified-column diff.
5. Adding or removing a default still produces the appropriate migration diff.
6. A schema serialized into `_pyre_schema` and reparsed compares equal to its authored source.

## Temporary Consumer Workaround

Keep value-default fields immediately adjacent to the following field/link so authored source matches Pyre's canonical persisted schema. This is formatting-sensitive and should be removed after the semantic comparison is fixed.
