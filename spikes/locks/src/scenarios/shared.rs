//! Witnesses and stats shared by the scenarios.

use crate::coord::{self, Filter, Registration};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Default)]
pub struct Stats {
    pub registrations: u64,
    /// Registrations that had to wait at least once.
    pub waited: u64,
    /// Total failed attempts.
    pub retries: u64,
    pub violations: u64,
    pub errors: u64,
    pub max_wait_us: u64,
}

impl Stats {
    pub fn record(&mut self, reg: &Registration, took: Duration) {
        self.registrations += 1;
        self.waited += u64::from(reg.waits > 0);
        self.retries += u64::from(reg.waits);
        self.max_wait_us = self.max_wait_us.max(took.as_micros() as u64);
    }

    pub fn line(&self) -> String {
        format!(
            "RESULT {} {} {} {} {} {}",
            self.registrations, self.waited, self.retries, self.violations, self.errors, self.max_wait_us
        )
    }

    pub fn merge_line(&mut self, line: &str) -> bool {
        let Some(rest) = line.strip_prefix("RESULT ") else {
            return false;
        };
        let v: Vec<u64> = rest.split(' ').map(|x| x.parse().unwrap()).collect();
        self.registrations += v[0];
        self.waited += v[1];
        self.retries += v[2];
        self.violations += v[3];
        self.errors += v[4];
        self.max_wait_us = self.max_wait_us.max(v[5]);
        true
    }

    pub fn merge(&mut self, o: &Stats) {
        self.merge_line(&o.line());
    }
}

/// An independent check that registrations exclude each other: a lock on a
/// file the coordinator never touches, plus a plain-file counter.
pub struct Witness {
    lock: File,
    counter: PathBuf,
}

impl Witness {
    /// Create the files under `root`; call once, before any `open`.
    pub fn init(root: &Path) {
        std::fs::write(root.join("witness.lock"), b"").unwrap();
        std::fs::write(root.join("counter"), b"0").unwrap();
    }

    /// One per thread or process: locks are per handle.
    pub fn open(root: &Path) -> Self {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("witness.lock"))
            .unwrap();
        Witness {
            lock,
            counter: root.join("counter"),
        }
    }

    /// Take the witness lock (shared or exclusive) without waiting. Failure
    /// means two registrations overlapped.
    pub fn enter(&self, shared: bool, stats: &mut Stats) -> bool {
        let got = if shared {
            try_lock_shared(&self.lock)
        } else {
            coord::try_lock(&self.lock)
        };
        match got {
            Ok(true) => true,
            Ok(false) => {
                stats.violations += 1;
                false
            }
            Err(e) => {
                println!("error: witness lock: {e}");
                stats.errors += 1;
                false
            }
        }
    }

    pub fn leave(&self) {
        coord::unlock(&self.lock).unwrap();
    }

    /// Unprotected read-modify-write; loses updates if registrations overlap.
    pub fn bump(&self, stats: &mut Stats) {
        let step = || -> std::io::Result<()> {
            let n: u64 = std::fs::read_to_string(&self.counter)?.trim().parse().unwrap_or(0);
            std::fs::write(&self.counter, (n + 1).to_string())
        };
        if let Err(e) = step() {
            println!("error: counter: {e}");
            stats.errors += 1;
        }
    }

    pub fn count(root: &Path) -> u64 {
        std::fs::read_to_string(root.join("counter"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
}

fn try_lock_shared(f: &File) -> std::io::Result<bool> {
    #[cfg(unix)]
    match rustix::fs::flock(f, rustix::fs::FlockOperation::NonBlockingLockShared) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(false),
        Err(e) => Err(e.into()),
    }
    #[cfg(windows)]
    match f.try_lock_shared() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// Sorted names in `dir/running`.
pub fn running(dir: &Path) -> Vec<String> {
    let mut v: Vec<_> = std::fs::read_dir(dir.join("running"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// `none`, `all` or `any:A,B`.
pub fn parse_filter(s: &str) -> Filter {
    match s {
        "none" => Filter::None,
        "all" => Filter::All,
        _ => Filter::AnyOf(s.strip_prefix("any:").unwrap().split(',').map(str::to_owned).collect()),
    }
}

/// `-` or `A,B`.
pub fn parse_labels(s: &str) -> Vec<String> {
    if s == "-" {
        vec![]
    } else {
        s.split(',').map(str::to_owned).collect()
    }
}
