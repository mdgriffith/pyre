# Preparing a Release

This repo uses `version-*` git tags to build draft GitHub releases. Do not use `v*` tags for release builds unless the workflow is changed first.

## Version Bump

For a Pyre release, update the tracked version files that apply to the released artifacts:

- `Cargo.toml`
- `Cargo.lock`
- `pyre-cli/Cargo.toml`
- `wasm/Cargo.toml`
- `wasm/Cargo.lock`
- `packages/cli/package.json`
- `packages/core/package.json`
- `packages/server/package.json`
- `packages/client/package.json`
- `bun.lock`
- `docs/releases/version-X.Y.Z.md`

Keep package dependency versions aligned when they reference another package in this repo. For example, `packages/server/package.json` should depend on the matching `@pyre/core` release version.

`wasm/pkg/package.json` is generated and ignored by git. Do not include it in the release commit unless the generated wasm package artifacts are intentionally being checked in.

## Release Notes

Add release notes at:

```text
docs/releases/version-X.Y.Z.md
```

The release workflow reads this exact file for non-Windows jobs by deriving `VERSION` from the tag name. For tag `version-0.1.3`, the workflow expects:

```text
docs/releases/version-0.1.3.md
```

## Validation

Run the checks relevant to the release contents before tagging:

```bash
cargo check
cargo check --manifest-path wasm/Cargo.toml
bun run release:check
bun run release:smoke
```

For targeted bug-fix releases, also run the focused regression tests for the change.

## Commit

Inspect the worktree before committing:

```bash
git status --short
git diff
```

Stage the implementation, tests, version bumps, lockfile updates, and release note:

```bash
git add <changed-files>
git commit -m "Release X.Y.Z"
```

## Tagging

Use an annotated `version-*` tag for real releases:

```bash
git tag -a version-X.Y.Z -m "Pyre X.Y.Z"
git push origin HEAD
git push origin version-X.Y.Z
```

The release workflow triggers on:

```yaml
tags:
  - test-release
  - version-*
```

`version-*` tags build platform binaries and the `@pyre/core`, `@pyre/server`, and `@pyre/client` package tarballs. The workflow creates a draft GitHub release with all of those artifacts attached. `test-release` uploads binary build artifacts but does not create the versioned GitHub release.

## After Tagging

Check the `Prepare release` GitHub Actions run. The workflow creates a draft GitHub release, so review the generated release, release notes, platform binaries, and these TypeScript package assets before publishing it:

```text
pyre-core-X.Y.Z.tgz
pyre-server-X.Y.Z.tgz
pyre-client-X.Y.Z.tgz
```
