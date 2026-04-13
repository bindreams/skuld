# Contributing to skuld

## Releasing

Releases go through two GitHub Actions workflows. Both are triggered by hand — there is no automatic release trigger. The first workflow is fully reversible; the second performs the irreversible operations (publishing to crates.io, creating the tag). The human reviewing the draft release between the two is the last chance to catch problems.

### Prerequisites

- `Cargo.toml` and `macros/Cargo.toml` already have the intended release version (say `X.Y.Z`) on `main`.
- You have the GitHub CLI (`gh`) authenticated for the `bindreams/skuld` repo.
- A `Deploy` GitHub Environment is configured with a `CARGO_REGISTRY_TOKEN` scoped to `skuld` + `skuld-macros` with `publish-new` + `publish-update` permissions.

### Stage 1 — Draft Release

```sh
gh workflow run draft-release.yaml -f version=X.Y.Z

# watch progress
gh run watch
```

This workflow:

- Validates the input version and checks `Cargo.toml` versions agree (via `cargo xtask version --check --exact`).
- Runs the full CI matrix (lint + 6-platform tests) against the release commit.
- Runs `cargo publish --dry-run` for both crates.
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
- Publishes both crates to crates.io in one `cargo publish -p skuld-macros -p skuld --locked` command (cargo handles topological ordering and index-visibility waiting).
- Flips the GitHub release from draft to published, which creates the `vX.Y.Z` git tag.

### Recovery

If stage 2 fails between `skuld-macros` and `skuld` publishing successfully, the macros crate is on crates.io alone. Yank it and cut a new version:

```sh
cargo yank -p skuld-macros --version X.Y.Z
```

Bump `Cargo.toml` + `macros/Cargo.toml` to `X.Y.Z+1`, fix the root cause, then re-run both workflows with the new version.

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
