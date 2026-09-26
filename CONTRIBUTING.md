# Contributing to skuld

## Releasing

Releases go through two GitHub Actions workflows. Both are triggered by hand — there is no automatic release trigger. The first workflow is fully reversible; the second performs the irreversible operations (publishing to crates.io, creating the tag). The human reviewing the draft release between the two is the last chance to catch problems.

### Prerequisites

- `Cargo.toml`, `macros/Cargo.toml` and `cargo-skuld/Cargo.toml` already have the intended release version (say `X.Y.Z`) on `main`, and the exact pins between them match it. `cargo xtask version --check --exact` enumerates workspace members dynamically, so it validates version agreement and every intra-workspace `=` pin across all three.
- You have the GitHub CLI (`gh`) authenticated for the `bindreams/skuld` repo.
- **For recovery only:** a personal crates.io token with the `yank` scope on every publishable member, via `cargo login` or `cargo yank --token`. Publishing mints its own short-lived token inside the job, so there is none to borrow. Without this, the first command of the abandon-and-bump recovery path fails on authentication.
- Every publishable member has a **trusted publisher** configured on crates.io — GitHub, owner `bindreams`, repository `skuld`, workflow `publish-release.yaml`, environment `Deploy`. Stage 2 mints a short-lived token by OIDC and carries no long-lived secret. All four fields are matched exactly, so both the workflow **filename** and the environment name are load-bearing: renaming the file or dropping `environment: Deploy` breaks publishing, and neither is visible until the irreversible step.
- **Open action item, not yet configured:** the `Deploy` environment needs a **deployment branch policy limiting it to `main`**. A trusted-publisher config has no ref field — it matches only the four values above — so GitHub's branch policy is the one place a ref restriction can live. Until it is set, any branch carrying this workflow filename and this environment name can mint a token valid for all three crates and publish from unreviewed code, going around `main`'s protection. Set it under Settings → Environments → Deploy → Deployment branches. Unlike the bullets above, this one describes work still to do; the comment on the job in `publish-release.yaml` says the same.

### Adding a publishable member

A crate that does not exist on crates.io **cannot** have a trusted publisher configured, so stage 2 cannot publish it. Its first release is manual, once:

1. **Before** bumping the workspace, publish it by hand at the **currently released** version `X.Y.(Z-1)`, with a temporary token scoped to that crate with `publish-new` (`cargo publish -p <crate> --locked`). Publishing it at the version you are about to release would leave stage 2 seeing one member published and the rest absent.
2. Configure its trusted publisher with the four fields above, then **confirm it is listed** under the crate's Settings → Trusted Publishing. Nothing automated can check this: crates.io exposes no unauthenticated way to read a publisher config, so a missing one is invisible until stage 2 gets a 403 on that member. For a leaf like `cargo-skuld` that upload comes after its dependencies, so the others will already be up.
3. Revoke the temporary token, once a release has gone out through stage 2.

If the new member needs sibling APIs that are not yet released, step 1 is impossible: it cannot build against `X.Y.(Z-1)`, and the exact intra-workspace pin forbids `X.Y.Z` while the siblings are unpublished. Split it across two releases instead — give the member `publish = false` and cut `X.Y.Z` without it (`cargo metadata` then reports `publish: []`, which the first-publish check skips and `cargo publish --workspace` will not upload), then hand-publish it at `X.Y.Z`, configure its publisher, drop `publish = false`, and let `X.Y.(Z+1)` be its first automated release.

`draft-release.yaml` fails on any member that has never been published, which catches step 1 being skipped. It cannot catch step 2 being skipped — it verifies the crate exists, not that a publisher is configured for it.

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
- Checks crates.io for every publishable member: that each one already exists (a crate that does not cannot have a trusted publisher, so stage 2 could never publish it), and that the version about to be released is **not already taken**. The dry-run above does not catch the latter — it warns "already exists on crates.io" and still exits `0`. The version checked is the one in the manifests, which is also asserted to equal the dispatched input.
- Creates a **draft** GitHub release pinned to the exact commit SHA.

Review the draft at:

```
https://github.com/bindreams/skuld/releases
```

Check the generated release notes, edit if needed. Do **not** manually publish the draft — stage 2 handles that.

> **Before running stage 2**, confirm every publishable member has a trusted publisher listed under its crates.io Settings → Trusted Publishing, matching owner `bindreams`, repo `skuld`, workflow `publish-release.yaml`, environment `Deploy`. Nothing up to this point has authenticated, so a missing or mismatched config is invisible until the irreversible upload. Renaming the workflow file or dropping `environment: Deploy` breaks publishing the same way.

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
- Packages and verify-builds the publishable members, **before** minting the token. `cargo publish` would otherwise run that build inside the credential's ~30-minute life, and a cold build can consume most of it — expiring the token between uploads, which is a partial publish. The member list comes from `cargo metadata` rather than `--workspace`, which would also package `publish = false` members: a superset that both lengthens this build and enforces packaging rules on crates that are never packaged.
- Classifies every publishable member's state **at this version** via the crates.io JSON API (`.github/scripts/crate-state.sh`), and skips whichever are already `published` — this is what makes a re-dispatch after a partial or interrupted publish safe: it will not try to re-upload a member that already went out. Refuses outright, publishing nothing, if any member is `yanked` at this version — that slot can never be reused; see Recovery below.
- Publishes whatever is left with `cargo publish --workspace --locked --no-verify --exclude <already-published member>...` (cargo handles topological ordering and index-visibility waiting among what remains, and skips `publish = false` members). Skipped entirely if every member is already published. `--no-verify` is safe only because the packaging step above just did that verification over the whole set; what it still skips is the registry-side checks cargo makes at upload time, such as the size cap.
- Flips the GitHub release from draft to published, which creates the `vX.Y.Z` git tag. This step has no `if:` — it runs even when publishing was skipped, so a re-dispatch that finds everything already on crates.io still finishes the release.

### Recovery

**Check the job summary first.** A red run whose summary says `RELEASE COMPLETE` is a finished release: the failure came from a post step that runs after the flip — most often the auth action revoking its short-lived token. Take no action: do not yank, bump, or re-dispatch. If the summary does not show the marker, check the step's raw log too before concluding the flip never happened: the marker is written to both the log and the summary specifically so a summary write failure (`GITHUB_STEP_SUMMARY` unwritable, say) cannot hide a release that did complete — `tee` still writes to its other outputs, including stdout, even when one output fails. Everything below applies only to runs where the marker is absent from both.

Publishing is topological — `skuld-macros`, then `skuld`, then `cargo-skuld` — and `cargo publish` is not atomic, so a server-side error, a cancellation, or an expiring token can leave the workspace partially published.

**In every case below except a `yanked` version, the fix is the same: re-run stage 2 at the same version.** Before touching the registry, stage 2 now classifies every publishable member's state at that version via the crates.io JSON API and skips whichever are already `published` — including all of them, if every upload from the previous attempt actually succeeded and only the GitHub-release flip failed. A member reaching `published` already passed packaging and verification in the run that published it — that step runs for the whole set before any upload — so nothing still pending can fail for a code reason; only `yanked` blocks a re-run, because that slot can never be reused.

**First, establish which state you are in.** The workflow log says where it stopped, but the registry is authoritative. This uses the same scripts the workflow does, so it cannot disagree with them — run it from the repository root:

```bash
(
V=X.Y.Z   # the version you were publishing — the only thing to edit

if [ "$V" = X.Y.Z ]; then
  echo "Set V to the version you were publishing, then re-run."; exit 1
fi

members=$(.github/scripts/publishable-members.sh) || exit 1
for c in $members; do
  if state=$(.github/scripts/crate-state.sh "$c" "$V"); then
    printf '%-14s %s: %s\n' "$c" "$V" "$state"
  else
    printf '%-14s %s: refused — see the error above; do not act on this line\n' "$c" "$V"
  fi
done
)
```

`crate-state.sh` reads the crates.io JSON API rather than the sparse index, which is CDN-cached for 600s — long enough to still read `absent` right after a real upload — and refuses instead of guessing on anything ambiguous, so a `refused` line means re-run the check rather than act on it.

**Any `yanked`.** The version slot is gone — crates.io reserves a version permanently on publish, and yanking hides it but never frees it, so stage 2 refuses to publish anything at this version regardless of how the other members read. Skip to "Abandoning this version" below. If a member reads `absent` immediately after a successful-looking upload, wait a minute and re-check before concluding anything — index propagation lags, though `crate-state.sh` itself is not index-based and should not.

**No `yanked`, at least one `published`.** Just re-run stage 2 with the same version. It will skip whatever already succeeded — including finishing a release where every upload went out but the flip did not — and publish only what is left.

**Nothing published** (every member `absent`, none `yanked`). crates.io is untouched and there is nothing to undo. What to do next depends on why it stopped:

- _Environmental_ (registry outage, runner failure): re-run **stage 2** with the same version once the cause has cleared. The draft's pinned commit is still correct, so nothing else needs doing. Note that a cause you can only fix by committing is a _tree_ cause, not this one — a commit changes the tree, so it takes the path below.
- _Tree_ (packaging, verification, or the version re-check): stage 2 checks out the draft's pinned commit, so re-running replays the identical failure. Delete the draft with `gh release delete "vX.Y.Z" --yes`, push the fix, then re-run **stage 1** and stage 2. Stage 1 refuses to create a draft while a release with that tag exists, which is why the delete comes first. A tree cause can only happen here, before anything has published: packaging and verification cover the whole member set and run before any upload, so once even one member is `published` the remaining ones already passed that gate.

### Abandoning this version

Only needed when a member reads `yanked`, or the release is being pulled for a content reason unrelated to publish mechanics. Otherwise use the re-run path above instead — it is strictly less work and does not spend a version slot.

Yank every member the diagnostic above reported as `published` (skip ones already `yanked` or `absent`):

```sh
cargo yank <crate>@X.Y.Z
```

Capture the commit before deleting the draft — the draft is its only source:

```sh
SHA=$(gh release view "vX.Y.Z" --json targetCommitish -q .targetCommitish) &&
  git fetch origin &&
  git tag "vX.Y.Z" "$SHA" &&
  git push origin "vX.Y.Z" &&
  gh release delete "vX.Y.Z" --yes   # NOT --cleanup-tag: that deletes the tag just pushed
```

The tag has to be created by hand because only the final GitHub-release flip creates it, and that never ran — so the newest tag is still `vX.Y.(Z-1)` and `cargo xtask version --check` would reject `X.Y.(Z+1)` as a two-step jump, blocking the bump commit both locally and in Lint.

Then bump every publishable member's manifest to `X.Y.(Z+1)` (`.github/scripts/publishable-members.sh` lists them; today that is `Cargo.toml`, `macros/Cargo.toml` and `cargo-skuld/Cargo.toml`), fix the root cause if there is one, and re-run both workflows with the new version. Because the bump is lockstep, a member that published cleanly at `X.Y.Z` and was never yanked keeps that version in its history, while a yanked or never-reached one does not — a gap or a yank in one crate's version line is the accepted cost of a shared workspace version, not a problem to work around.

### Useful commands during a release

```sh
# List recent workflow runs
gh run list --workflow draft-release.yaml
gh run list --workflow publish-release.yaml

# Tail logs of the most recent run
gh run view --log

# See the draft release
gh release view vX.Y.Z

# Delete a draft (e.g. to re-run stage 1). Omit --cleanup-tag if you created the
# tag by hand during recovery — it would delete it.
gh release delete vX.Y.Z --yes --cleanup-tag
```
