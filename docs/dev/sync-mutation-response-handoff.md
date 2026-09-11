# Sync Mutation Result Correction

The root `sync-mutation-response-handoff.md` records a historical bug and proposed
fix. Its claims that current Rust/TypeScript sync execution discards mutation
results are stale. It must not be used as the current integration contract.

- `src/server/query.rs` formats named mutation results in sync execution as well
  as normal execution, and extracts affected rows separately. Generated edits
  return their affected identity as `{ id }`.
- `packages/server/query.ts` also formats the active SQL result in sync mode.
- When legacy Rust sync fanout wraps a response, `src/server/sync.rs` preserves
  it under `result`; decode that nested result, not the entire sync envelope.
  Do not assume every invocation wraps: the no-change path can return without it.
- The local-edit batch protocol is different: accepted envelopes contain ordered
  `{ index, operation, value }` results plus a commit revision and reconciliation
  requirement. Validate count, IDs and codecs before typed access.

Typed results need not be reconstructed from `affected_rows`. A decoding failure
after commit is not proof of rollback and must not trigger automatic write retry.
The TypeScript server binding reports a known committed codec failure as
`acceptedUnreconciled` / `InvalidResult`, with revision and no fabricated result.
See [current local-edit usage](../usage/local-edits.md) and the
[contract](../spec/local-edits.md). Production cross-runtime conformance remains a
MEC-119 verification gate, not a conclusion of this historical correction.
