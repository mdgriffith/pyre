# Pyre - 0.1.11

`0.1.11` adds tagged-union session predicate paths across generated SQL, Rust and TypeScript servers, WASM sync, manifests, and MCP queries. Permissions and queries can now target variant payloads such as `Session.scope.Workspace.id`, with discriminator guards and runtime validation for nested and recursive session values.

Insert permissions are now enforced against proposed rows before they are written, including generated IDs, defaults, nested inserts, and generated CRUD operations. Constant permission policies such as `@allow(query) { True }` and `@allow(insert) { False }` are also supported. Fine-grained `@allow` declarations must now explicitly cover `query`, `insert`, `update`, and `delete`; schemas that relied on omitted operations being implicitly denied should add explicit deny rules or use `@allow(*)`.

Embedded and migration-stored namespace schemas are now standalone, including required session types and tagged-union dependencies, so they can be reloaded and typechecked independently. This release also fixes nullable typed JSON `null` semantics, typed JSON shaping in live sync deltas, sync permission hashing, null session bindings, generated TypeScript session validation, Elm list update types, and generated trailing whitespace.
