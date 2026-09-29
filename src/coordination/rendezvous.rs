//! Panic-safe replacement for [`std::sync::Barrier`], for tests only.
//!
//! `Barrier::wait()` blocks forever if a participant dies before arriving,
//! leaving a CI job timeout as the only backstop. With [`rendezvous`] every
//! participant proceeds once all have arrived, or panics as soon as any
//! participant's [`RendezvousPoint`] is dropped without having arrived. No
//! timeout, no polling, no extra thread.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

/// Where a rendezvous stands, as seen by one participant.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Status {
    /// Not every participant has arrived, and none has died.
    Pending,
    /// Every participant has arrived.
    Released,
    /// A participant was dropped without arriving; the rest can never be released.
    Aborted,
}

struct State {
    participants: usize,
    arrived: usize,
    aborted: bool,
}

impl State {
    fn status(&self) -> Status {
        if self.arrived == self.participants {
            Status::Released
        } else if self.aborted {
            Status::Aborted
        } else {
            Status::Pending
        }
    }
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    /// Poisoning is irrelevant: nothing under this lock can panic (the
    /// `arrive` contract assert runs after the guard is dropped).
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One participant's handle to an `n`-way [`rendezvous`]. Call [`Self::wait`]
/// exactly once per participant.
pub(super) struct RendezvousPoint {
    shared: Arc<Shared>,
    arrived: bool,
}

impl RendezvousPoint {
    /// Block until every participant has arrived; panic if a
    /// participant's point was dropped without calling it. A participant that
    /// is alive but never arrives still blocks everyone, as with `Barrier`;
    /// under `cargo test` nothing bounds that, and under nextest
    /// `.config/nextest.toml`'s `slow-timeout` fails the test.
    pub(super) fn wait(mut self) {
        self.arrive();
        let shared = Arc::clone(&self.shared);
        let state = shared
            .changed
            .wait_while(shared.lock(), |state| state.status() == Status::Pending)
            .unwrap_or_else(PoisonError::into_inner);
        let status = state.status();
        // Release the lock before panicking so it is never poisoned.
        drop(state);
        assert!(
            status == Status::Released,
            "rendezvous aborted: a participant's point was dropped without arriving \
             (its thread ended or panicked first)"
        );
    }

    /// The non-blocking half of [`Self::wait`]: record this participant's
    /// arrival and return the resulting status.
    pub(super) fn arrive(&mut self) -> Status {
        debug_assert!(!self.arrived, "RendezvousPoint::arrive called twice");
        self.arrived = true;
        let mut state = self.shared.lock();
        state.arrived += 1;
        let (arrived, participants) = (state.arrived, state.participants);
        let status = state.status();
        drop(state);
        debug_assert!(arrived <= participants);
        self.shared.changed.notify_all();
        status
    }

    /// The current status, without blocking or arriving.
    pub(super) fn status(&self) -> Status {
        self.shared.lock().status()
    }
}

impl Drop for RendezvousPoint {
    fn drop(&mut self) {
        if !self.arrived {
            self.shared.lock().aborted = true;
            self.shared.changed.notify_all();
        }
    }
}

/// Set up an `n`-participant rendezvous: one [`RendezvousPoint`] per
/// participant, each to be handed to exactly one thread.
pub(super) fn rendezvous(n: usize) -> Vec<RendezvousPoint> {
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            participants: n,
            arrived: 0,
            aborted: false,
        }),
        changed: Condvar::new(),
    });
    (0..n)
        .map(|_| RendezvousPoint {
            shared: Arc::clone(&shared),
            arrived: false,
        })
        .collect()
}
