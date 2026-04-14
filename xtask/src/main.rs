//! `cargo xtask` — workspace task runner binary entry point.

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let cli = xtask::Cli::parse();
    match xtask::dispatch(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e:#}");
            ExitCode::FAILURE
        }
    }
}
