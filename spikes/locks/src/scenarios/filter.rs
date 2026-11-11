//! Filter semantics, event-based: a worker that must block prints WAITING on
//! its first blocked attempt, and only then does the driver release the holder.

use super::shared::{parse_filter, parse_labels, running};
use super::{signal_and_wait, Worker};
use crate::coord::Coordinator;
use crate::report::check;
use std::path::Path;

const S: &str = "filter";

/// (filter, labels) as `w-hold` / `w-reg` take them.
type Spec = (&'static str, &'static str);

/// (holder, worker, worker must block)
const CASES: &[(Spec, Spec, bool)] = &[
    (("any:A", "-"), ("none", "B"), false),
    (("any:A", "-"), ("none", "A"), true),
    (("all", "-"), ("none", "-"), true),
    (("none", "-"), ("all", "-"), true),
    (("none", "A"), ("any:A", "-"), true),
    (("none", "B"), ("any:A", "-"), false),
    (("none", "-"), ("none", "-"), false),
    (("any:A", "-"), ("any:A", "-"), false),
];

pub fn run() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_str().unwrap();
    for &((hf, hl), (wf, wl), blocks) in CASES {
        let what = format!("holder {hf}/{hl}, worker {wf}/{wl}");
        let mut holder = Worker::spawn("w-hold", &[d, hf, hl, "release"]);
        holder.until(&["HELD"]);
        let mut worker = Worker::spawn("w-reg", &[d, wf, wl]);
        let first = worker.until(&["WAITING", "REGISTERED"]);
        let blocked = first == "WAITING";
        let verdict = if blocks {
            "blocked until the holder released"
        } else {
            "registered without waiting"
        };
        check(S, blocked == blocks, format!("{what}: {verdict}"));
        holder.release();
        check(S, holder.wait().success(), format!("{what}: holder exit"));
        if blocked {
            worker.until(&["REGISTERED"]);
        }
        check(S, worker.wait().success(), format!("{what}: worker exit"));
    }
    let left = running(dir.path());
    check(S, left.is_empty(), format!("running/ empty afterwards: {left:?}"));
}

/// `w-hold <dir> <filter> <labels> <release|abort>`: register, print HELD,
/// wait for the driver, then unregister or abort.
pub fn hold_worker(args: &[String]) {
    let coord = Coordinator::new(Path::new(&args[0]));
    let reg = coord.register("hold", &parse_labels(&args[2]), &parse_filter(&args[1]));
    signal_and_wait("HELD");
    if args[3] == "abort" {
        std::process::abort();
    }
    drop(reg);
}

/// `w-reg <dir> <filter> <labels>`: register, print REGISTERED, unregister.
/// Prints WAITING from the hook on the first blocked attempt.
pub fn reg_worker(args: &[String]) {
    let coord = Coordinator::new(Path::new(&args[0])).with_hook(|p| {
        if p == "waiting" {
            println!("WAITING");
        }
    });
    let reg = coord.register("reg", &parse_labels(&args[2]), &parse_filter(&args[1]));
    println!("REGISTERED");
    drop(reg);
}
