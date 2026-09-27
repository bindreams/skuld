//! Test-only coordination for the fixture workspace's target directory,
//! shared by `metadata::metadata_tests`, `discovery::discovery_tests`, and
//! the `tests/gen_and_run.rs` integration test binary.
//!
//! Lives under `src/`, not `tests/support/`: this package's own
//! `exclude = ["tests/**"]` drops `tests/**` from what's published, so a
//! `#[path]`-included file living there compiles locally but is missing
//! from the published crate, breaking `cargo test` against it (verified on
//! the extracted `.crate`). Compiled into two separate crates: this
//! crate's own lib, as a plain `mod test_support;` (`src/lib.rs`), and
//! `tests/gen_and_run.rs`, a different crate, via
//! `#[path = "../src/test_support.rs"]` — neither has any business making
//! this crate's `pub` API, so everything here is `pub(crate)` (within its
//! own crate) with `#![allow(dead_code)]` for whichever half of the API
//! the other crate doesn't use.
//!
//! # Why a lock at all
//!
//! Serializes every `cargo-skuld` test that builds, lists, or runs against
//! `cargo-skuld/tests/fixtures/test-workspace` (`metadata_tests`,
//! `discovery_tests`, and `gen_and_run.rs` — `cargo nextest run`, unlike
//! plain `cargo test`, runs every test binary in one concurrent pool, so
//! all three need it, not just two). Two independent races make this
//! necessary:
//!
//! - On macOS, `cargo` deletes and re-creates every flat
//!   `target/debug/<bin>` convenience path on *every* invocation that
//!   touches a workspace, even a no-op `cargo nextest list`. A concurrent
//!   invocation spawning that same flat path (a discovered binary, a
//!   `CARGO_BIN_EXE_*` path baked in by `env!`) can observe it mid-delete:
//!   `failed to spawn ".../target/debug/<bin>": No such file or directory
//!   (os error 2)`. Robustly demonstrated for the outer `skuld`/
//!   `cargo-skuld` workspace's own `target/`: `discovery_tests` used to
//!   list the repo root, racing `skuld`'s own `tests/*_cli.rs`; pointing it
//!   at the fixture instead (see `discovery_tests.rs`) took a stress run
//!   from 2/40 failing to 40/40 passing.
//! - For the fixture's own `target/` specifically — mutual exclusion
//!   between `metadata_tests`/`discovery_tests` and `gen_and_run.rs`'s
//!   builds — the evidence is weaker and worth stating honestly: a
//!   lockless mutant didn't fail in one round of testing (~200 runs), but
//!   did fail in another, independent round, within roughly 60 runs of
//!   `cargo test -p cargo-skuld --lib` under 8-way parallelism (a spawned
//!   fixture binary exited with no status code at all — killed by a
//!   signal, consistent with being overwritten mid-exec by a concurrent
//!   build of the same package). Real, but narrower and less reliably hit
//!   than the outer-`target/` race above; no specific run count is claimed
//!   as reproducing it on demand. `test_support_tests.rs`'s
//!   `lock_fixture_workspace_really_locks_something_a_second_handle_is_
//!   excluded_from` guards the mechanism directly instead, deterministically.
//!
//! An in-process `Mutex` isn't enough for either race, since both
//! reproduce across separate OS processes, not just threads. Locking beats
//! giving each test its own isolated `CARGO_TARGET_DIR` on cost, not
//! correctness: a stale fixture `Cargo.lock` gets rewritten in place
//! regardless of where `CARGO_TARGET_DIR` points, so CI's "Fixture lock is
//! current" check isn't what's at stake — wall time is: a cold `cargo
//! nextest list` against this fixture takes ~7s wall / ~31s CPU here,
//! ~0.2s warm, paid again per test if isolated across the 16 that touch it.
//!
//! This lock is *not* what stopped CI run 36320721947 — that was a
//! different race, closed separately by `build.rs` declaring its own
//! `rerun-if-changed` and by `cargo-skuld/tests/fixtures/test-workspace/
//! .gitignore`'s `/target*` wildcard (see those files). This lock also
//! doesn't cover `tests/compile_errors.rs`'s trybuild run, which builds
//! `skuld` fresh under its own `CARGO_TARGET_DIR` with no knowledge of the
//! fixture at all — the `.gitignore` fix is the one that covers that case
//! too.
//!
//! # Design of the lock itself
//!
//! `fixture_target_dir` resolves the fixture's real `target_directory` via
//! `cargo metadata`, run with the fixture as cwd, rather than assuming
//! `<fixture>/target`: cargo resolves a *relative* `CARGO_TARGET_DIR` (and
//! `.cargo/config.toml` discovery) against the current directory, not a
//! `--manifest-path` argument, and an inherited override can redirect the
//! fixture's build output elsewhere (`build_and_locate_broken_binary`
//! already has to account for the same inheritance).
//!
//! The lock *file* lives inside the target directory, once it exists, not
//! beside it and not under the system temp dir: opening
//! `target_dir.join(".skuld-fixture.lock")` resolves through a symlinked
//! `CARGO_TARGET_DIR` exactly the way opening any other file under it
//! would, and `flock`/`LockFileEx` contend on the resulting file's
//! identity, not the path string used to reach it — so two different
//! spellings of the same physical target directory still lock each other
//! out correctly, with no name derivation or canonicalization needed. A
//! lock file living beside the target directory instead would need both of
//! those (to keep two lock files from aliasing) and would need that
//! directory's *parent* to be writable — a strictly stronger requirement
//! than anything a normal build against the fixture already needs.
//!
//! Creating the target directory ourselves, rather than letting cargo find
//! it missing and initialize it, risks losing what cargo's own
//! initialization does: writing `CACHEDIR.TAG`, marking the directory
//! excluded from Time Machine and iCloud sync on macOS, and (on Windows)
//! setting `FILE_ATTRIBUTE_NOT_CONTENT_INDEXED` to exclude it from content
//! indexing — cargo skips all of that for a directory it finds already
//! exists, which a naive `create_dir_all` here would leave to a subsequent
//! cargo build to discover it didn't need to do. So `ensure_target_dir`
//! doesn't hand-roll any of that; it calls `cargo_util::paths::
//! create_dir_all_excluded_from_backups_atomic`, the exact function cargo
//! itself calls for this — same atomic create-under-a-temp-name-then-
//! rename, same marks, same `CACHEDIR.TAG` content, and, per its own doc,
//! already idempotent and safe under concurrent callers (including cargo
//! itself, mid-build): "won't exclude `p` from cache if it already
//! exists."
#![allow(dead_code)]
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/test-workspace")
}

/// Resolves the fixture workspace's real `target_directory` — honoring an
/// inherited `CARGO_TARGET_DIR`/`.cargo/config.toml` override exactly the
/// way a `cargo` invocation against the fixture would — rather than
/// assuming `<fixture>/target`. Cargo resolves a *relative*
/// `CARGO_TARGET_DIR` (and `.cargo/config.toml` discovery) against the
/// current directory, not the manifest path, so this must run with the
/// fixture as its cwd — same as `build_and_locate_broken_binary`'s own
/// `cargo build` does — or it can resolve a different directory than the
/// one the fixture's actual builds use.
fn fixture_target_dir() -> PathBuf {
    let output = Command::new("cargo")
        .current_dir(fixture_root())
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .unwrap_or_else(|e| panic!("failed to run `cargo metadata` for the fixture workspace: {e}"));
    assert!(
        output.status.success(),
        "`cargo metadata` for the fixture workspace failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("`cargo metadata` did not return valid JSON: {e}"));
    PathBuf::from(
        parsed["target_directory"]
            .as_str()
            .unwrap_or_else(|| panic!("`cargo metadata` output has no target_directory: {parsed}")),
    )
}

/// Creates `target_dir` (and any missing ancestors) if it doesn't exist
/// yet, marked exactly the way cargo itself would mark a target directory
/// it finds missing — see the module doc for why this doesn't hand-roll
/// that instead.
pub(crate) fn ensure_target_dir(target_dir: &Path) {
    cargo_util::paths::create_dir_all_excluded_from_backups_atomic(target_dir)
        .unwrap_or_else(|e| panic!("failed to create fixture target dir {target_dir:?}: {e}"));
}

/// The fixture lock file's path once `target_dir` exists — see the module
/// doc for why it lives here rather than beside the target directory or
/// under the system temp dir.
pub(crate) fn lock_file_path(target_dir: &Path) -> PathBuf {
    target_dir.join(".skuld-fixture.lock")
}

/// See the root `Cargo.toml`'s comment on why Unix goes through
/// `rustix::fs::flock` instead of `std::fs::File::lock`/`try_lock`: those
/// aren't implemented on every Unix target `std` otherwise treats as
/// `flock`-capable.
#[cfg(unix)]
pub(crate) fn lock_exclusive(file: &File) -> std::io::Result<()> {
    loop {
        match rustix::fs::flock(file, rustix::fs::FlockOperation::LockExclusive) {
            Ok(()) => return Ok(()),
            Err(rustix::io::Errno::INTR) => continue,
            Err(errno) => return Err(errno.into()),
        }
    }
}

#[cfg(windows)]
pub(crate) fn lock_exclusive(file: &File) -> std::io::Result<()> {
    file.lock()
}

#[cfg(unix)]
pub(crate) fn try_lock_exclusive(file: &File) -> Result<(), std::fs::TryLockError> {
    match rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(()),
        Err(errno) => {
            let err: std::io::Error = errno.into();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                Err(std::fs::TryLockError::WouldBlock)
            } else {
                Err(std::fs::TryLockError::Error(err))
            }
        }
    }
}

#[cfg(windows)]
pub(crate) fn try_lock_exclusive(file: &File) -> Result<(), std::fs::TryLockError> {
    file.try_lock()
}

/// A held exclusive lock on the fixture workspace's target directory, plus
/// the two paths that were resolved to acquire it. This is the *only* way
/// any caller can get at the fixture's root or target directory: there is
/// deliberately no free-standing accessor for either, so a test that needs
/// one has no way to get it without going through an already-acquired
/// guard first. A prior version exposed `fixture_root()` as a free
/// function, and every one of its four call sites (`metadata_tests.rs`,
/// `discovery_tests.rs`, `gen_and_run.rs`, and this file) could read the
/// fixture's path without ever acquiring the lock — "forgot to lock" was a
/// silent, latent race instead of a compile error.
///
/// Releases the lock on drop, via `_lock`'s own `Drop` — `flock`/
/// `LockFileEx` are released when the last handle to the file closes, so
/// this needs no `Drop` impl of its own. Hold this for as long as any path
/// obtained from the target directory (a discovered binary, a
/// built-and-located binary) might still be read or executed — dropping it
/// early re-opens the race this exists to close.
pub(crate) struct FixtureGuard {
    root: PathBuf,
    target_dir: PathBuf,
    _lock: File,
}

impl FixtureGuard {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn target_dir(&self) -> &Path {
        &self.target_dir
    }
}

/// Blocks until exclusive access to the shared fixture workspace's target
/// directory is acquired, then returns a guard that releases it on drop —
/// see `FixtureGuard`'s own doc for why that guard is the only way to
/// reach the fixture's root or target directory at all.
#[must_use]
pub(crate) fn lock_fixture_workspace() -> FixtureGuard {
    let root = fixture_root();
    let target_dir = fixture_target_dir();
    ensure_target_dir(&target_dir);
    let path = lock_file_path(&target_dir);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("failed to open fixture lock file {path:?}: {e}"));
    lock_exclusive(&file).unwrap_or_else(|e| panic!("failed to acquire fixture lock {path:?}: {e}"));
    FixtureGuard {
        root,
        target_dir,
        _lock: file,
    }
}
