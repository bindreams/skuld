//! Thread-scoped test seams for the coordination retry loops.
//!
//! Thread-local, not process-wide: `retry_busy` also runs in every
//! `TestRegistration`'s drop, and libtest runs those concurrently, so a global
//! signal could wake a test before its own call retried. Each seam is
//! installed by a single-purpose worker thread and reset by an RAII guard.

use std::cell::RefCell;
use std::sync::mpsc::{sync_channel, Receiver, Sender, SyncSender};

// Retry rendezvous =====

/// The worker's half of a [`retry_rendezvous`], installed with
/// [`set_test_retry_hook`].
pub(crate) struct RetryWorker {
    signal: Sender<()>,
    ack: Receiver<()>,
}

/// The test's half of a [`retry_rendezvous`].
pub(crate) struct RetryTest {
    signal: Receiver<()>,
    ack: SyncSender<()>,
}

/// A paired signal and ack. Each retry the worker makes sends one signal and
/// then blocks until the test sends one ack, so whatever the test does between
/// [`RetryTest::wait_for_retry`] and [`RetryTest::release`] happens-before the
/// worker's next attempt. Nothing relies on the backoff sleep.
pub(crate) fn retry_rendezvous() -> (RetryWorker, RetryTest) {
    let (signal_tx, signal_rx) = std::sync::mpsc::channel();
    let (ack_tx, ack_rx) = sync_channel(1);
    (
        RetryWorker {
            signal: signal_tx,
            ack: ack_rx,
        },
        RetryTest {
            signal: signal_rx,
            ack: ack_tx,
        },
    )
}

impl RetryTest {
    /// Block until the worker is inside a retry. Panics if the worker exited
    /// without retrying (its end was dropped), which turns a broken retry path
    /// into a test failure instead of a hang.
    pub(crate) fn wait_for_retry(&self) {
        self.signal
            .recv()
            .expect("the worker exited without hitting a retryable busy error — test setup is broken");
    }

    /// Let the worker proceed past the retry it last signalled.
    pub(crate) fn release(&self) {
        // Never blocks: the worker consumed the previous ack before it sent
        // the signal being released. A full slot or an exited worker leaves
        // nothing to release.
        let _ = self.ack.try_send(());
    }

    /// Let the worker retry `n` times while whatever makes it busy stays in
    /// place: for each, wait for its signal and release it.
    pub(crate) fn pass_retries(&self, n: usize) {
        for _ in 0..n {
            self.wait_for_retry();
            self.release();
        }
    }

    /// Whether the worker has sent a signal not yet consumed by
    /// [`Self::wait_for_retry`], without blocking.
    #[cfg(test)]
    pub(crate) fn signal_pending(&self) -> bool {
        self.signal.try_recv().is_ok()
    }

    /// Whether [`Self::release`] would not block, i.e. the worker consumed the
    /// previous ack.
    #[cfg(test)]
    pub(crate) fn ack_slot_free(&self) -> bool {
        self.ack.try_send(()).is_ok()
    }
}

thread_local! {
    static RETRY_HOOK: RefCell<Option<RetryWorker>> = const { RefCell::new(None) };
}

/// Resets the calling thread's retry hook on drop.
#[must_use = "the hook is removed when this guard drops"]
pub(crate) struct RetryHookGuard(());

impl Drop for RetryHookGuard {
    fn drop(&mut self) {
        RETRY_HOOK.with(|c| *c.borrow_mut() = None);
    }
}

/// Activate `worker` as the retry hook for the calling thread only.
pub(crate) fn set_test_retry_hook(worker: RetryWorker) -> RetryHookGuard {
    RETRY_HOOK.with(|c| {
        let mut slot = c.borrow_mut();
        debug_assert!(slot.is_none(), "set_test_retry_hook: already set on this thread");
        *slot = Some(worker);
    });
    RetryHookGuard(())
}

/// Called by the retry loops on every retry: send one signal, then block until
/// the test acks it. A no-op unless this thread installed a hook. If the test
/// side is gone, does not block.
pub(super) fn signal_retry() {
    RETRY_HOOK.with(|c| {
        if let Some(w) = c.borrow().as_ref() {
            if w.signal.send(()).is_ok() {
                let _ = w.ack.recv();
            }
        }
    });
}

// After-write seam =====

/// A write whose post-write moved-DB check a test wants to intercept.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AfterWriteSite {
    /// `coordinate`, right after its COMMIT succeeded.
    Coordinate,
    /// `TestRegistration::drop`, right after its DELETE succeeded.
    Drop,
}

type AfterWriteHook = (AfterWriteSite, Box<dyn FnOnce()>);

thread_local! {
    static AFTER_WRITE_HOOK: RefCell<Option<AfterWriteHook>> = const { RefCell::new(None) };
}

/// Clears the calling thread's after-write hook on drop.
#[must_use = "the hook is removed when this guard drops"]
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) struct AfterWriteHookGuard(());

impl Drop for AfterWriteHookGuard {
    fn drop(&mut self) {
        AFTER_WRITE_HOOK.with(|c| *c.borrow_mut() = None);
    }
}

/// Run `f` on the calling thread once, at the next `site` write.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn set_test_after_write_hook(site: AfterWriteSite, f: impl FnOnce() + 'static) -> AfterWriteHookGuard {
    AFTER_WRITE_HOOK.with(|c| {
        let mut slot = c.borrow_mut();
        debug_assert!(slot.is_none(), "set_test_after_write_hook: already set on this thread");
        *slot = Some((site, Box::new(f)));
    });
    AfterWriteHookGuard(())
}

pub(super) fn run_after_write(site: AfterWriteSite) {
    let hook = AFTER_WRITE_HOOK.with(|c| {
        let mut slot = c.borrow_mut();
        match slot.take() {
            Some((s, f)) if s == site => Some(f),
            other => {
                *slot = other;
                None
            }
        }
    });
    if let Some(f) = hook {
        f();
    }
}
