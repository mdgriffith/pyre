# Pyre 0.3.1

- Fix missing `createdAt` and `updatedAt` fields in generated Elm stream rows and decoders for records using `@timestamps`.
- Expand timestamp shorthand and implicit fields before schema validation so all generators receive the same expanded schema.
