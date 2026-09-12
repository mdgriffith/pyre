# Generated CRUD Mutations

Pyre compiles schema-derived `{Table}Create`, `{Table}Update`, and `{Table}Delete`
operations alongside handwritten mutations. They require no handwritten `.pyre`
file and generate no read queries. The [local-edit contract](local-edits.md)
defines batching, cardinality, identity results, prediction and reconciliation;
the [usage guide](../usage/local-edits.md) shows current TS/Elm imports.

## Generation And Permissions

The current compiler creates all three operation definitions for each table with
an integer or UUID primary key and reserves their names, including for denied
operations. Other primary-key kinds remain available to ordinary generation, but
their local-edit CRUD/descriptors are omitted with an explicit warning. Availability of a builder
is not permission to write. Compiled permission checks and the effective server
session determine whether execution is allowed; generation does not filter out
operations based on an individual session's permissions.

User-authored operations cannot override reserved CRUD names. Name collisions
produce a compiler error identifying the table and operation. Generated operations
carry compiler metadata; a handwritten command is never recognized as generated
CRUD merely by its spelling.

## Inputs

The compiled operations use **flat arguments**, not a nested `$input` object.
Create arguments are writable column names; update arguments are the actual
primary-key name followed by optional writable columns; delete takes only that
primary key. Primary keys need not be named `id`.

The schema-derived `{Table}.CreateInput` and `{Table}.UpdateInput` concepts describe
field semantics, not the current generated transport layout. For example, the
User fixture's `Records.User.update(userId, { note: null })` captures a flat operation
input `{ key: userId, note: null }`.

| Field rule | Create | Update |
| --- | --- | --- |
| Writable, non-nullable, no default | Required | Optional; omit to leave unchanged |
| Nullable | May omit or explicitly set null | Omit to leave unchanged; null clears |
| Non-nullable with default | May omit to use default; null is invalid | May omit; null is invalid |
| Immutable and otherwise insertable | Included | Excluded |
| Managed/server-owned | Excluded | Excluded |
| Client UUID primary key | Supplied when required by schema | Target only, never a patch field |
| Generated integer primary key | Excluded | Target only, never a patch field |

Unknown/protected fields are rejected, not silently ignored. An empty update is
`InvalidEdit`, not a read or successful no-op. Setting a writable field to its
existing value is a write targeting one row. JSON and tagged unions replace whole
values, so concurrent changes to separate members can overwrite each other.

TypeScript uses partial patch objects. Elm uses opaque record-specific `Patch`
setters; nullable setters take `Maybe`, while non-nullable setters take plain
values. Duplicate Elm setters resolve left to right, last wins. Elm creates use a
required record plus optional opaque `CreateOption` setters through `createWith`.

## Results And Execution

Generated CRUD returns `Created<Id>`, `Updated<Id>`, or `Deleted<Id>`, each containing
`{ id }` with the affected identity, regardless of the schema's primary-key name.
It does not promise a readable row. Server-generated integer IDs come only from
authoritative execution, not temporary client IDs.

Every generated operation must affect exactly one row inside the transaction.
Zero/multiple writable targets reject with non-disclosing `TargetNotWritable` and
roll back the entire batch; missing and forbidden rows are indistinguishable.
Read-filtered result counts cannot establish write cardinality. Named mutations
retain their declared zero/many behavior and result codecs.

The server independently validates the manifest, namespace, session, protected
fields and inputs. A batch executes in order in one transaction and allocates one
commit revision on success, never exposing a successful prefix on rejection.
Named commands and generated CRUD share this executor; neither caller-supplied SQL
nor runtime query compilation is part of the edit protocol.

## Discoverability

Compiled CRUD participates in generated query metadata, manifests and `Query.*`
Elm modules. The pure local-edit surface is additionally exposed through
`typescript/edits/<Namespace>.ts` and `Db.<Namespace>.Edit.<Record>` in Elm.
Browser fenced execution is opt-in through `ServerConfig.localEdits`; pure builder
imports do not initialize a database or transport. Regenerate clients and server
manifests together rather than hand-authoring operation IDs.

## Domain Commands

Use generated CRUD for ordinary writes only when schema rules fully express the
required constraints. CRUD can bypass invariants or side effects implemented only
inside a named command. Such writes must keep using that command, including its
generated `Commands`/Elm `Command.*.run` wrapper when batching. MEC-117 remains the
enforcement/discoverability follow-up; this documentation does not claim it solved.

Named commands also remain appropriate for nested inserts, computed values,
session-derived assignments and custom return shapes. They are nonoptimistic.
Generated prediction is conservative, and one unpredictable member makes the whole
batch nonoptimistic. See the local-edit contract for the safety requirements.

Upserts, read-query generation, CRUD name overrides and dependent binding of
earlier batch results are not part of this surface. Cross-runtime release
conformance remains subject to verification under MEC-119.
