//! Per-test output capture via file-descriptor redirection.
//!
//! Each test runs under an [`FdCapture`] guard that redirects `stdout` and
//! `stderr` to an in-process pipe. The runner spawns a reader thread to
//! drain the pipe into a `Vec<u8>`; on failure the bytes are dumped to the
//! saved real stderr, on pass they are discarded.
//!
//! Capture is keyed on libtest-mimic's `--nocapture` flag. When running
//! under `cargo test` (flag unset), [`crate::runner`] forces
//! `test_threads=1` and wraps each test body in [`FdCapture::begin`]. When
//! running under `--nocapture` or `cargo nextest run` (which always passes
//! `--nocapture`), `begin` is never called and nothing intercepts the
//! test's stdio.
//!
//! # Why file descriptors, not a tracing subscriber
//!
//! The previous implementation installed a thread-local
//! `tracing_subscriber::fmt` layer via `tracing::subscriber::set_default`.
//! That approach silently clobbered any subscriber a test installed
//! itself, because `set_default` is thread-local and takes precedence
//! over `set_global_default`. Capturing at the FD layer removes Skuld
//! from the tracing dispatch path entirely: user subscribers work exactly
//! as they would under plain `#[test]`.
//!
//! # Cross-platform story
//!
//! On Unix, `libc::dup2` is enough because Rust's stdio, C libraries, and
//! the kernel all share one file descriptor table.
//!
//! On Windows, Rust's `io::stdout`/`io::stderr` bypass the C runtime fd
//! table and write through `GetStdHandle` + `WriteFile` directly, while
//! C libraries use the CRT fd table (`_write(1, ...)`). Redirecting only
//! one of the two leaves the other silently escaping. We swap both: the
//! Win32 console handle via `SetStdHandle` and the CRT fd via
//! `libc::_dup2`. Subprocess inheritance piggy-backs on `SetStdHandle`
//! since `CreateProcess` snapshots `STD_OUTPUT_HANDLE` into
//! `STARTUPINFO.hStdOutput` at spawn time.
//!
//! # Reader thread
//!
//! The pipe's read end is drained by a dedicated thread spawned during
//! `begin`. Without it, a test writing more than the pipe's buffer size
//! (as small as ~4 KiB on Windows, ~64 KiB on Linux) before the swap-back
//! would block on `write`. The reader thread receives `PipeReader` by
//! move; once `end` restores the saved fds, the process's last references
//! to the pipe write end are closed and the reader sees EOF.
//!
//! # Contract with callers
//!
//! * **Single-threaded only.** The FD redirect is process-wide on both
//!   Unix and Windows. Running two captures concurrently, or running
//!   non-captured parallel tests alongside a captured one, is unsupported
//!   and will interleave output. `crate::runner` enforces this by forcing
//!   `--test-threads=1` whenever capture is enabled.
//!
//! * **No long-lived child processes.** A test body may spawn child
//!   processes, but the test MUST wait for them to exit (e.g. via
//!   `Command::status()` or `Command::output()`) before returning. Child
//!   processes inherit fds 1/2, which point at the pipe during capture;
//!   until the child exits, the pipe's write end stays open and the
//!   reader thread cannot see EOF. [`FdCapture::end`] will then block on
//!   `JoinHandle::join` waiting for a reader that is waiting for a child
//!   that is waiting for something the parent test is no longer
//!   monitoring. This is a constraint, not a bug: the capture layer
//!   cannot distinguish "reader is stuck because of orphaned child" from
//!   "reader is stuck because of legitimate slow I/O".
//!
//! * **FdCapture::end must be called.** The [`Drop`] impl is a safety
//!   net for exceptional unwind paths and performs best-effort restore,
//!   but the happy path runs through [`FdCapture::end`]. If restore
//!   fails at any point during `end` or `Drop`, the process is aborted
//!   (see [`abort_with_broken_stdio`]) because continuing with undefined
//!   stdio state would corrupt every subsequent test's output and
//!   potentially leak sensitive data into the wrong buffers.

use std::io::{self, Write};
use std::thread::{self, JoinHandle};

use os_pipe::{PipeReader, PipeWriter};

// Abort helper =========================================================================================

/// Aborts the process after a failure to restore stdio. Writes a
/// diagnostic to the saved file descriptor (bypassing `io::stderr()`,
/// which may be redirected to a closed pipe by this point) and then
/// calls `std::process::abort()`.
///
/// This is called when a restore operation on fd 1/fd 2 fails partway
/// through and we can no longer guarantee that stdio is in a usable
/// state. Continuing would corrupt every subsequent test's output and
/// is worse than a clean abort.
fn abort_with_broken_stdio(context: &str, err: &io::Error) -> ! {
    // Best-effort write to a known-good fd. On Unix, fd 2 *might* still
    // work even mid-restore; on Windows the same. If it doesn't, the
    // abort will still fire.
    let msg = format!(
        "\n[skuld] FATAL: capture restore failed at {context}: {err}\n\
         [skuld] FATAL: stdio is in an unknown state; aborting to avoid \
         corrupting subsequent tests.\n"
    );
    #[cfg(unix)]
    unsafe {
        let _ = libc::write(libc::STDERR_FILENO, msg.as_ptr() as *const libc::c_void, msg.len());
    }
    #[cfg(windows)]
    unsafe {
        let _ = libc::write(2, msg.as_ptr() as *const libc::c_void, msg.len() as u32);
    }
    std::process::abort();
}

// Platform shims =======================================================================================

#[cfg(unix)]
mod sys {
    use super::*;
    use std::os::fd::AsRawFd;

    /// Saved state of the pre-capture stdio fds.
    pub(super) struct Saved {
        pub stdout: libc::c_int,
        pub stderr: libc::c_int,
    }

    /// Snapshot the current fd 1 / fd 2 so they can be restored later.
    pub(super) fn save() -> io::Result<Saved> {
        // SAFETY: STDOUT_FILENO / STDERR_FILENO are always open in a
        // normal process; `dup` returns the new fd or -1 on error.
        let stdout = unsafe { libc::dup(libc::STDOUT_FILENO) };
        if stdout < 0 {
            return Err(io::Error::last_os_error());
        }
        let stderr = unsafe { libc::dup(libc::STDERR_FILENO) };
        if stderr < 0 {
            let err = io::Error::last_os_error();
            unsafe { libc::close(stdout) };
            return Err(err);
        }
        Ok(Saved { stdout, stderr })
    }

    /// Redirect fd 1 and fd 2 at the pipe's write end. Consumes
    /// `writer`.
    ///
    /// Transactional: if the second `dup2` fails, the first is undone
    /// using `saved` before returning Err, so the caller observes a
    /// state identical to pre-call.
    pub(super) fn redirect_to_pipe(writer: PipeWriter, _live: &mut LiveWriters, saved: &Saved) -> io::Result<()> {
        let fd = writer.as_raw_fd();

        // Step 1: dup2(pipe, STDOUT).
        if unsafe { libc::dup2(fd, libc::STDOUT_FILENO) } < 0 {
            // Nothing redirected yet; the caller's Saved is unchanged.
            return Err(io::Error::last_os_error());
        }

        // Step 2: dup2(pipe, STDERR). If this fails, undo step 1.
        if unsafe { libc::dup2(fd, libc::STDERR_FILENO) } < 0 {
            let err = io::Error::last_os_error();
            // Undo step 1. If this also fails, stdio is unrecoverable.
            if unsafe { libc::dup2(saved.stdout, libc::STDOUT_FILENO) } < 0 {
                abort_with_broken_stdio(
                    "rollback of stdout after stderr dup2 failure",
                    &io::Error::last_os_error(),
                );
            }
            return Err(err);
        }

        // Dropping `writer` here closes its original fd. fd 1 / fd 2
        // retain their own references from `dup2`, so this does not
        // break the capture.
        drop(writer);
        Ok(())
    }

    /// Restore fd 1 / fd 2 from the saved snapshot.
    ///
    /// Attempts both restores even if the first fails; returns the
    /// first error (if any) with both attempts reflected in the
    /// process state.
    pub(super) fn restore(saved: &Saved) -> io::Result<()> {
        let mut first_err: Option<io::Error> = None;
        if unsafe { libc::dup2(saved.stdout, libc::STDOUT_FILENO) } < 0 {
            first_err.get_or_insert_with(io::Error::last_os_error);
        }
        if unsafe { libc::dup2(saved.stderr, libc::STDERR_FILENO) } < 0 {
            first_err.get_or_insert_with(io::Error::last_os_error);
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Drop the saved fds. Must be called after `restore` or after
    /// deciding to abandon the saved state.
    pub(super) fn close_saved(saved: Saved) {
        unsafe {
            libc::close(saved.stdout);
            libc::close(saved.stderr);
        }
    }

    /// Placeholder for Windows `LiveWriters`. Unix doesn't need to
    /// retain any writer clones post-swap, so this is a zero-sized
    /// type that keeps the cross-platform API symmetric.
    #[derive(Default)]
    pub(super) struct LiveWriters;
}

#[cfg(windows)]
mod sys {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};

    /// Saved state of the pre-capture stdio handles *and* CRT fds.
    pub(super) struct Saved {
        pub stdout_handle: HANDLE,
        pub stderr_handle: HANDLE,
        pub stdout_crt: libc::c_int,
        pub stderr_crt: libc::c_int,
    }

    /// Snapshot both the Win32 console handles and the CRT fds.
    pub(super) fn save() -> io::Result<Saved> {
        // SAFETY: GetStdHandle is thread-safe and takes no out-params.
        // Returns `Result<HANDLE>`; missing handles return Err. Headless
        // binaries (GUI subsystem, or spawned with CREATE_NO_WINDOW and
        // no explicit redirection) may not have valid stdio handles —
        // callers see that as an ordinary io::Error.
        let stdout_handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.map_err(io::Error::other)?;
        let stderr_handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) }.map_err(io::Error::other)?;

        let stdout_crt = unsafe { libc::dup(1) };
        if stdout_crt < 0 {
            return Err(io::Error::last_os_error());
        }
        let stderr_crt = unsafe { libc::dup(2) };
        if stderr_crt < 0 {
            let err = io::Error::last_os_error();
            unsafe { libc::close(stdout_crt) };
            return Err(err);
        }
        Ok(Saved {
            stdout_handle,
            stderr_handle,
            stdout_crt,
            stderr_crt,
        })
    }

    /// Redirect both Win32 stdio and the CRT fd table entries to the
    /// pipe's write end. Transactional: if any step fails, all prior
    /// steps are undone before returning Err.
    ///
    /// On success, `live` is populated with the two `PipeWriter` clones
    /// whose handles are now referenced by `STD_OUTPUT_HANDLE` and
    /// `STD_ERROR_HANDLE`. These MUST stay alive until `restore` has
    /// reassigned the global handles — otherwise the Win32 stdio slots
    /// would dangle.
    pub(super) fn redirect_to_pipe(writer: PipeWriter, live: &mut LiveWriters, saved: &Saved) -> io::Result<()> {
        // Phase 1: allocate all resources. Nothing global has been
        // touched yet, so failure here is a clean return.
        let w_stdout_handle = writer.try_clone()?;
        let w_stderr_handle = writer.try_clone()?;
        let w_stdout_crt = writer.try_clone()?;
        let w_stderr_crt = writer.try_clone()?;
        drop(writer);

        // Convert the CRT-destined writers into CRT fds. `_open_osfhandle`
        // transfers ownership of the raw handle to the CRT. On failure,
        // we must wrap the leaked raw handle back into an `OwnedHandle`
        // to close it, because `into_raw_handle` already released the
        // Rust-side owner.
        let crt_stdout_raw = w_stdout_crt.into_raw_handle();
        let crt_stdout_fd = unsafe { libc::open_osfhandle(crt_stdout_raw as libc::intptr_t, 0) };
        if crt_stdout_fd < 0 {
            // Safety: `crt_stdout_raw` is a valid HANDLE we just
            // unwrapped from PipeWriter; reconstituting OwnedHandle and
            // letting it drop is the documented way to close it.
            unsafe { drop(OwnedHandle::from_raw_handle(crt_stdout_raw)) };
            return Err(io::Error::last_os_error());
        }

        let crt_stderr_raw = w_stderr_crt.into_raw_handle();
        let crt_stderr_fd = unsafe { libc::open_osfhandle(crt_stderr_raw as libc::intptr_t, 0) };
        if crt_stderr_fd < 0 {
            let err = io::Error::last_os_error();
            unsafe { libc::close(crt_stdout_fd) };
            unsafe { drop(OwnedHandle::from_raw_handle(crt_stderr_raw)) };
            return Err(err);
        }

        // Phase 2: commit the global mutations. Each successful step
        // adds to `rollback`'s "to undo" list. On any failure, `rollback`
        // goes out of scope WITHOUT being committed and its Drop undoes
        // the state.
        //
        // Ordering matters: we must mirror the order in `restore` so
        // that a partial rollback leaves the process in a
        // previous-captures-still-valid state.
        let mut rollback = WinRollback {
            saved,
            stdout_handle_set: false,
            stderr_handle_set: false,
            stdout_dup2_set: false,
            stderr_dup2_set: false,
            committed: false,
        };

        let stdout_raw = HANDLE(w_stdout_handle.as_raw_handle() as *mut _);
        if let Err(e) = unsafe { SetStdHandle(STD_OUTPUT_HANDLE, stdout_raw) } {
            unsafe { libc::close(crt_stdout_fd) };
            unsafe { libc::close(crt_stderr_fd) };
            return Err(io::Error::other(e));
        }
        rollback.stdout_handle_set = true;

        let stderr_raw = HANDLE(w_stderr_handle.as_raw_handle() as *mut _);
        if let Err(e) = unsafe { SetStdHandle(STD_ERROR_HANDLE, stderr_raw) } {
            unsafe { libc::close(crt_stdout_fd) };
            unsafe { libc::close(crt_stderr_fd) };
            return Err(io::Error::other(e));
        }
        rollback.stderr_handle_set = true;

        if unsafe { libc::dup2(crt_stdout_fd, 1) } < 0 {
            let err = io::Error::last_os_error();
            unsafe { libc::close(crt_stdout_fd) };
            unsafe { libc::close(crt_stderr_fd) };
            return Err(err);
        }
        rollback.stdout_dup2_set = true;

        if unsafe { libc::dup2(crt_stderr_fd, 2) } < 0 {
            let err = io::Error::last_os_error();
            unsafe { libc::close(crt_stdout_fd) };
            unsafe { libc::close(crt_stderr_fd) };
            return Err(err);
        }
        rollback.stderr_dup2_set = true;

        // `dup2` duplicated the kernel handle refs; the intermediate
        // CRT fds are no longer needed.
        unsafe { libc::close(crt_stdout_fd) };
        unsafe { libc::close(crt_stderr_fd) };

        // Commit — disarm the rollback.
        rollback.committed = true;
        drop(rollback);

        // Stash the Win32-stdio-backing writers so they outlive the
        // swap. Dropping them before `restore` reassigns SetStdHandle
        // would leave the Win32 slot dangling.
        live.stdout_handle_writer = Some(w_stdout_handle);
        live.stderr_handle_writer = Some(w_stderr_handle);
        Ok(())
    }

    /// Drop guard that undoes partial-redirect mutations to Win32 stdio
    /// and the CRT fd table. On any `return Err` path from
    /// `redirect_to_pipe`, this guard goes out of scope and its `Drop`
    /// restores whatever was set before the error.
    struct WinRollback<'a> {
        saved: &'a Saved,
        stdout_handle_set: bool,
        stderr_handle_set: bool,
        stdout_dup2_set: bool,
        stderr_dup2_set: bool,
        committed: bool,
    }

    impl Drop for WinRollback<'_> {
        fn drop(&mut self) {
            if self.committed {
                return;
            }
            // Undo in reverse order. Any failure here means stdio is
            // unrecoverable — abort the process rather than silently
            // continue with corrupted state.
            if self.stderr_dup2_set && unsafe { libc::dup2(self.saved.stderr_crt, 2) } < 0 {
                abort_with_broken_stdio("rollback of stderr CRT dup2", &io::Error::last_os_error());
            }
            if self.stdout_dup2_set && unsafe { libc::dup2(self.saved.stdout_crt, 1) } < 0 {
                abort_with_broken_stdio("rollback of stdout CRT dup2", &io::Error::last_os_error());
            }
            if self.stderr_handle_set {
                if let Err(e) = unsafe { SetStdHandle(STD_ERROR_HANDLE, self.saved.stderr_handle) } {
                    abort_with_broken_stdio("rollback of STD_ERROR_HANDLE", &io::Error::other(e));
                }
            }
            if self.stdout_handle_set {
                if let Err(e) = unsafe { SetStdHandle(STD_OUTPUT_HANDLE, self.saved.stdout_handle) } {
                    abort_with_broken_stdio("rollback of STD_OUTPUT_HANDLE", &io::Error::other(e));
                }
            }
        }
    }

    /// Restore both Win32 stdio and CRT fd 1/2 to the saved snapshot.
    ///
    /// Attempts all four restores even if some fail; returns the first
    /// error. The caller MUST treat any Err return as fatal: stdio is
    /// in an unknown state and the process must abort.
    pub(super) fn restore(saved: &Saved) -> io::Result<()> {
        let mut first_err: Option<io::Error> = None;
        if let Err(e) = unsafe { SetStdHandle(STD_OUTPUT_HANDLE, saved.stdout_handle) } {
            first_err.get_or_insert_with(|| io::Error::other(e));
        }
        if let Err(e) = unsafe { SetStdHandle(STD_ERROR_HANDLE, saved.stderr_handle) } {
            first_err.get_or_insert_with(|| io::Error::other(e));
        }
        if unsafe { libc::dup2(saved.stdout_crt, 1) } < 0 {
            first_err.get_or_insert_with(io::Error::last_os_error);
        }
        if unsafe { libc::dup2(saved.stderr_crt, 2) } < 0 {
            first_err.get_or_insert_with(io::Error::last_os_error);
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Close the saved CRT fds. Win32 handles were not owned by us —
    /// they were owned by whoever set them before capture started, so
    /// the plain `HANDLE` values in `saved` are dropped without
    /// `CloseHandle`.
    pub(super) fn close_saved(saved: Saved) {
        unsafe {
            libc::close(saved.stdout_crt);
            libc::close(saved.stderr_crt);
        }
    }

    /// Writers that must stay alive as long as the Win32 stdio HANDLEs
    /// point at them. Dropped after `restore` reassigns the HANDLEs to
    /// the saved values.
    #[derive(Default)]
    pub(super) struct LiveWriters {
        pub stdout_handle_writer: Option<PipeWriter>,
        pub stderr_handle_writer: Option<PipeWriter>,
    }
}

// FdCapture ============================================================================================

/// RAII handle for a single test's stdio capture. See the module
/// docstring for the contract with callers, in particular around child
/// processes and restore-failure semantics.
pub(crate) struct FdCapture {
    saved: Option<sys::Saved>,
    reader_handle: Option<JoinHandle<io::Result<Vec<u8>>>>,
    // Only populated on Windows; ZST on Unix. Must outlive the stdio
    // swap. On Windows, dropping this before `restore` has reassigned
    // SetStdHandle would dangle the global slots.
    #[cfg_attr(unix, allow(dead_code))]
    live: sys::LiveWriters,
}

impl FdCapture {
    /// Begin capturing stdout/stderr on the current thread. Returns a
    /// guard that must be released via [`FdCapture::end`] (happy path).
    /// [`Drop`] is a safety net only; callers that rely on it for
    /// cleanup are wrong.
    pub(crate) fn begin() -> io::Result<Self> {
        // Flush anything Rust has buffered on stdout/stderr before we
        // swap. We acquire the stdio locks for this so no other Rust
        // writer can `println!` concurrently with the swap and race us.
        //
        // Note: on Windows, stdio locks are advisory only at the Rust
        // level; they do not lock `STD_OUTPUT_HANDLE` against raw
        // `WriteFile` callers in FFI code. In capture mode the runner
        // forces `--test-threads=1`, so the only other potential
        // writers are background threads (panic hook, logger, tokio),
        // and those generally go through `io::stderr()`.
        let stdout = io::stdout();
        let stderr = io::stderr();
        let mut stdout_guard = stdout.lock();
        let mut stderr_guard = stderr.lock();
        let _ = stdout_guard.flush();
        let _ = stderr_guard.flush();

        let saved = sys::save()?;
        let (reader, writer) = os_pipe::pipe()?;

        let mut live = sys::LiveWriters::default();

        if let Err(e) = sys::redirect_to_pipe(writer, &mut live, &saved) {
            // `redirect_to_pipe` is transactional: on Err the process
            // state is as if the call never happened. We just need to
            // close the saved snapshot.
            sys::close_saved(saved);
            return Err(e);
        }

        // Locks released here (scope ends).
        drop(stderr_guard);
        drop(stdout_guard);

        // Spawn the drain thread. It must run concurrently with the
        // test body, or any write larger than the pipe buffer (~4 KiB
        // on Windows, ~64 KiB on Linux) would block the writer.
        let reader_handle = thread::Builder::new()
            .name("skuld-capture-drain".into())
            .spawn(move || drain_pipe(reader))
            .map_err(|e| io::Error::other(format!("spawning drain thread: {e}")))?;

        Ok(Self {
            saved: Some(saved),
            reader_handle: Some(reader_handle),
            live,
        })
    }

    /// Stop capturing, restore stdout/stderr, and return the captured
    /// bytes. After calling this, `Drop` is a no-op.
    ///
    /// If `restore` fails at the OS level (extraordinarily rare — would
    /// require a kernel-level error on `dup2`/`SetStdHandle`), this
    /// aborts the process rather than return: we cannot guarantee that
    /// any subsequent write to stdout/stderr lands at the intended
    /// destination, which is worse than terminating the test run.
    pub(crate) fn end(mut self) -> io::Result<Vec<u8>> {
        let saved = self
            .saved
            .take()
            .expect("FdCapture::end called on an already-ended capture");

        // Acquire locks around the restore swap so concurrent Rust
        // writers don't race with us.
        let stdout = io::stdout();
        let stderr = io::stderr();
        let mut stdout_guard = stdout.lock();
        let mut stderr_guard = stderr.lock();

        // Flush the test's last writes through the pipe BEFORE the
        // swap — otherwise those bytes arrive at the (restored) real
        // stdio instead of the capture buffer.
        let _ = stdout_guard.flush();
        let _ = stderr_guard.flush();

        if let Err(e) = sys::restore(&saved) {
            // Can't return this — stdio is in an unknown state and any
            // subsequent test's output would be corrupted.
            abort_with_broken_stdio("FdCapture::end", &e);
        }

        drop(stderr_guard);
        drop(stdout_guard);

        sys::close_saved(saved);

        // Restore succeeded. It is now safe to drop the Win32-stdio-
        // backing pipe writers (on Unix this is a no-op ZST).
        self.live = sys::LiveWriters::default();

        // The reader thread is still draining. Now that no one holds a
        // pipe write reference in this process, the reader will see
        // EOF shortly — UNLESS the test spawned a child process that
        // inherited the pipe and is still running. In that case, this
        // `join()` blocks until the child exits and closes its inherited
        // fds. Tests must not spawn long-lived children without waiting
        // for them (see the module docstring).
        let handle = self.reader_handle.take().expect("reader_handle missing in end()");
        handle
            .join()
            .map_err(|_| io::Error::other("skuld-capture-drain thread panicked"))?
    }
}

impl Drop for FdCapture {
    fn drop(&mut self) {
        // Happy path: `end()` already took `saved`, so this path is a
        // no-op. Drop runs only if `end` was skipped — normally
        // impossible in `run_with_observability` because the runner
        // always reaches `end` after `catch_unwind`. If it does run,
        // we're in an exceptional state (bug in the runner or an
        // unreachable panic path).
        let Some(saved) = self.saved.take() else {
            return;
        };

        // Attempt restore. On failure, abort — we cannot leave the
        // process with unknown stdio state.
        if let Err(e) = sys::restore(&saved) {
            abort_with_broken_stdio("FdCapture::drop", &e);
        }
        sys::close_saved(saved);

        // Restore succeeded. On Windows, drop the live writer handles
        // (on Unix this is a no-op).
        self.live = sys::LiveWriters::default();

        // Reader thread: drop its JoinHandle without joining. Detaching
        // avoids blocking the unwind path on a slow reader. If the
        // test spawned a child process that still holds the pipe, the
        // reader stays alive in the background until the child exits
        // — wasteful, but confined to the already-exceptional unwind
        // path.
        let _ = self.reader_handle.take();
    }
}

// Drain helper =========================================================================================

fn drain_pipe(mut reader: PipeReader) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    match reader.read_to_end(&mut buf) {
        Ok(_) => Ok(buf),
        Err(e) => match e.kind() {
            // Expected terminal states when the write end is closed.
            io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe => Ok(buf),
            _ => {
                // Windows surfaces ERROR_OPERATION_ABORTED (995) when a
                // pipe is torn down during a pending read; treat it as
                // EOF.
                #[cfg(windows)]
                if e.raw_os_error() == Some(995) {
                    return Ok(buf);
                }
                // Real error — surface it with the captured bytes
                // up to this point, so the runner can still dump what
                // it has and log the drain failure as a separate
                // diagnostic.
                Err(io::Error::new(
                    e.kind(),
                    format!("skuld drain thread read_to_end error after {} bytes: {e}", buf.len()),
                ))
            }
        },
    }
}
