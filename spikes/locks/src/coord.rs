//! Test coordination on OS file locks, no database.
//!
//! Layout under the coordination dir: `coord.lock` (the global mutex, held
//! only while scanning or publishing), and per registered test
//! `running/<id>.lock` (held for the registration's lifetime, so the kernel
//! releases it if the owner dies) plus `running/<id>.json` (name, labels,
//! filter, published by rename).

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Stand-in for skuld's `LabelFilter`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Filter {
    None,
    /// skuld's `SERIAL_ALL`.
    All,
    AnyOf(Vec<String>),
}

impl Filter {
    pub fn matches(&self, labels: &[String]) -> bool {
        match self {
            Filter::None => false,
            Filter::All => true,
            Filter::AnyOf(wanted) => wanted.iter().any(|w| labels.contains(w)),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Entry {
    name: String,
    labels: Vec<String>,
    filter: Filter,
}

/// skuld's `can_start`, over the live entries.
fn can_start(live: &[Entry], labels: &[String], filter: &Filter) -> bool {
    if live.iter().any(|e| matches!(e.filter, Filter::All)) {
        return false;
    }
    if live.iter().any(|e| e.filter.matches(labels)) {
        return false;
    }
    if matches!(filter, Filter::All) {
        return live.is_empty();
    }
    live.iter().all(|e| !filter.matches(&e.labels))
}

// Locks -----

/// Blocking exclusive lock; the kernel waits.
pub fn lock(f: &File) -> io::Result<()> {
    #[cfg(unix)]
    loop {
        match rustix::fs::flock(f, rustix::fs::FlockOperation::LockExclusive) {
            Err(rustix::io::Errno::INTR) => continue,
            r => return r.map_err(Into::into),
        }
    }
    #[cfg(windows)]
    f.lock()
}

/// `Ok(false)` if another handle holds the lock.
pub fn try_lock(f: &File) -> io::Result<bool> {
    #[cfg(unix)]
    match rustix::fs::flock(f, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(false),
        Err(e) => Err(e.into()),
    }
    #[cfg(windows)]
    match f.try_lock() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

pub fn unlock(f: &File) -> io::Result<()> {
    #[cfg(unix)]
    return rustix::fs::flock(f, rustix::fs::FlockOperation::Unlock).map_err(Into::into);
    #[cfg(windows)]
    f.unlock()
}

/// Holds `coord.lock`.
struct Mutex(File);

impl Drop for Mutex {
    fn drop(&mut self) {
        unlock(&self.0).expect("unlock coord.lock");
    }
}

fn remove(p: &Path) {
    match fs::remove_file(p) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => panic!("remove {p:?}: {e}"),
        _ => {}
    }
}

/// True if the `.lock` is missing or nobody holds it.
fn owner_dead(lock_path: &Path) -> bool {
    match OpenOptions::new().read(true).write(true).open(lock_path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => panic!("open {lock_path:?}: {e}"),
        Ok(f) => try_lock(&f).expect("try_lock"),
    }
}

// Coordinator -----

pub type Hook = Box<dyn Fn(&str) + Send + Sync>;

pub struct Coordinator {
    dir: PathBuf,
    hook: Option<Hook>,
    /// Spike knob (`LOCKS_SPIKE_FIX`): create and lock `<id>.lock` while
    /// holding `coord.lock`, instead of before taking it. See `create-race`.
    create_under_mutex: bool,
}

pub struct Registration<'a> {
    coord: &'a Coordinator,
    id: String,
    lock: Option<File>,
    /// Failed attempts before the test could start.
    pub waits: u32,
}

impl Coordinator {
    pub fn new(dir: &Path) -> Self {
        fs::create_dir_all(dir.join("running")).expect("create coordination dir");
        Coordinator {
            dir: dir.to_owned(),
            hook: None,
            create_under_mutex: std::env::var_os("LOCKS_SPIKE_FIX").is_some(),
        }
    }

    /// Called with a point name as registration passes it, and `"waiting"`
    /// on a registration's first blocked attempt.
    pub fn with_hook(mut self, hook: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.hook = Some(Box::new(hook));
        self
    }

    fn emit(&self, point: &str) {
        if let Some(h) = &self.hook {
            h(point);
        }
    }

    fn running(&self, file: &str) -> PathBuf {
        self.dir.join("running").join(file)
    }

    fn mutex(&self) -> Mutex {
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join("coord.lock"))
            .expect("open coord.lock");
        lock(&f).expect("lock coord.lock");
        Mutex(f)
    }

    /// Drop dead owners' files; return the live entries. Caller holds the mutex.
    fn scan(&self) -> Vec<Entry> {
        let names: Vec<String> = fs::read_dir(self.dir.join("running"))
            .expect("read running/")
            .map(|e| e.expect("dir entry").file_name().to_string_lossy().into_owned())
            .collect();
        let mut live = Vec::new();
        for n in &names {
            if let Some(id) = n.strip_suffix(".json") {
                if owner_dead(&self.running(&format!("{id}.lock"))) {
                    remove(&self.running(n));
                    remove(&self.running(&format!("{id}.lock")));
                } else {
                    let bytes = fs::read(self.running(n)).expect("read entry");
                    live.push(serde_json::from_slice(&bytes).expect("parse entry"));
                }
            } else if n.ends_with(".tmp") {
                remove(&self.running(n));
            } else if let Some(id) = n.strip_suffix(".lock") {
                if !names.contains(&format!("{id}.json")) && owner_dead(&self.running(n)) {
                    remove(&self.running(n));
                }
            }
        }
        live
    }

    /// Create `<id>.lock` and hold it exclusively.
    fn create_lock(&self, id: &str) -> File {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(self.running(&format!("{id}.lock")))
            .expect("create .lock");
        self.emit("created");
        lock(&f).expect("lock .lock");
        self.emit("owned");
        f
    }

    /// Block until the test may start.
    pub fn register(&self, name: &str, labels: &[String], filter: &Filter) -> Registration<'_> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let id = format!(
            "{}-{}-{nanos}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let mut lock_file = (!self.create_under_mutex).then(|| self.create_lock(&id));
        let (mut waits, mut delay) = (0, Duration::from_millis(10));
        loop {
            self.emit("locking");
            let mutex = self.mutex();
            self.emit("locked");
            lock_file.get_or_insert_with(|| self.create_lock(&id));
            let live = self.scan();
            self.emit("scanned");
            if can_start(&live, labels, filter) {
                let entry = Entry {
                    name: name.to_owned(),
                    labels: labels.to_vec(),
                    filter: filter.clone(),
                };
                let tmp = self.running(&format!("{id}.json.tmp"));
                fs::write(&tmp, serde_json::to_vec(&entry).unwrap()).expect("write entry");
                fs::rename(&tmp, self.running(&format!("{id}.json"))).expect("publish entry");
                self.emit("published");
                drop(mutex);
                return Registration {
                    coord: self,
                    id,
                    lock: lock_file.take(),
                    waits,
                };
            }
            drop(mutex);
            if waits == 0 {
                self.emit("waiting");
            }
            waits += 1;
            std::thread::sleep(delay);
            delay = (delay * 2).min(Duration::from_millis(200));
        }
    }
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        let c = self.coord;
        c.emit("unreg-start");
        let mutex = c.mutex();
        c.emit("unreg-locked");
        remove(&c.running(&format!("{}.json", self.id)));
        c.emit("unreg-json-removed");
        let f = self.lock.take().unwrap();
        unlock(&f).expect("unlock .lock");
        drop(f);
        remove(&c.running(&format!("{}.lock", self.id)));
        c.emit("unreg-lock-removed");
        drop(mutex);
        c.emit("unregistered");
    }
}
