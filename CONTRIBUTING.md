# Contributing to skuld

## Releasing

Releases go through two GitHub Actions workflows. Both are triggered by hand — there is no automatic release trigger. The first workflow is fully reversible; the second performs the irreversible operations (publishing to crates.io, creating the tag). The human reviewing the draft release between the two is the last chance to catch problems.

### Prerequisites

- `Cargo.toml`, `macros/Cargo.toml` and `cargo-skuld/Cargo.toml` already have the intended release version (say `X.Y.Z`) on `main`, and the exact pins between them match it. `cargo xtask version --check --exact` enumerates workspace members dynamically, so it validates version agreement and every intra-workspace `=` pin across all three.
- You have the GitHub CLI (`gh`) authenticated for the `bindreams/skuld` repo.
- **For recovery only:** a personal crates.io token with the `yank` scope on every publishable member, via `cargo login` or `cargo yank --token`. Publishing mints its own short-lived token inside the job, so there is none to borrow. Without this, the first command of either partial-publish recovery fails on authentication.
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
- Publishes them with `cargo publish --workspace --locked --no-verify` (cargo handles topological ordering and index-visibility waiting, and skips `publish = false` members). Deliberately not a hand-written `-p` list. `--no-verify` is safe only because the step above just did that verification over exactly this member set.
- Flips the GitHub release from draft to published, which creates the `vX.Y.Z` git tag.

### Recovery

**Check the job summary first.** A red run whose summary says `RELEASE COMPLETE` is a finished release: the failure came from a post step that runs after the flip — most often the auth action revoking its short-lived token. Take no action: do not yank, bump, or re-dispatch. Everything below applies only to runs _without_ that marker.

Publishing is topological — `skuld-macros`, then `skuld`, then `cargo-skuld` — and `cargo publish` is not atomic, so a server-side error part-way through leaves the workspace partially published.

**First, establish which state you are in.** The workflow log says where it stopped; the registry is authoritative. Use the sparse index, which needs no `User-Agent` — the crates.io JSON API answers `403` with an empty body to curl's default one.

The snippet reads the status code separately from the body, and that is why it does not use `--fail`. `curl -s` alone exits `0` whatever the server said, so judging by the body only cannot tell a 404 from a 503; `--fail` collapses them the other way, into one non-zero exit. Either way a transient error reads as "absent", which reports an untouched registry and routes you into re-running stage 2 at a version that is in fact already taken. Reading `%{http_code}` is necessary but not sufficient: curl writes that value even when the transfer fails, and once response headers have arrived it reads `200`, so a connection cut mid-body yields `200` with a short body. The index lists versions in publication order, so the one you are asking about is the last line — exactly what a truncation drops. The snippet therefore branches on curl's **exit status** and only trusts the code when the transfer completed. Any `UNKNOWN` line means re-run the check rather than act on it. The block runs in a subshell so a refusal cannot close your terminal.

```bash
(
V=X.Y.Z   # the version you were publishing — the only thing to edit

# Both guards exist because the failure they prevent is silent: every crate
# reads `absent`, which is exactly what an untouched registry looks like.
if [ "$V" = X.Y.Z ]; then
  echo "Set V to the version you were publishing, then re-run."; exit 1
fi

members=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.publish != []) | .name')
if [ -z "$members" ]; then
  echo "Could not enumerate publishable members. Fix that before trusting anything below."; exit 1
fi

for c in $members; do
  # Same helper the release workflow uses, so the two cannot disagree about
  # where a crate lives in the index.
  path=$(.github/scripts/crate-index-path.sh "$c") || exit 1

  if out=$(curl -sS -w '\n%{http_code}' --retry 3 --retry-all-errors --max-time 30 \
      -X GET "https://index.crates.io/$path"); then
    code=$(printf '%s\n' "$out" | tail -n 1)
  else
    code=000   # transfer failed; the body may be truncated, so do not read it
  fi
  line=$(printf '%s\n' "$out" | grep -F "\"vers\":\"$V\"" || true)

  if [ "$code" != "200" ] && [ "$code" != "404" ]; then
    printf '%-14s %s: UNKNOWN (HTTP %s) — do not act on this line\n' "$c" "$V" "$code"
  elif [ -z "$line" ]; then
    printf '%-14s %s: absent\n' "$c" "$V"
  elif printf '%s' "$line" | grep -q '"yanked":true'; then
    printf '%-14s %s: YANKED — slot consumed, bump\n' "$c" "$V"
  else
    printf '%-14s %s: PUBLISHED\n' "$c" "$V"
  fi
done
)
```

The member list is derived rather than written out, so it stays right as the workspace grows. **Run this from the repository root**: it invokes `.github/scripts/crate-index-path.sh` by a root-relative path — the same helper the release workflow uses, so the two cannot disagree about where a crate lives in the index.

**Any `YANKED` means the version slot is gone.** crates.io reserves a version permanently on publish; yanking hides it but never frees it, so stage 2 can never succeed at that version again. Go to the bump path below regardless of what the other crates report. If a crate reads `absent` immediately after a successful-looking upload, wait a minute and re-check before yanking anything — index propagation lags.

**Nothing published** (every crate `absent`, none `YANKED`). crates.io is untouched and there is nothing to undo. What to do next depends on why it stopped:

- _Environmental_ (registry outage, runner failure): re-run **stage 2** with the same version once the cause has cleared. The draft's pinned commit is still correct, so nothing else needs doing. Note that a cause you can only fix by committing is a _tree_ cause, not this one — a commit changes the tree, so it takes the path below.
- _Tree_ (packaging, verification, or the version re-check): stage 2 checks out the draft's pinned commit, so re-running replays the identical failure. Delete the draft with `gh release delete "vX.Y.Z" --yes`, push the fix, then re-run **stage 1** and stage 2. Stage 1 refuses to create a draft while a release with that tag exists, which is why the delete comes first.

**Only `skuld-macros` published:**

```sh
cargo yank skuld-macros@X.Y.Z
```

**`skuld-macros` and `skuld` published** (the likelier partial: `cargo-skuld` publishes last):

```sh
cargo yank skuld-macros@X.Y.Z
cargo yank skuld@X.Y.Z
```

**In either partial case above**, capture the commit before deleting the draft — the draft is its only source:

```sh
SHA=$(gh release view "vX.Y.Z" --json targetCommitish -q .targetCommitish) &&
  git fetch origin &&
  git tag "vX.Y.Z" "$SHA" &&
  git push origin "vX.Y.Z" &&
  gh release delete "vX.Y.Z" --yes   # NOT --cleanup-tag: that deletes the tag just pushed
```

The tag has to be created by hand because only the final GitHub-release flip creates it, and that never ran — so the newest tag is still `vX.Y.(Z-1)` and `cargo xtask version --check` would reject `X.Y.Z+1` as a two-step jump, blocking the bump commit both locally and in Lint.

Then bump every publishable member's manifest to `X.Y.Z+1` (`cargo metadata` above lists them; today that is `Cargo.toml`, `macros/Cargo.toml` and `cargo-skuld/Cargo.toml`), fix the root cause, and re-run both workflows with the new version. Because the bump is lockstep, `cargo-skuld` then has no `X.Y.Z` at all — a gap in its version line is the accepted cost of a shared workspace version, not a problem to work around.

**All three published.** Whatever failed afterwards — the GitHub-release flip, or `cargo publish` itself during the index-visibility wait — nothing is wrong on crates.io. Do **not** yank, and do **not** bump: the release is complete apart from its tag.

```sh
gh release edit "vX.Y.Z" --draft=false
```

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
