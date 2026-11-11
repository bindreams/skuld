//! The `contend` check inside one process, as skuld runs trials on threads.

use super::contend::{body, verify};
use super::shared::{Stats, Witness};
use crate::coord::Coordinator;
use std::time::Instant;

pub fn run(n: usize, m: usize) {
    let s = format!("threads[{n} threads x {m}]");
    let dir = tempfile::tempdir().unwrap();
    Witness::init(dir.path());
    let coord = Coordinator::new(&dir.path().join("coord"));
    let start = Instant::now();
    let mut stats = Stats::default();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..n)
            .map(|_| {
                scope.spawn(|| {
                    let witness = Witness::open(dir.path());
                    body(&coord, &witness, m)
                })
            })
            .collect();
        for h in handles {
            stats.merge(&h.join().unwrap());
        }
    });
    verify(&s, dir.path(), n, m, &stats, start.elapsed());
}
