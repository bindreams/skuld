//! One module per property skuld's coordination relies on.

pub mod churn;
pub mod cold;
pub mod contend;
pub mod crash;
pub mod filter;
pub mod kill;
pub mod race;
pub mod shared;
pub mod threads;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};

pub fn run_all() -> i32 {
    println!(
        "target: {}-{} ({}-bit)",
        std::env::consts::ARCH,
        std::env::consts::OS,
        usize::BITS
    );
    contend::run(8, 200);
    threads::run(8, 200);
    filter::run();
    crash::run();
    kill::run(10, 100);
    cold::run(8, 100);
    churn::run(8, 300);
    let fails = crate::report::fails();
    println!("{fails} FAIL(s)");
    i32::from(fails != 0)
}

/// A spawned copy of this binary with piped stdin/stdout.
pub struct Worker {
    child: Child,
    out: BufReader<ChildStdout>,
}

impl Worker {
    pub fn spawn(sub: &str, args: &[&str]) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg(sub)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        Worker { child, out }
    }

    /// Next stdout line, `None` at EOF.
    pub fn line(&mut self) -> Option<String> {
        let mut buf = String::new();
        (self.out.read_line(&mut buf).unwrap() > 0).then(|| buf.trim_end().to_owned())
    }

    /// Read until one of `wanted`; returns it. Panics if stdout closes first.
    pub fn until(&mut self, wanted: &[&str]) -> String {
        loop {
            let line = self
                .line()
                .unwrap_or_else(|| panic!("worker exited before printing one of {wanted:?}"));
            if wanted.contains(&line.as_str()) {
                return line;
            }
            println!("  worker: {line}");
        }
    }

    /// Everything the worker prints until EOF.
    pub fn drain(&mut self) -> Vec<String> {
        std::iter::from_fn(|| self.line()).collect()
    }

    /// Unblock a worker waiting in [`signal_and_wait`].
    pub fn release(&mut self) {
        writeln!(self.child.stdin.as_mut().unwrap()).unwrap();
    }

    pub fn wait(&mut self) -> ExitStatus {
        self.child.wait().unwrap()
    }

    /// Kills only this worker's own child.
    pub fn kill(&mut self) {
        self.child.kill().unwrap();
    }
}

/// Worker side: say `line`, then block until the driver writes a line.
pub fn signal_and_wait(line: &str) {
    println!("{line}");
    std::io::stdout().flush().unwrap();
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf).unwrap();
}
