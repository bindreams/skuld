//! Kill a registering worker at every point it can reach; afterwards a fresh
//! `All` registration must succeed and leave `running/` empty.
//!
//! A gated worker stops after printing each point until the driver answers,
//! so the kill lands exactly there. A free worker keeps running, so the kill
//! lands somewhere after the k-th line.

use super::shared::running;
use super::{signal_and_wait, Worker};
use crate::coord::{Coordinator, Filter};
use crate::report::{check, info};
use std::collections::BTreeMap;
use std::path::Path;

const S: &str = "kill-anywhere";
/// Points one register/unregister cycle prints.
const POINTS: usize = 11;

#[derive(Default)]
struct Tally {
    kills: u64,
    bad: u64,
    waits: u64,
    max_waits: u32,
    by_point: BTreeMap<String, u64>,
}

fn kill_at(dir: &Path, k: usize, gated: bool, t: &mut Tally) {
    let mut w = Worker::spawn("w-loop", &[dir.to_str().unwrap(), if gated { "gated" } else { "free" }]);
    let mut point = String::new();
    for i in 1..=k {
        point = w.line().expect("worker exited before the kill point");
        if gated && i < k {
            w.release();
        }
    }
    w.kill();
    w.wait();
    t.kills += 1;
    *t.by_point.entry(point.clone()).or_default() += 1;
    let coord = Coordinator::new(dir);
    let reg = coord.register("driver", &[], &Filter::All);
    t.waits += u64::from(reg.waits);
    t.max_waits = t.max_waits.max(reg.waits);
    let during = running(dir);
    drop(reg);
    let after = running(dir);
    if during.len() != 2 || !after.is_empty() {
        t.bad += 1;
        println!("  bad: killed after {point:?} (k={k}, gated={gated}): during {during:?}, after {after:?}");
    }
}

pub fn run(gated_reps: usize, free_kills: usize) {
    let dir = tempfile::tempdir().unwrap();
    let (mut gated, mut free) = (Tally::default(), Tally::default());
    for _ in 0..gated_reps {
        for k in 1..=POINTS {
            kill_at(dir.path(), k, true, &mut gated);
        }
    }
    for k in 1..=free_kills {
        kill_at(dir.path(), k, false, &mut free);
    }
    for (name, t) in [("gated", &gated), ("free", &free)] {
        check(
            S,
            t.bad == 0,
            format!(
                "{name}: {} kills, every later All registration succeeded and left running/ empty ({} bad)",
                t.kills, t.bad
            ),
        );
        info(
            S,
            format!(
                "{name}: driver waited {} times in total (max {} on one registration)",
                t.waits, t.max_waits
            ),
        );
        info(S, format!("{name}: kills per point {:?}", t.by_point));
    }
}

/// `w-loop <dir> <gated|free>`: register and unregister forever, printing
/// `P <point>` at each step.
pub fn worker(args: &[String]) {
    let gated = args[1] == "gated";
    let coord = Coordinator::new(Path::new(&args[0])).with_hook(move |p| {
        if gated {
            signal_and_wait(&format!("P {p}"));
        } else {
            println!("P {p}");
        }
    });
    loop {
        drop(coord.register("loop", &[], &Filter::All));
    }
}
