//! Deterministic replay of a flaw in the design as written: `<id>.lock` is
//! created and then locked before `coord.lock` is taken, and in between a
//! scanner may see it as a leftover (unlocked, no `.json`) and delete it.
//!
//! The owner then locks an unlinked file, publishes its `.json`, and the next
//! scanner finds that `.json` with no `.lock` and declares the owner dead.
//! Expected FAIL as written; PASS with `LOCKS_SPIKE_FIX=1`.

use super::shared::running;
use super::{signal_and_wait, Worker};
use crate::coord::{Coordinator, Filter};
use crate::report::check;
use std::path::Path;

const S: &str = "create-race";

pub fn run() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    let mut holder = Worker::spawn("w-race", &[d]);
    holder.until(&["CREATED"]);
    // As written the holder is paused unlocked and without coord.lock, so this
    // scan deletes its .lock. With LOCKS_SPIKE_FIX the holder is paused holding
    // coord.lock, so nothing can be staged and the step is skipped.
    if std::env::var_os("LOCKS_SPIKE_FIX").is_none() {
        drop(Coordinator::new(dir.path()).register("driver", &[], &Filter::All));
    }
    holder.release();
    holder.until(&["HELD"]);
    // The holder now holds an All registration.
    let mut other = Worker::spawn("w-reg", &[d, "all", "-"]);
    let first = other.until(&["WAITING", "REGISTERED"]);
    check(
        S,
        first == "WAITING",
        format!("second All registration while the first is held: {first}"),
    );
    holder.release();
    holder.wait();
    if first == "WAITING" {
        other.until(&["REGISTERED"]);
    }
    other.wait();
    let left = running(dir.path());
    check(S, left.is_empty(), format!("running/ empty afterwards: {left:?}"));
}

/// `w-race <dir>`: register `All`, pausing right after the `.lock` is
/// created; print HELD once registered and wait.
pub fn worker(args: &[String]) {
    let coord = Coordinator::new(Path::new(&args[0])).with_hook(|p| {
        if p == "created" {
            signal_and_wait("CREATED");
        }
    });
    let reg = coord.register("race", &[], &Filter::All);
    signal_and_wait("HELD");
    drop(reg);
}
