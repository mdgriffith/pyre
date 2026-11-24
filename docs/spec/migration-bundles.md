# Finalized migration bundles

[MEC-191](https://linear.app/mechanical-elephant/issue/MEC-191) tracks this planned workflow. The [migration guide](../usage/migrations.md) describes the currently available commands.

A finalized migration is an immutable, self-contained artifact. It supplies the SQL and the target schema to install; applying it does not depend on the current working schema. Each namespace has one predecessor chain, ordered by the links in the manifests rather than by folder names.

Format 1 contains `manifest.json`, `migration.sql` (which may be empty), the standalone target `schema.pyre`, and optional named `assertions/*.sql`. The manifest records `format`, `namespace`, `hash`, `predecessor`, and `targetApiVersion`. It may also record `graceSeconds` and `externalIdentities`. The bundle hash covers the canonical manifest without `hash` and the paths and contents of the bundled files. A catalog stores bundles; the target records application receipts and baseline anchors separately.

## Implementation order

1. [MEC-192](https://linear.app/mechanical-elephant/issue/MEC-192): Define sealing, verification, history validation, and the catalog interface.
2. [MEC-193](https://linear.app/mechanical-elephant/issue/MEC-193) and [MEC-194](https://linear.app/mechanical-elephant/issue/MEC-194): Enforce the transactional SQL boundary and implement single-database execution and recovery.
3. [MEC-195](https://linear.app/mechanical-elephant/issue/MEC-195) and [MEC-198](https://linear.app/mechanical-elephant/issue/MEC-198): Wire filesystem authoring and replay checks, including optional predecessor fixtures.
4. [MEC-196](https://linear.app/mechanical-elephant/issue/MEC-196) and [MEC-197](https://linear.app/mechanical-elephant/issue/MEC-197): Add structural audit, explicit baselining, and a database-backed catalog.
