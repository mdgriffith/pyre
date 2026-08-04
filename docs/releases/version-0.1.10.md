# Pyre - 0.1.10

`0.1.10` fixes generated TypeScript typechecking for recursive tagged unions decoded with Zod preprocessing. Recursive decoders now model normalized input separately from strongly typed output using native Zod 4 schema types, including recursive dictionaries, nested lists, nullable values, coercions, and references to other preprocessed generated types.

Generated TypeScript now explicitly supports Zod 4, with pinned compile and runtime regression coverage that no longer silently skips when the JavaScript toolchain is unavailable. This release also fixes namespaced database initialization so generated schema-bound database helpers initialize and migrate the selected namespace correctly.
