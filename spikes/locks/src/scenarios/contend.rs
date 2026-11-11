//! N processes x M registrations, all `All`, with two independent witnesses
//! inside each registration: a lock on a separate file and a lossy counter.

use super::shared::{Stats, Witness};
use super::Worker;
use crate::coord::{Coordinator, Filter};
use crate::report::{check, info};
use std::path::Path;
use std::time::{Duration, Instant};

/// `m` registrations on one thread. Shared with the `threads` scenario.
pub fn body(coord: &Coordinator, witness: &Witness, m: usize) -> Stats {
    let mut stats = Stats::default();
    for _ in 0..m {
        let t = Instant::now();
        let reg = coord.register("t", &[], &Filter::All);
        let took = t.elapsed();
        stats.record(&reg, took);
        let held = witness.enter(false, &mut stats);
        witness.bump(&mut stats);
        if held {
            witness.leave();
        }
        drop(reg);
    }
    stats
}

/// Assertions shared by `contend` and `threads`.
pub fn verify(s: &str, root: &Path, n: usize, m: usize, st: &Stats, wall: Duration) {
    let want = (n * m) as u64;
    check(
        s,
        st.violations == 0,
        format!("no overlapping registrations: {} overlap(s)", st.violations),
    );
    check(s, st.errors == 0, format!("no errors: {}", st.errors));
    let total = Witness::count(root);
    check(
        s,
        total == want,
        format!("no lost updates: counter={total}, want {want}"),
    );
    check(
        s,
        st.waited > 0,
        format!(
            "contention was real: {} of {} registrations waited",
            st.waited, st.registrations
        ),
    );
    let left = super::shared::running(&root.join("coord"));
    check(s, left.is_empty(), format!("running/ empty afterwards: {left:?}"));
    info(
        s,
        format!(
            "wall {:.2}s, {} retries total, slowest register {:.0} ms",
            wall.as_secs_f64(),
            st.retries,
            st.max_wait_us as f64 / 1000.0
        ),
    );
}

pub fn run(n: usize, m: usize) {
    let s = format!("contend[{n} procs x {m}]");
    let dir = tempfile::tempdir().unwrap();
    Witness::init(dir.path());
    let (root, ms) = (dir.path().to_str().unwrap(), m.to_string());
    let start = Instant::now();
    let mut workers: Vec<_> = (0..n).map(|_| Worker::spawn("w-contend", &[root, &ms])).collect();
    let mut stats = Stats::default();
    for w in &mut workers {
        for line in w.drain() {
            if !stats.merge_line(&line) {
                println!("  worker: {line}");
            }
        }
        let status = w.wait();
        check(&s, status.success(), format!("worker exit: {status}"));
    }
    verify(&s, dir.path(), n, m, &stats, start.elapsed());
}

pub fn worker(args: &[String]) {
    let root = Path::new(&args[0]);
    let coord = Coordinator::new(&root.join("coord"));
    let stats = body(&coord, &Witness::open(root), args[1].parse().unwrap());
    println!("{}", stats.line());
}
