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
- **The filesystem holding `target/` must support a blocking advisory file
  lock (`flock` on Unix, `LockFileEx` on Windows)**, since `connect()` and
  `open_db()` now serialize the coordination DB's creation, publication, and
  schema initialization through one on a sibling `.skuld.db.lock` file (see
  below). Every mainstream local filesystem and every filesystem this crate
  otherwise supports (see the two bullets above) already does.

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
  - A lazily-initialized Process-scoped fixture is now built on the trial
    thread that first requests it, rather than on the dispatching thread.
    A resource whose setup ties itself to _the creating thread_, not just
    to the value it returns — a child started with `PR_SET_PDEATHSIG`,
    which dies with its parent thread — would die when that trial's
    thread joins, breaking every later trial that reuses the cached
    value. This is libtest's own behavior too (it also runs each test on
    its own thread), not new to skuld. Use `warm_up` from `main()` for
    such thread-bound resources, so setup runs on the long-lived main
    thread instead; see `fixture::warm_up`'s docs.
- **A failing fixture setup now fails a `should_panic` test.** Fixture
  resolution (`enter_test_scope` plus each `#[fixture]` parameter's
  `fixture_get`) now runs _before_ `catch_unwind`, not inside it. Previously,
  a fixture whose setup panicked would satisfy `should_panic`, masking a
  broken fixture as a passing test. Every fixture handle and the test scope
  guard still move into `catch_unwind` together, in declaration order, so
  teardown timing and order for a satisfied `should_panic` test match the
  plain (non-`should_panic`) case: a `Drop` impl sees
  `std::thread::panicking() == true`, and a Variable-scoped fixture that
  depends on a Test-scoped one still drops before the Test-scoped one is
  reclaimed. A panic from teardown itself (a fixture `Drop`, or `__scope`'s
  reclaim) is no longer mistaken for the expected one either: if the test
  body returns normally, any panic `catch_unwind` then catches is resumed
  rather than counted as satisfying `should_panic`, so a test whose body
  never panics still fails when a fixture's `Drop` does — for both
  `should_panic` and `should_panic = "..."`, which now share one generated
  code path instead of two hand-mirrored copies.
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
- **An async test's fixture setup and teardown now run outside `block_on`,
  under an explicit `Runtime::enter()` guard instead.** Previously, setup and
  teardown were part of the same async block `block_on` drove, so they got a
  live runtime for free but ran too late relative to `should_panic`'s
  `catch_unwind` (see above) to let a failing setup fail the test correctly.
  Moving them out of `block_on` fixed that, but would otherwise have broken
  a synchronous `#[fixture]` constructor or `Drop` impl that calls
  `Handle::current()` — "there is no reactor running" — since neither runs
  under `block_on`'s runtime context anymore. `Runtime::enter()`'s guard is
  now held for the whole closure — setup, the `block_on`'d call, and
  teardown, in that order — so all three still see a live runtime.
- **On Windows, stale-entry cleanup now treats `OpenProcess` failing with
  anything other than `ERROR_INVALID_PARAMETER` (a nonexistent PID) as "the
  process exists but we can't query it," not as dead** — `ERROR_ACCESS_DENIED`
  included, e.g. a process owned by another user or a protected/elevated
  one. Matches Unix's existing `EPERM` handling, which already treats
  "exists but not signalable" as alive rather than assuming a permission
  failure means gone. **Known hang, not fixed here:** if a `running` row's
  PID has since been reused by a process this uid can't `OpenProcess`,
  cleanup can never delete that row, and a later test whose serial filter
  conflicts with it blocks in `coordinate`'s wait loop for as long as the
  unrelated process lives — potentially indefinitely. Same class of problem
  as Unix's `EPERM` case. Fixing it needs a way to distinguish "this PID,
  this instance" from "this PID, reused by someone else," which will be
  addressed together with the existing cross-namespace `is_pid_alive`
  tracking issue.

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
  - `connect()` and `open_db()` now both run under a blocking, cross-process
    advisory lock (`flock` on Unix, `LockFileEx` on Windows, via
    `std::fs::File`'s own native `lock`/`unlock`) on a sibling
    `.skuld.db.lock` file, held for the whole create-or-open-and-initialize
    sequence. Whoever holds it is the only actor in the system allowed to
    create, publish, or schema-initialize `.skuld.db` at that instant. On
    Windows, `connect()` still skips the `.skuld.db` publish step entirely
    (there's no uid-mixing hazard to guard against there) but takes the
    same init lock as every other platform, since `open_db`'s WAL
    negotiation race is cross-platform.
  - The lock file itself is published at 0666 the same way `.skuld.db` is,
    and opened read-only (`flock`/`LockFileEx` only need read access on the
    handle) — a lock file opened read-write, at whatever mode a plain
    `open(O_CREAT)` gave it under the active umask, would reintroduce
    exactly the lockout publishing `.skuld.db` itself exists to prevent, one
    level down.
  - Only creation needs mode and no-replace-rename support: `ensure_published`
    checks for an existing `.skuld.db` first (`lstat`, so a dangling symlink
    counts as "already there" too, matching the rename's own `EEXIST`
    handling below) and returns immediately if so. The first process to see
    an absent `.skuld.db` creates a private
    `.skuld-publish-<pid>-<nanos>-<seq>.tmp` (outside the `.skuld.db*`
    glob), `fchmod`s it 0666, and publishes it with an atomic no-replace
    rename (`renameat2(..., RENAME_NOREPLACE)` on Linux and Android,
    `renamex_np(..., RENAME_EXCL)` on macOS) — a lost race silently
    discards the loser's temp and uses the winner's file as-is, with no
    further checks. Every later connection, on every process, skips the
    create/fchmod/rename dance entirely via the existence check instead of
    repeating it just to hit an `EEXIST` no-op on the rename.
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
