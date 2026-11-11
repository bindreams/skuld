//! N processes released at once against a coordination dir that does not
//! exist yet.

use super::shared::running;
use super::{signal_and_wait, Worker};
use crate::coord::{Coordinator, Filter};
use crate::report::{check, info};
use std::path::Path;

const S: &str = "cold";

pub fn run(n: usize, rounds: usize) {
    let parent = tempfile::tempdir().unwrap();
    let (mut bad, mut waits) = (0, 0u64);
    for round in 0..rounds {
        let dir = parent.path().join(format!("round{round}"));
        let d = dir.to_str().unwrap();
        let mut workers: Vec<_> = (0..n).map(|_| Worker::spawn("w-cold", &[d])).collect();
        for w in &mut workers {
            w.until(&["READY"]);
        }
        for w in &mut workers {
            w.release();
        }
        for (i, w) in workers.iter_mut().enumerate() {
            let lines = w.drain();
            let status = w.wait();
            match lines.iter().find_map(|l| l.strip_prefix("DONE ")) {
                Some(n) if status.success() => waits += n.parse::<u64>().unwrap(),
                _ => {
                    bad += 1;
                    println!("  bad: round {round} worker {i}: {status}, output {lines:?}");
                }
            }
        }
        let left = if dir.exists() {
            running(&dir)
        } else {
            vec!["<dir missing>".into()]
        };
        if !left.is_empty() {
            bad += 1;
            println!("  bad: round {round}: running/ not empty: {left:?}");
        }
    }
    check(
        S,
        bad == 0,
        format!("{rounds} rounds x {n} processes all registered and unregistered ({bad} bad)"),
    );
    info(S, format!("{waits} failed attempts in total"));
}

/// `w-cold <dir>`: wait for the barrier, then register and unregister.
pub fn worker(args: &[String]) {
    signal_and_wait("READY");
    let coord = Coordinator::new(Path::new(&args[0]));
    let reg = coord.register("cold", &[], &Filter::All);
    let waits = reg.waits;
    drop(reg);
    println!("DONE {waits}");
}
