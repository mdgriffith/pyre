# Pyre - 0.1.4

`0.1.4` fixes type resolution for foreign keys targeting UUID primary keys. Generated TypeScript validators and Rust server bindings now use string-compatible UUID types for these fields, while record ID aliases continue to work for both integer and UUID foreign keys.
