//! Tests of the retry rendezvous itself.

use super::test_hooks::{retry_rendezvous, set_test_retry_hook, signal_retry};

/// One signal consumes exactly one ack. Driven on a single thread with the ack
/// pre-buffered, so the outcome is deterministic: a hook that sends without
/// waiting for its ack leaves the slot full.
#[test]
fn signal_retry_sends_one_signal_and_consumes_one_ack() {
    let (worker, test) = retry_rendezvous();
    let _hook = set_test_retry_hook(worker);

    test.release();
    signal_retry();

    assert!(test.signal_pending(), "signal_retry must send a signal");
    assert!(
        test.ack_slot_free(),
        "signal_retry must consume the ack before returning; a hook that does not wait leaves it buffered"
    );
}

/// Without an installed hook the seam is inert.
#[test]
fn signal_retry_is_a_no_op_without_a_hook() {
    signal_retry();
}

/// A dropped test side must not hang the worker.
#[test]
fn signal_retry_does_not_block_once_the_test_side_is_gone() {
    let (worker, test) = retry_rendezvous();
    let _hook = set_test_retry_hook(worker);
    drop(test);
    signal_retry();
}

/// The guard resets the slot, so a thread can install a hook again.
#[test]
fn dropping_the_guard_allows_reinstalling() {
    let (worker, _test) = retry_rendezvous();
    drop(set_test_retry_hook(worker));
    let (worker, _test) = retry_rendezvous();
    let _again = set_test_retry_hook(worker);
}
