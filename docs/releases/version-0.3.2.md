# Pyre 0.3.2

- Fix migrations incorrectly reporting `createdAt` and `updatedAt` as removed when the source schema uses `@timestamps` and the stored schema has explicit timestamp fields.
- Normalize timestamp shorthand and implicit fields on both sides of logical schema comparisons using the same expansion as typechecking.
- Add regression coverage for runtime, push, and file-based migrations, including preservation of existing timestamp values and detection of actual timestamp removals.
