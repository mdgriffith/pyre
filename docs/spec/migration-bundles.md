# Finalized migration bundles

[MEC-191](https://linear.app/mechanical-elephant/issue/MEC-191) tracks this planned workflow. The [migration guide](../usage/migrations.md) describes the currently available commands.

A finalized migration is an immutable, self-contained artifact. It supplies the SQL and the target schema to install; applying it does not depend on the current working schema. Each namespace has one predecessor chain, ordered by the links in the manifests rather than by folder names.

Format 1 contains `manifest.json`, `migration.sql` (which may be empty), the standalone target `schema.pyre`, and optional named `assertions/*.sql`. The manifest records `format`, `namespace`, `hash`, `predecessor`, and `targetApiVersion`. It may also record `graceSeconds` and `externalIdentities`. The bundle hash covers the canonical manifest without `hash` and the paths and contents of the bundled files. A catalog stores bundles; the target records application receipts and baseline anchors separately.

## Format 1 encoding

`hash` and non-null `predecessor` values are lowercase hexadecimal SHA-256 digests. A root must include `"predecessor":null`. `targetApiVersion` is the exact version string supplied by the compiler. Grace defaults to 86,400 seconds and is omitted from canonical JSON at that value. External identity entries are sorted by `(namespace, record, field)` and included only when the list is nonempty. Duplicate or unknown manifest members and duplicate identity keys are invalid.

The unsigned canonical manifest is compact UTF-8 JSON, with fields in this order: `format`, `namespace`, `predecessor`, `targetApiVersion`, `graceSeconds`, `externalIdentities`. Identity objects use `namespace`, `record`, `field`, `kind` in that order, with `kind` equal to `"Id.Int"` or `"Id.Uuid"`. JSON strings escape quotes and backslashes, use `\b`, `\t`, `\n`, `\f`, `\r` and lowercase `\u00xx` for the remaining ASCII control characters below U+0020, and leave all other UTF-8 characters unescaped. The manifest on disk need not use canonical whitespace or member ordering.

Hash the following sequence with SHA-256:

1. The literal bytes `pyre-migration-bundle\0v1\0`, where `\0` is a NUL byte.
2. The framed unsigned canonical manifest.
3. The payload file count as an unsigned 64-bit big-endian integer.
4. For each payload file in bytewise path order, its framed UTF-8 relative path (with `/` separators) followed by its framed contents. A frame is an unsigned 64-bit big-endian byte length followed by that many bytes.

The payload contains only the required files and optional `assertions/<name>.sql` files. Paths and UTF-8 contents are hashed exactly, including line endings; checkouts must preserve those bytes. Folder names and `manifest.json` itself are outside the payload. Assertions are read in filename order. Sealing and reading both validate that `schema.pyre` can be loaded using just the manifest namespace and identities.

The filesystem catalog reads `migrations/<namespace>/<folder>/`, including `migrations/_default/` for the default namespace. A folder without a manifest is outside finalized history. Finalized directories require regular files and reject extra entries. History order comes from predecessor links, not directory names. A content hash checks internal integrity; knowing whether a catalog is truncated or entirely replaced requires a separately trusted head or receipt.

## Implementation order

1. [MEC-192](https://linear.app/mechanical-elephant/issue/MEC-192): Define sealing, verification, history validation, and the catalog interface.
2. [MEC-193](https://linear.app/mechanical-elephant/issue/MEC-193) and [MEC-194](https://linear.app/mechanical-elephant/issue/MEC-194): Enforce the transactional SQL boundary and implement single-database execution and recovery.
3. [MEC-195](https://linear.app/mechanical-elephant/issue/MEC-195) and [MEC-198](https://linear.app/mechanical-elephant/issue/MEC-198): Wire filesystem authoring and replay checks, including optional predecessor fixtures.
4. [MEC-196](https://linear.app/mechanical-elephant/issue/MEC-196) and [MEC-197](https://linear.app/mechanical-elephant/issue/MEC-197): Add structural audit, explicit baselining, and a database-backed catalog.
