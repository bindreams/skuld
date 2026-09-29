//! Tests for [`super::rendezvous`] itself.
//!
//! The status tests drive `arrive`/`status` on one thread, so state-machine
//! bugs such as early release or an ignored abort fail them without hanging.
//! `wait`'s blocking is covered by the threaded tests, which check it end to
//! end. A mutant that stops waking blocked waiters can only be seen by
//! those tests, and shows up as a hang: only a time bound could detect it,
//! which under nextest is `.config/nextest.toml`'s `slow-timeout` (a
//! backstop; no test logic depends on it).

use super::rendezvous::{rendezvous, Status};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

// Status =====

#[test]
fn nobody_is_released_until_the_last_participant_arrives() {
    let mut points = rendezvous(3);

    assert_eq!(points[0].arrive(), Status::Pending);
    assert_eq!(points[1].arrive(), Status::Pending);
    assert_eq!(points[0].status(), Status::Pending);
    assert_eq!(points[2].arrive(), Status::Released);
    for point in &points {
        assert_eq!(point.status(), Status::Released);
    }
}

#[test]
fn a_single_participant_is_released_on_arrival() {
    let mut points = rendezvous(1);

    assert_eq!(points[0].arrive(), Status::Released);
}

#[test]
fn no_participants_yield_no_points() {
    assert!(rendezvous(0).is_empty());
}

/// Dropping a point that never arrived aborts everyone else, whichever index
/// it holds and whether the others arrived before or after.
fn aborted_by_death_of(dead: usize) {
    const PARTICIPANTS: usize = 4;

    let mut points = rendezvous(PARTICIPANTS);
    let dead_point = points.remove(dead);
    let (early, late) = points.split_at_mut(1);
    assert_eq!(early[0].arrive(), Status::Pending);

    drop(dead_point);

    assert_eq!(early[0].status(), Status::Aborted);
    for point in late.iter_mut() {
        assert_eq!(point.status(), Status::Aborted);
        // Arriving after the abort must not read as released.
        assert_eq!(point.arrive(), Status::Aborted);
    }
}

#[test]
fn death_of_the_first_participant_aborts_the_rest() {
    aborted_by_death_of(0);
}

#[test]
fn death_of_a_middle_participant_aborts_the_rest() {
    aborted_by_death_of(2);
}

#[test]
fn death_of_the_last_participant_aborts_the_rest() {
    aborted_by_death_of(3);
}

/// A point that arrived is done; dropping it must not abort the others.
#[test]
fn dropping_a_point_that_arrived_does_not_abort_the_rest() {
    let mut points = rendezvous(2);
    let mut second = points.pop().unwrap();
    let mut first = points.pop().unwrap();

    assert_eq!(first.arrive(), Status::Pending);
    drop(first);

    assert_eq!(second.status(), Status::Pending);
    assert_eq!(second.arrive(), Status::Released);
}

// wait =====

fn all_released_together(n: usize) {
    let bumped = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for point in rendezvous(n) {
            s.spawn(|| {
                bumped.fetch_add(1, SeqCst);
                point.wait();
                assert_eq!(
                    bumped.load(SeqCst),
                    n,
                    "wait() returned before every participant arrived"
                );
            });
        }
    });
    assert_eq!(bumped.load(SeqCst), n);
}

#[test]
fn wait_returns_for_everyone_once_all_have_arrived() {
    all_released_together(8);
}

#[test]
fn wait_returns_immediately_for_a_single_participant() {
    all_released_together(1);
}

#[test]
fn wait_with_no_participants_has_nothing_to_wait_for() {
    all_released_together(0);
}

/// `dead` panics in its own thread before waiting; every other participant's
/// `wait` must panic rather than block.
fn survivors_panic_when_participant_dies(dead: usize) {
    const PARTICIPANTS: usize = 4;

    let mut points = rendezvous(PARTICIPANTS);
    let dead_point = points.remove(dead);
    let failed = AtomicUsize::new(0);

    std::thread::scope(|s| {
        for point in points {
            s.spawn(|| {
                if catch_unwind(AssertUnwindSafe(|| point.wait())).is_err() {
                    failed.fetch_add(1, SeqCst);
                }
            });
        }
        // Moved into a panicking thread, so the drop comes from a real unwind.
        let mutant = std::thread::spawn(move || {
            let _point = dead_point;
            panic!("participant panicking before the rendezvous");
        });
        // Its panic is the trigger, not the thing under test.
        let _ = mutant.join();
    });

    assert_eq!(
        failed.load(SeqCst),
        PARTICIPANTS - 1,
        "every survivor's wait() must panic once a participant dies"
    );
}

#[test]
fn wait_panics_for_everyone_when_the_first_participant_dies() {
    survivors_panic_when_participant_dies(0);
}

#[test]
fn wait_panics_for_everyone_when_a_middle_participant_dies() {
    survivors_panic_when_participant_dies(2);
}

#[test]
fn wait_panics_for_everyone_when_the_last_participant_dies() {
    survivors_panic_when_participant_dies(3);
}
