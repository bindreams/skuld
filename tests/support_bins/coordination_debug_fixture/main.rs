//! Subject of subprocess invocation in `tests/coordination_debug_cli.rs`.
//! `hold_a` is globally serial and, once registered, prints a line and
//! blocks on stdin until told to release — this lets the driver guarantee
//! (not merely hope) that `hold_b`'s first coordinate() attempt happens
//! while `hold_a` is still registered, without any sleep/timing bet.
//! `hold_b` is globally serial with a trivial body: the interesting part
//! (the coordination wait) happens inside coordinate(), before its closure
//! ever runs.

use std::io::{BufRead, Write};

fn main() {
    let mut runner = skuld::TestRunner::new();
    runner.add_serial("hold_a", &[], false, || {
        println!("REGISTERED");
        std::io::stdout().flush().expect("flush stdout");
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .expect("read RELEASE from stdin");
    });
    runner.add_serial("hold_b", &[], false, || {});
    runner.run();
}
