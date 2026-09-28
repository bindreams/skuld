//! Panic-safe replacement for [`std::sync::Barrier`], for tests only.
//!
//! `std::sync::Barrier::wait()` blocks until every participant arrives, with
//! no other way out: if one participant's thread panics (or otherwise never
//! reaches the barrier), every other participant blocks in `wait()` forever.
//! The coordination tests used to accept that — one of them said so directly
//! ("the CI job-level timeout is the intended backstop") — but a CI job
//! timeout killing a hung process is exactly the "expiry as proof of
//! something" shape this crate's own retry/wait logic is built to avoid
//! elsewhere; a test relying on the same thing for its own liveness is no
//! different. [`rendezvous`] replaces `Barrier` with a mechanism that fails
//! fast instead: every participant either proceeds once all of them arrive,
//! or panics immediately once any one of them is detected missing — no
//! timeout, no polling, just a channel closing.

/// One participant's handle to an `n`-way [`rendezvous`]. Call [`Self::wait`]
/// exactly once per participant.
pub(super) struct RendezvousPoint {
    ready_tx: std::sync::mpsc::Sender<()>,
    go_rx: std::sync::mpsc::Receiver<()>,
}

impl RendezvousPoint {
    /// Block until every participant in this rendezvous has called `wait`,
    /// or panic as soon as any one of them is detected to have died first
    /// (its own `RendezvousPoint` dropped without ever calling `wait`) —
    /// never hangs waiting on a straggler *that has died* the way
    /// `Barrier::wait` would. A participant that's merely alive but stuck
    /// somewhere else (never reaching `wait` at all, without dying either)
    /// is not distinguishable from "hasn't arrived yet" and still hangs
    /// every other participant, same as `Barrier` — this only fixes the
    /// death case.
    pub(super) fn wait(self) {
        self.ready_tx
            .send(())
            .expect("rendezvous aborted: a fellow participant died");
        self.go_rx
            .recv()
            .expect("rendezvous aborted: a fellow participant died");
    }
}

/// Set up an `n`-participant rendezvous, returning one [`RendezvousPoint`]
/// per participant (hand each to exactly one thread) and a [`JoinHandle`]
/// for its coordinator thread — callers must join it themselves (inside or
/// outside whatever scope hands out the points; the coordinator has no
/// borrows tying it to one) so a coordinator-side panic is never silently
/// dropped. The coordinator releases every participant together once all
/// `n` have called `wait`, or releases no one — leaving every waiting
/// participant's `go_rx.recv()` to fail once this coordinator's own channel
/// handles drop — once every earlier participant (in the fixed index order
/// the coordinator itself waits on `ready_rx`s in) has arrived and the next
/// one's `ready_tx` is found dropped without sending (i.e. that
/// participant's thread ended, panic or not, before `wait`). Not "the
/// moment any one participant dies," which would need polling or a select
/// over all `n` receivers at once — this coordinator finds out about a
/// dead participant only once its own sequential scan reaches that
/// participant's index, after every index before it has already checked in.
///
/// Two independent one-shot channels per participant (`ready`/`go`), not one
/// `Sender` cloned `n` ways: a clone-based design can't tell "one specific
/// participant died" from "the rest just haven't arrived yet" — the channel
/// only closes once *every* clone is gone, by which point the others may
/// already be stuck waiting on a signal that was never coming. A dedicated
/// pair per participant means that participant's own `ready_rx` closing
/// (immediately, on `channel()`'s sole `Sender` being dropped) is
/// unambiguous, specific evidence of that one participant's absence.
pub(super) fn rendezvous(n: usize) -> (Vec<RendezvousPoint>, std::thread::JoinHandle<()>) {
    let (ready_txs, ready_rxs): (Vec<_>, Vec<_>) = (0..n).map(|_| std::sync::mpsc::channel::<()>()).unzip();
    let (go_txs, go_rxs): (Vec<_>, Vec<_>) = (0..n).map(|_| std::sync::mpsc::channel::<()>()).unzip();

    let coordinator = std::thread::spawn(move || {
        for rx in &ready_rxs {
            if rx.recv().is_err() {
                // A participant's ready_tx was dropped without sending:
                // that participant is gone. Don't release anyone — return
                // without sending on any go_tx, dropping them all, which
                // fails every already-waiting (or still-arriving) survivor's
                // go_rx.recv()/ready_tx.send() instead of leaving them
                // blocked.
                return;
            }
        }
        for tx in &go_txs {
            let _ = tx.send(());
        }
    });

    let points = ready_txs
        .into_iter()
        .zip(go_rxs)
        .map(|(ready_tx, go_rx)| RendezvousPoint { ready_tx, go_rx })
        .collect();
    (points, coordinator)
}
