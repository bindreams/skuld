//! Tests for [`super::rendezvous`] itself.

use super::rendezvous::rendezvous;

/// TDD red/green for [`rendezvous`] itself: a "mutant" participant panics
/// before ever calling `wait`.
///
/// Structured to survive mutations a naive version couldn't: the dying
/// participant is *last*, not first, and every survivor runs `wait()` in
/// its own spawned thread — reporting its own `catch_unwind` result back,
/// rather than being called synchronously on the main test thread after
/// the coordinator is already known to be done. Both choices exist for the
/// same reason: they make this test's own verdict independent of the
/// coordinator's internal iteration order or control flow. Every survivor
/// is a real, already-running thread blocked in `wait()` — not code that
/// hasn't executed yet — so its ready signal is genuinely available the
/// moment the coordinator looks for it, in whatever order that happens: a
/// coordinator that iterates in reverse, or that keeps going past a
/// detected failure (`continue`) instead of stopping immediately
/// (`return`), still can't produce a false "everyone survived," because
/// there's no still-unactivated survivor left for it to accidentally
/// release. An earlier version of this test put the mutant first and called
/// survivors synchronously after joining the coordinator; targeted
/// mutations of the coordinator (reversed iteration order; `continue`
/// instead of `return`) either hung that version or made it pass when it
/// shouldn't have.
///
/// One mutation this can't catch, by construction: swapping the coordinator
/// for one built on `std::sync::Barrier` itself. `Barrier::wait()` has no
/// failure path at all — it simply never returns once a participant is
/// missing, for any participant — so there is no channel to close, no
/// signal to observe, nothing here to detect. Catching that specific
/// regression would need a timeout, which this crate's own rules forbid
/// using as a correctness check; it's instead bounded only by the CI job's
/// own runner timeout, the same backstop every `Barrier`-based test in this
/// file relied on before `rendezvous` existed.
///
/// The mutant's `RendezvousPoint` is moved bodily into its own spawned
/// thread (`let _p = mutant_point;`), not just held in this function's own
/// stack frame: that's what makes its drop — and the coordinator's
/// detection of it — a genuine consequence of a panicking thread's unwind,
/// the real mechanism under test, rather than an ordinary drop this test
/// would trigger on its own regardless of whether panics correctly unwind
/// through spawned threads at all.
#[test]
fn rendezvous_fails_fast_instead_of_hanging_when_a_participant_panics_before_it() {
    const THREADS: usize = 4;

    let (points, coordinator) = rendezvous(THREADS);
    let mut points = points.into_iter();

    // Survivors first, each a real thread already blocked in wait() before
    // the mutant is even spawned — see this function's own doc for why
    // that's what makes the rest of this test independent of the
    // coordinator's internal iteration order.
    let survivors: Vec<std::thread::JoinHandle<bool>> = points
        .by_ref()
        .take(THREADS - 1)
        .map(|point| {
            std::thread::spawn(move || std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| point.wait())).is_err())
        })
        .collect();

    // The mutant is last, not first.
    let mutant_point = points
        .next()
        .expect("test bug: rendezvous(THREADS) must yield THREADS points");
    assert!(
        points.next().is_none(),
        "test bug: rendezvous(THREADS) must yield exactly THREADS points, no more"
    );
    let mutant = std::thread::spawn(move || {
        let _p = mutant_point;
        panic!("mutant: panicking before the rendezvous");
    });
    // The mutant's own panic isn't itself the thing under test — only that
    // its point dropped as a result of it. Swallow it here.
    let _ = mutant.join();

    // The coordinator must have detected the mutant's absence and returned
    // cleanly (not itself panicked) without releasing anyone.
    coordinator
        .join()
        .expect("rendezvous coordinator thread must not itself panic");

    let failures = survivors
        .into_iter()
        .map(|h| {
            h.join()
                .expect("survivor thread must not panic itself — only point.wait() inside its catch_unwind may")
        })
        .filter(|&wait_failed| wait_failed)
        .count();
    assert_eq!(
        failures,
        THREADS - 1,
        "every survivor's wait() must fail once a fellow participant dies before the \
         rendezvous, not just some of them"
    );
}
