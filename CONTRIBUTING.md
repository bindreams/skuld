# Contributing to skuld

## Releasing

Releases go through two GitHub Actions workflows. Both are triggered by hand — there is no automatic release trigger. The first workflow is fully reversible; the second performs the irreversible operations (publishing to crates.io, creating the tag). The human reviewing the draft release between the two is the last chance to catch problems.

### Prerequisites

- `Cargo.toml`, `macros/Cargo.toml` and `cargo-skuld/Cargo.toml` already have the intended release version (say `X.Y.Z`) on `main`, and the exact pins between them match it. `cargo xtask version --check --exact` enumerates workspace members dynamically, so it validates version agreement and every intra-workspace `=` pin across all three.
- You have the GitHub CLI (`gh`) authenticated for the `bindreams/skuld` repo.
- A `Deploy` GitHub Environment is configured with a `CARGO_REGISTRY_TOKEN` scoped to `skuld` + `skuld-macros` + `cargo-skuld` with `publish-new` + `publish-update` permissions. A token scoped to only the first two cannot publish `cargo-skuld` — that omission is why the CLI, added to the workspace in #48, was never released.

### Stage 1 — Draft Release

```sh
gh workflow run draft-release.yaml -f version=X.Y.Z

# watch progress
gh run watch
```

This workflow:

- Validates the input version and checks `Cargo.toml` versions agree (via `cargo xtask version --check --exact`).
- Runs the full CI matrix (lint + 6-platform tests) against the release commit.
- Runs `cargo publish --workspace --dry-run`, covering every publishable member.
- Creates a **draft** GitHub release pinned to the exact commit SHA.

Review the draft at:

```
https://github.com/bindreams/skuld/releases
```

Check the generated release notes, edit if needed. Do **not** manually publish the draft — stage 2 handles that.

### Stage 2 — Publish Release

Once the draft looks right:

```sh
gh workflow run publish-release.yaml -f version=X.Y.Z

gh run watch
```

This workflow:

- Re-verifies the draft release exists and is pinned to a valid commit SHA.
- Checks out that commit.
- Re-runs `cargo xtask version --check --exact` against the checked-out tree.
- Publishes every publishable member to crates.io in one `cargo publish --workspace --locked` command (cargo handles topological ordering and index-visibility waiting, and skips `publish = false` members). Deliberately not a hand-written `-p` list: omitting a member from one is what left `cargo-skuld` unpublished from #48 until 0.3.1.
- Flips the GitHub release from draft to published, which creates the `vX.Y.Z` git tag.

### Recovery

Publishing is topological — `skuld-macros`, then `skuld`, then `cargo-skuld` — so stage 2 has two partial states. Yank whatever landed and cut a new version.

If it failed after `skuld-macros`:

```sh
cargo yank skuld-macros@X.Y.Z
```

If it failed after `skuld` (the likelier one: `cargo-skuld` publishes last, and its token scope is the one historically missing):

```sh
cargo yank skuld-macros@X.Y.Z
cargo yank skuld@X.Y.Z
```

If instead **all three published** and the "Publish GitHub release" step failed, nothing is wrong on crates.io. Do **not** yank. Flip the draft release to published by hand; that creates the tag and completes the release.

Because the bump below is lockstep, `cargo-skuld` then has no `X.Y.Z` at all. A gap in its version line is the accepted cost of a shared workspace version, not a problem to work around.

Bump `Cargo.toml` + `macros/Cargo.toml` + `cargo-skuld/Cargo.toml` to `X.Y.Z+1`, fix the root cause, then re-run both workflows with the new version.

### Useful commands during a release

```sh
# List recent workflow runs
gh run list --workflow draft-release.yaml
gh run list --workflow publish-release.yaml

# Tail logs of the most recent run
gh run view --log

# See the draft release
gh release view vX.Y.Z

# Delete a draft (e.g. to re-run stage 1)
gh release delete vX.Y.Z --yes --cleanup-tag
```
