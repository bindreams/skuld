# Changelog

All notable changes to this project are documented in this file.

## [Unreleased]

### Breaking

- **Trial names must be unique.** At startup, `TestRunner::run()` now panics if
  two trials (inventory-registered or dynamically added) resolve to the same
  final trial name — regardless of whether `libtest_names()` is enabled. The
  panic lists every offending name with all of its declaration sites.
  Previously, duplicate names were silently accepted and both trials ran and
  reported independently — libtest-mimic tracks results in a plain `Vec`, not
  by name — but two identically-named entries in the same summary is
  indistinguishable from one trial being reported twice.

  Downstream crates pinned to an older skuld release are unaffected until
  they upgrade, but must rename any duplicates before moving to this
  release.

- **The coordination database's atomic-publish step only builds on Linux
  (any libc), Android and macOS.** Every other Unix (FreeBSD, NetBSD,
  OpenBSD, illumos, iOS) fails to compile, with a `compile_error!` naming
  the missing no-replace rename primitive, rather than silently falling
  back to an unprotected `open(O_CREAT)`. This is a deliberate restriction
  of this release's scope, not an oversight.
- **A `target/` on a filesystem that doesn't keep file modes (FAT/exFAT,
  vboxsf, some 9p/virtiofs setups) or has no atomic no-replace rename (some
  NFS configurations return `EINVAL` for `RENAME_NOREPLACE`) now panics the
  first time `.skuld.db` needs to be created**, where previously — with no
  atomic-publish step at all — `.skuld.db` was created with a plain
  `open(O_CREAT)` and whatever mode/rename semantics the filesystem gave it,
  no matter how limited.

### Changed

- **Trials now run on a fresh, named thread instead of the dispatching
  thread.** Each trial spawns on a `thread::Builder` named after the trial,
  joins, and re-raises any panic payload with
  `resume_unwind` so libtest-mimic still reports it normally. This closes a
  cross-test leak: skuld's default (capture-enabled) mode forces
  libtest-mimic's `--test-threads=1`, under which every trial previously ran
  sequentially on the _same_ thread — the real main thread. A
  `thread_local!` left set by one trial was therefore always observable by
  the next, a real hazard for any test suite with more than a handful of
  `thread_local!`-backed fixtures. The spawned thread uses std's default
  stack size (2 MiB, honoring `RUST_MIN_STACK` like any other spawned
  thread) rather than matching the main thread's typically larger one; a
  deep-recursion trial that relied on the old main thread's bigger stack may
  need `RUST_MIN_STACK` set explicitly. A trial whose final name contains a
  NUL byte — whether from `#[skuld::test(name = "...")]` or a dynamically
  registered trial — now panics at the point it would have run, naming the
  trial, because `std::thread::Builder::spawn` rejects such names;
  previously it ran on the dispatching thread like any other and the NUL
  byte was never inspected.
- **A failing fixture setup now fails a `should_panic` test.** Fixture
  resolution (`enter_test_scope` plus each `#[fixture]` parameter's
  `fixture_get`) now runs _before_ `catch_unwind`, not inside it. Previously,
  a fixture whose setup panicked would satisfy `should_panic`, masking a
  broken fixture as a passing test.
- **`TestRegistration::drop` (the coordination DB's last connection per test)
  now panics loudly when its own `connect()` call fails**, instead of only
  `eprintln!`-warning. It goes through the same `connect()` helper as every
  other connection (see below), so a DB that's become unusable since
  registration (e.g. replaced by a directory, or a filesystem-level publish
  failure) is now surfaced as a hard failure, not
  silently swallowed — _except_ when the test's own body already panicked
  (or is otherwise unwinding) and this drop is running as part of that
  unwind: a second, uncaught panic during an active unwind is Rust's "panic
  in a destructor during cleanup", which aborts the whole process
  (`SIGABRT`) rather than just failing the one test. In that one case the DB
  failure is downgraded to a loud `eprintln!` warning instead of a panic, so
  one broken test can never take the rest of the run down with it.
- **An async test's fixture setup and teardown now run inside the test's
  tokio runtime context (`Runtime::enter()`), not just the `block_on`'d test
  call.** Previously, only the async body itself ran under the runtime;
  fixture setup ran before `block_on` and teardown after it returned, so a
  synchronous `#[fixture]` constructor or `Drop` impl that called
  `Handle::current()` would panic with "there is no reactor running". Now
  the runtime guard is held for the whole closure — setup, call and
  teardown, in that order — so both see a live runtime.

### Added

- **The coordination database (`.skuld.db`) is now published atomically at
  mode 0666 on Unix**, so a second uid (for example, an unprivileged step
  following a root lane) can open it. Root only ever runs this in CI
  containers or a throwaway VM — running it as root elsewhere is not a
  supported workflow; given that, the only requirement is that whichever
  uid gets there first leaves the file world-writable — there's no
  verification of an existing file, no ownership check, and no cleanup of
  abandoned publish temps, since none of that is load-bearing once
  disturbing another local user's test scheduling is the entire downside.
  This only protects a `.skuld.db` this release creates fresh: one left
  behind by an older skuld (mode 0644 by default) is used as-is, same as
  any other pre-existing file — delete it once after upgrading if you need
  the new mode. The publish step itself is Unix-only — no Windows lane
  mixes uids — but every connection now goes through one `connect()`
  helper (see below), which applies on every platform, Windows included.
  - On Unix, `connect()` calls `ensure_published` before running any SQL, so
    every connection — `open_db` and `TestRegistration::drop` alike — sees a
    `.skuld.db` already at 0666. On Windows, `connect()` skips that call
    (there's no uid-mixing hazard to guard against) and only provides
    `Connection::open`'s panic-on-failure wrapper, same as every platform.
  - The first process to see an absent `.skuld.db` creates a private
    `.skuld-publish-<pid>-<nanos>-<seq>.tmp` (outside the `.skuld.db*`
    glob), `fchmod`s it 0666, and publishes it with an atomic no-replace
    rename (`renameat2(..., RENAME_NOREPLACE)` via a raw `syscall()` on
    Linux and Android — going straight to the kernel sidesteps every
    libc's own version floor for the `renameat2` _wrapper_ symbol: glibc
    only exports it from 2.28, musl only from 1.2.6 (newer than what Rust's
    own bundled musl target links against), and uclibc never exports it at
    all — `renamex_np(..., RENAME_EXCL)` on macOS) — a lost race silently
    discards the loser's temp and uses the winner's file as-is, with no
    further checks.
  - The `-wal`/`-shm` companions are not pre-created or `fchmod`ed by
    Skuld at all: SQLite's own Unix VFS derives their mode from the main
    DB file's already-0666 mode, so once `.skuld.db` is published they come
    out 0666 on their own, umask or not — verified under a restrictive
    umask in `tests/coordination_publish_cli.rs`.
- **`TestRunner::libtest_names()`**: an opt-in builder method that reports
  each trial under its `<module path minus the crate name>::<test name>`
  instead of the bare test name, matching `cargo test`'s own libtest naming.
  Off by default, so existing trial names are unaffected until a caller
  opts in. The duplicate trial name check above runs unconditionally
  either way, over whichever final names are actually in effect.
