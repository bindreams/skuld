//! N processes x M register/unregister cycles with mixed labels and filters.
//! An `All` registration takes an exclusive witness lock and every other one a
//! shared lock, so an `All` overlapping anything shows up as a violation.

use super::shared::{running, Stats, Witness};
use super::Worker;
use crate::coord::{Coordinator, Filter};
use crate::report::{check, info};
use std::path::Path;
use std::time::Instant;

const S: &str = "churn";
const LABELS: [&str; 3] = ["A", "B", "C"];

pub fn run(n: usize, m: usize) {
    let s = format!("{S}[{n} procs x {m}]");
    let dir = tempfile::tempdir().unwrap();
    Witness::init(dir.path());
    let (root, ms) = (dir.path().to_str().unwrap(), m.to_string());
    let start = Instant::now();
    let mut workers: Vec<_> = (0..n)
        .map(|i| Worker::spawn("w-churn", &[root, &(i + 1).to_string(), &ms]))
        .collect();
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
    check(
        &s,
        stats.registrations == (n * m) as u64,
        format!("{} cycles completed", stats.registrations),
    );
    check(
        &s,
        stats.violations == 0,
        format!("no All overlapping another test: {} overlap(s)", stats.violations),
    );
    check(&s, stats.errors == 0, format!("no errors: {}", stats.errors));
    let left = running(&dir.path().join("coord"));
    check(&s, left.is_empty(), format!("running/ empty afterwards: {left:?}"));
    info(
        &s,
        format!(
            "wall {:.2}s, {} of {} registrations waited, {} retries, slowest register {:.0} ms",
            start.elapsed().as_secs_f64(),
            stats.waited,
            stats.registrations,
            stats.retries,
            stats.max_wait_us as f64 / 1000.0
        ),
    );
}

fn next(state: &mut u64) -> u64 {
    // xorshift64
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// `w-churn <root> <seed> <cycles>`
pub fn worker(args: &[String]) {
    let root = Path::new(&args[0]);
    let mut rng = args[1].parse::<u64>().unwrap().wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let coord = Coordinator::new(&root.join("coord"));
    let witness = Witness::open(root);
    let mut stats = Stats::default();
    for _ in 0..args[2].parse::<usize>().unwrap() {
        let labels: Vec<String> = LABELS
            .iter()
            .filter(|_| next(&mut rng).is_multiple_of(2))
            .map(|l| (*l).to_owned())
            .collect();
        let filter = match next(&mut rng) % 16 {
            0..=6 => Filter::None,
            7..=11 => Filter::AnyOf(vec![LABELS[next(&mut rng) as usize % 3].to_owned()]),
            12..=14 => Filter::AnyOf(LABELS.iter().take(2).map(|l| (*l).to_owned()).collect()),
            _ => Filter::All,
        };
        let t = Instant::now();
        let reg = coord.register("churn", &labels, &filter);
        stats.record(&reg, t.elapsed());
        let held = witness.enter(!matches!(filter, Filter::All), &mut stats);
        for _ in 0..3 {
            std::thread::yield_now();
        }
        if held {
            witness.leave();
        }
        drop(reg);
    }
    println!("{}", stats.line());
}
