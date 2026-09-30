# Contributing to skuld

## Releasing

Releases go through two GitHub Actions workflows. Both are triggered by hand — there is no automatic release trigger. The first workflow is fully reversible; the second performs the irreversible operations (publishing to crates.io, creating the tag). The human reviewing the draft release between the two is the last chance to catch problems.

### Prerequisites

- `Cargo.toml`, `macros/Cargo.toml` and `cargo-skuld/Cargo.toml` already have the intended release version (say `X.Y.Z`) on `main`, and the exact pins between them match it. `cargo xtask version --check --exact` enumerates workspace members dynamically, so it validates version agreement and every intra-workspace `=` pin across all three.
- You have the GitHub CLI (`gh`) authenticated for the `bindreams/skuld` repo.
- **For recovery only:** a personal crates.io token with the `yank` scope on every publishable member, via `cargo login` or `cargo yank --token`. Publishing mints its own short-lived token inside the job, so there is none to borrow. Without this, the first command of the abandon-and-bump recovery path fails on authentication.
- Every publishable member has a **trusted publisher** configured on crates.io — GitHub, owner `bindreams`, repository `skuld`, workflow `publish-release.yaml`, environment `Deploy`. Stage 2 mints a short-lived token by OIDC and carries no long-lived secret. All four fields are matched exactly, so both the workflow **filename** and the environment name are load-bearing: renaming the file or dropping `environment: Deploy` breaks publishing, and neither is visible until the irreversible step.
- The `Deploy` environment's deployment branch policy allows only `main` (owner decision, 2026-09-30, superseding the 2026-09-26 decision to have none). A trusted-publisher config has no ref field — it matches only the four values above — so this GitHub policy is the one place a restriction on the _workflow's_ ref can live; without it, any branch carrying this workflow filename and this environment name could mint a token valid for all three crates. It restricts the dispatch ref only: it does not restrict which commit is published (that is whatever the draft release targets) nor what code runs in the job that holds the token. Consequence: dispatch stage 2 from `main` (`gh workflow run` without `--ref` uses the default branch, `main`); on any other ref, GitHub refuses the publish job itself (the jobs before it still run). The comment on the job in `publish-release.yaml` says the same.

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

- **Resolve job** (write-scoped token, nothing compiled): re-verifies the draft release exists and is pinned to a valid commit SHA, and reads the release toolchain from `.github/release-toolchain` at that commit (see "Toolchain pin" below). The token is exposed as `GH_TOKEN` to the draft lookup only.
- **Build job** (`contents: read`, no environment, no OIDC token): checks out that commit without persisting credentials, re-runs `cargo xtask version --check --exact`, and packages and verify-builds the publishable members. Everything that compiles third-party build scripts or proc-macros lives here, so a compromised dependency cannot mint the crates.io token or push to the repo. It uploads the `.crate` files as the `crates` artifact (for forensics only; nothing downstream reads it) and the sha256 manifest as a separate, tiny `sha256sums` artifact. The verify build runs before any token exists: `cargo publish` would otherwise do it inside the credential's ~30-minute life, and a cold build can consume most of it — expiring the token between uploads, which is a partial publish. The member list comes from `cargo metadata` rather than `--workspace`, which would also package `publish = false` members: a superset that both lengthens this build and enforces packaging rules on crates that are never packaged.
- **Publish job** (`Deploy` environment, `id-token: write`, compiles nothing): checks out the same commit, installs the pinned toolchain, re-checks the manifest version with `cargo metadata`, then re-derives the packages with `cargo package --no-verify --locked` and fails on any sha256 that differs from the build job's manifest. It downloads only the `sha256sums` artifact, not the archives, so it unpacks as little build-job-controlled data as possible. `GH_TOKEN` reaches the draft flip only. The job takes nothing from the build job except that hash list, and runs its actions from the dispatch commit, not the release tree, so a compromised build job can cause a refusal but not code execution or different bytes here.
- Classifies every publishable member's state **at this version** via the crates.io JSON API (`.github/scripts/crate-state.sh`), and skips whichever are already `published` **and match this run's own build** — verified by comparing crates.io's checksum for that version against the sha256 of this run's own re-derived `.crate` (byte-identical to the build job's), so a re-dispatch only ever skips a member because this tree already published it, never because some other build occupies the slot. Refuses outright on a checksum mismatch. Refuses outright, publishing nothing, if any member is `yanked` at this version — that slot can never be reused; see Recovery below.
- Publishes whatever is left with `cargo publish --workspace --locked --no-verify --exclude <already-published member>...` (cargo handles topological ordering and index-visibility waiting among what remains, and skips `publish = false` members). Skipped entirely if every member is already published. `--no-verify` is safe only because the build job did that verification over exactly this member set, and the re-derive step proved the archives identical — it skips only that local verification build. Registry-side limits such as crates.io's upload size cap still apply at upload, and nothing before stage 2 exercises them.
- Flips the GitHub release from draft to published, which creates the `vX.Y.Z` git tag. This step has no `if:` — it runs even when publishing was skipped, so a re-dispatch that finds everything already on crates.io still finishes the release.

### Toolchain pin

`.github/release-toolchain` holds one exact `X.Y.Z` (no `rust-toolchain.toml`, which would also change dev builds). The build and publish jobs, and stage 1's `validate` and `verify` jobs, install exactly that toolchain (the `read-toolchain-pin` action runs `.github/scripts/release-toolchain.sh`, and `install-toolchain` exports `RUSTUP_TOOLCHAIN` for the later steps, which outranks any `rust-toolchain(.toml)` in the release tree, and refuses a toolchain that is not the one running; a missing or malformed pin fails stage 1 before any draft exists), because different cargo versions can package the same tree into different bytes, and the publish job holds its re-derived archives to the build job's hashes. Reading it from the draft's target commit also makes a re-dispatch after a partial publish byte-identical to the first run, even if a newer stable has shipped since.

**The draft must target a commit that contains the file**; stage 2 fails in the resolve job otherwise, and the fix is to recreate the draft from a newer commit. Bumping the pin is a normal PR: change the version, merge, then draft from a commit at or after the merge. Renovate opens those PRs (`customManagers` entry in `.github/renovate.json`, tracking `rust-lang/rust` releases). Stage 1 builds with the pin, so a dependency bump that needs a newer compiler fails there, before a draft exists.

### Recovery

**Check the job summary first.** A red run whose summary says `RELEASE COMPLETE` is a finished release: the failure came from a post step that runs after the flip — most often the auth action revoking its short-lived token. Take no action: do not yank, bump, or re-dispatch. If the summary does not show the marker, check the step's raw log too before concluding the flip never happened: the marker is written to both the log and the summary specifically so a summary write failure (`GITHUB_STEP_SUMMARY` unwritable, say) cannot hide a release that did complete — `tee` still writes to its other outputs, including stdout, even when one output fails. Everything below applies only to runs where the marker is absent from both.

Publishing is topological — `skuld-macros`, then `skuld`, then `cargo-skuld` — and `cargo publish` is not atomic, so a server-side error, a cancellation, or an expiring token can leave the workspace partially published.

**Most cases below are fixed the same way: re-run stage 2 at the same version.** Before touching the registry, stage 2 classifies every publishable member's state at that version via the crates.io JSON API and skips whichever are already `published` — but only after checking that member's crates.io checksum against this run's own re-derived `.crate` (byte-identical to the build job's), so a re-run only ever skips a member because _this tree_ already put it there, never because some other build occupies the slot. That includes skipping all of them, if every upload from the previous attempt actually succeeded and only the GitHub-release flip failed. Two things do not get fixed by re-running: a `yanked` version (that slot can never be reused) and a still-`absent` member that crates.io rejects again on content grounds — packaging and verification only prove the build is sound, not that crates.io's own upload checks will accept it, and a rejection for the same content repeats identically on every re-run. Both are tree causes; see "Abandoning this version" below rather than looping stage 2.

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

`crate-state.sh` reads the crates.io JSON API rather than the sparse index, which is CDN-cached for 600s — long enough to still read `absent` right after a real upload. That is a guarantee about the index's own cache, not about the API: crates.io does not document its API as read-after-write consistent, so a read here can still be stale. Nothing below depends on catching that in this diagnostic — see "Nothing published" for what actually guards against a member having published despite reading `absent` here. This script also refuses instead of guessing on anything ambiguous, so a `refused` line means re-run the check rather than act on it.

**A member reads `never-published`.** Stage 1's verify job already refuses to create a draft for any member in this state, so reaching stage 2 with one should be unreachable — if it happens anyway, the tree the draft points at has changed in a way stage 1 never saw it. There is nothing to yank and nothing a re-run fixes: follow "Adding a publishable member" above first, then re-run stage 1 before touching stage 2 again.

**Any `yanked`.** The version slot is gone — crates.io reserves a version permanently on publish, and yanking hides it but never frees it, so stage 2 refuses to publish anything at this version regardless of how the other members read. Skip to "Abandoning this version" below.

**No `yanked`, at least one `published`.** Just re-run stage 2 with the same version. It will skip whatever already succeeded — including finishing a release where every upload went out but the flip did not — and publish only what is left. Re-running is the only path even when every member already reads `published`: a manual `gh release edit --draft=false` flip would skip stage 2's classify step entirely, and with it the one thing that step exists to confirm — that each `published` member's crates.io checksum actually matches this tree's own packaged build, not just that a `published` label happens to be true. Re-running costs a repackage and re-verify it would otherwise skip, but it is what makes "this run's own build" a checked fact rather than an assumption.

**Stage 2 itself reports a checksum mismatch for a `published` member.** Stop — do not re-run, yank, or bump yet. This means crates.io's `published` verdict for that member is real, but it did not come from this run's own build: either a different commit published it (the draft's target may not be what you think, or someone published outside this pipeline), or `crate-state.sh`/the packaging step read the wrong artifact. Confirm which commit crates.io's version actually corresponds to before deciding anything — re-running only repeats the same refusal, and both yanking and bumping assume you already know why the mismatch happened.

**At least one member already `published`, and a still-`absent` one is tree-caused** (crates.io rejects its content again on every re-run, or the fix needs a commit). Deleting the draft and starting over does not apply here: the published members already occupy this version, so nothing about this version is undoable, and the fix itself requires a different tree, which lockstep versioning cannot give this version anyway. Skip straight to "Abandoning this version" below, which yanks every publishable member — including the ones that already published cleanly — as part of the same lockstep bump; see that section for why it does not try to spare them.

**Nothing published** (every member `absent`, none `yanked`). What to do next depends on why it stopped:

- _Environmental_ (registry outage, runner failure): re-run **stage 2** with the same version once the cause has cleared. The draft's pinned commit is still correct, so nothing else needs doing. Note that a cause you can only fix by committing is a _tree_ cause, not this one — a commit changes the tree, so it takes the path below.
- _Deploy environment refused the ref_ ("Branch … is not allowed to deploy to Deploy" on the publish job): nothing was uploaded. Re-dispatch stage 2 from `main`; a re-run in the UI keeps the bad ref and fails identically.
- _Pinned toolchain mismatch_ ("Pinned toolchain is X but cargo Y is running", or "Requested toolchain … but cargo … is running" in the build job): the toolchain install fell back to the runner's preinstalled cargo. Nothing was uploaded and the draft is still valid; re-run stage 2.
- _Uploaded tree is not the built tree_ ("Refusing to publish" from the re-derive step): both jobs verified they run the pinned cargo, and packaging is deterministic, so a hash mismatch is a possible sign the build job was compromised (a dependency's build script altered its output). Investigate it from the first occurrence, before re-running: read the build job's log and its `crates` artifact for the mismatching crate, and audit `Cargo.lock` changes since the last release. Nothing was uploaded, so the draft stays valid.
- _Tree_ (packaging, verification, a target commit without `.github/release-toolchain`, the version re-check, or crates.io rejecting a member's content on upload): stage 2 checks out the draft's pinned commit, so re-running replays the identical failure. Delete the draft with `gh release delete "vX.Y.Z" --yes`, push the fix, then re-run **stage 1** and stage 2. Stage 1 refuses to create a draft while a release with that tag exists, which is why the delete comes first. If a member actually was published despite reading `absent` above, stage 1's own re-check of crates.io catches that and refuses rather than creating a conflicting draft. This path assumes nothing has published yet — once even one member is confirmed `published`, use the case above instead.

### Abandoning this version

Needed when a member reads `yanked`, when a still-`absent` member is tree-blocked despite a partial publish, or when the release is being pulled for a content reason unrelated to publish mechanics. Otherwise use the re-run path above instead — it is strictly less work and does not spend a version slot.

Yank every publishable member (`.github/scripts/publishable-members.sh` lists them), not just the ones the diagnostic above reported as `published`: that reading can be stale, and skipping a member because it read `absent` risks leaving one that actually did publish un-yanked. crates.io's own response when you try is authoritative instead — a member that was never actually published at this version returns "version does not exist", which is the only acceptable no-op here; anything else means the yank did real work.

```sh
for c in $(.github/scripts/publishable-members.sh); do
  cargo yank "$c@X.Y.Z"
done
```

Capture the commit before deleting the draft. Read `SHA` from the failed run's own **"Verify draft release exists and resolve commit SHA"** step — it prints `Resolved vX.Y.Z -> <sha>` to both the log and the job summary — not from the live draft's target field: the draft can be edited after the run resolved it, and re-querying it here would tag a commit nobody actually built or uploaded.

```sh
SHA=<commit_sha from the failed run's "Verify draft release exists and resolve commit SHA" step>
git fetch origin &&
  git tag "vX.Y.Z" "$SHA" &&
  git push origin "vX.Y.Z" &&
  gh release delete "vX.Y.Z" --yes   # NOT --cleanup-tag: that deletes the tag just pushed
```

The tag has to be created by hand because only the final GitHub-release flip creates it, and that never ran — so the newest tag is still `vX.Y.(Z-1)` and `cargo xtask version --check` would reject `X.Y.(Z+1)` as a two-step jump, blocking the bump commit both locally and in Lint.

Then bump every publishable member's manifest to `X.Y.(Z+1)` (`.github/scripts/publishable-members.sh` lists them; today that is `Cargo.toml`, `macros/Cargo.toml` and `cargo-skuld/Cargo.toml`), fix the root cause if there is one, and re-run both workflows with the new version. Because the bump is lockstep, every publishable member moves to `X.Y.(Z+1)` together regardless of how far `X.Y.Z` got for each one individually — a yanked or never-reached `X.Y.Z` in one crate's version line is the accepted cost of a shared workspace version, not a problem to work around.

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

## Workflow conventions

- **Each job's first step is the checkout that provides what its steps run.** A step that runs a `.github/scripts/` script or a local `./` action needs the repo on disk. Any further checkout (sparse, into a subfolder, or at another ref) goes as early as its inputs and gates allow: directly after the first, unless its `ref` depends on an earlier step's output or it must wait for a gate, as `resolve`'s draft-target checkout waits for the target lookup. It always goes before any step that uses it. Remember that a later full-workspace checkout wipes an earlier subfolder checkout. Stage 2 (`publish-release.yaml`) cannot be exercised end to end without publishing, so a step there that runs before its checkout only fails during a real release, as [publish run 36659127833](https://github.com/bindreams/skuld/actions/runs/36659127833) did. Stage 1 is reversible and can be dispatched to test. This is a convention, deliberately not a CI check.
