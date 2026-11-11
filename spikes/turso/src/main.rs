//! Turso spike for skuld#121. `turso-spike all` runs every scenario and exits
//! non-zero on any FAIL. `w-*` subcommands are worker processes the scenarios
//! spawn.

mod db;
mod report;
mod scenarios;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = &args[1..];
    match args.first().map(String::as_str) {
        Some("all") => std::process::exit(scenarios::run_all()),
        // `cold <variant> <rounds>`: one cold-start variant on its own.
        Some("cold") => {
            scenarios::cold::run(8, &rest[0], rest[1].parse().unwrap());
            std::process::exit(i32::from(report::fails() != 0));
        }
        Some("w-contend") => scenarios::contend::worker(rest),
        Some("w-crash") => scenarios::crash::worker(rest),
        Some("w-cold") => scenarios::cold::worker(rest),
        Some("w-churn") => scenarios::churn::worker(rest),
        // `churn <procs> <cycles>`
        Some("churn") => {
            scenarios::churn::run(rest[0].parse().unwrap(), rest[1].parse().unwrap());
            std::process::exit(i32::from(report::fails() != 0));
        }
        #[cfg(feature = "interop")]
        Some(w) if w.starts_with("w-interop-") => scenarios::interop::worker(w, rest),
        other => panic!("unknown subcommand {other:?}"),
    }
}
