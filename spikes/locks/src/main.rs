//! File-lock spike for skuld#121. `locks-spike all` runs every scenario and
//! exits non-zero on any FAIL. `w-*` subcommands are worker processes the
//! scenarios spawn.

mod coord;
mod report;
mod scenarios;

use scenarios::{churn, cold, contend, crash, filter, kill, race, threads};

fn num(rest: &[String], i: usize, default: usize) -> usize {
    rest.get(i).map_or(default, |s| s.parse().unwrap())
}

fn finish() -> ! {
    std::process::exit(i32::from(report::fails() != 0))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("all") => std::process::exit(scenarios::run_all()),
        // `contend <procs> <registrations each>`
        Some("contend") => {
            contend::run(num(rest, 0, 8), num(rest, 1, 200));
            finish()
        }
        Some("threads") => {
            threads::run(num(rest, 0, 8), num(rest, 1, 200));
            finish()
        }
        Some("filter") => {
            filter::run();
            finish()
        }
        // Not part of `all`: expected FAIL unless LOCKS_SPIKE_FIX is set.
        Some("create-race") => {
            race::run();
            finish()
        }
        Some("crash") => {
            crash::run();
            finish()
        }
        // `kill-anywhere <gated repetitions> <free kills>`
        Some("kill-anywhere") => {
            kill::run(num(rest, 0, 10), num(rest, 1, 100));
            finish()
        }
        // `cold <rounds>`
        Some("cold") => {
            cold::run(8, num(rest, 0, 100));
            finish()
        }
        // `churn <procs> <cycles>`
        Some("churn") => {
            churn::run(num(rest, 0, 8), num(rest, 1, 300));
            finish()
        }
        Some("w-contend") => contend::worker(rest),
        Some("w-hold") => filter::hold_worker(rest),
        Some("w-reg") => filter::reg_worker(rest),
        Some("w-race") => race::worker(rest),
        Some("w-loop") => kill::worker(rest),
        Some("w-cold") => cold::worker(rest),
        Some("w-churn") => churn::worker(rest),
        other => panic!("unknown subcommand {other:?}"),
    }
}
