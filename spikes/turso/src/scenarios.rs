//! One module per property skuld's coordination relies on.

pub mod churn;
pub mod cold;
pub mod contend;
pub mod crash;
#[cfg(feature = "interop")]
pub mod interop;
pub mod smoke;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

pub fn run_all() -> i32 {
    println!(
        "target: {}-{} ({}-bit)",
        std::env::consts::ARCH,
        std::env::consts::OS,
        usize::BITS
    );
    smoke::run();
    for begin in ["BEGIN EXCLUSIVE", "BEGIN IMMEDIATE"] {
        contend::run(begin, 8, 200);
    }
    crash::run();
    for variant in ["fresh", "precreated", "init-locked"] {
        cold::run(8, variant, 20);
    }
    churn::run(8, 50);
    #[cfg(feature = "interop")]
    interop::run();
    let fails = crate::report::fails();
    println!("{fails} FAIL(s)");
    i32::from(fails != 0)
}

/// Spawn this binary as worker `sub` with piped stdin/stdout.
pub fn spawn(sub: &str, args: &[&str]) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .arg(sub)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap()
}

pub type Lines = Vec<String>;

/// Block until the child prints `line` (or panic if its stdout closes first).
/// Returns the reader and the lines printed before it.
pub fn await_line(child: &mut Child, line: &str) -> (BufReader<std::process::ChildStdout>, Lines) {
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut before = Vec::new();
    let mut buf = String::new();
    loop {
        buf.clear();
        assert!(
            out.read_line(&mut buf).unwrap() > 0,
            "worker exited before printing {line:?}"
        );
        if buf.trim_end() == line {
            return (out, before);
        }
        before.push(buf.trim_end().to_owned());
    }
}

/// Worker side: say `line`, then block until the driver writes a line.
pub fn signal_and_wait(line: &str) {
    println!("{line}");
    std::io::stdout().flush().unwrap();
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf).unwrap();
}

pub fn release(child: &mut Child) {
    writeln!(child.stdin.as_mut().unwrap()).unwrap();
}
