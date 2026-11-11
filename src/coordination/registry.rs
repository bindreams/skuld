//! The registry of running tests, kept on OS file locks.
//!
//! `coord_dir` (`<resolved>/.skuld`) holds the global mutex (see [`super::lock`], taken for
//! [`mutex_base`]) and `running/`, which holds per registered test:
//!
//! - `<id>.lock`, created and locked by its owner for the registration's lifetime. The kernel
//!   releases the lock when the owner dies, so liveness is whether the lock can be taken, not a
//!   PID.
//! - `<id>.json`, the [`Entry`], published by rename.
//!
//! Everything that creates or deletes in `running/` does so holding the mutex. A waiting test
//! holds no files. See the "Layout", "Register" and "Unregister" sections of the spec for why each
//! step is where it is.

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::file_identity::FileIdentity;
use super::lock::{lock_exclusive, open_lock_target, try_lock_exclusive, unlock, with_locked_target, GlobalLockHeld};
use super::{skuld_debug_eprintln, DB_DIR_HINT, SERIAL_ALL, SERIAL_NONE};
use crate::label::{Label, LabelFilter};

#[cfg(test)]
use super::test_hooks::{self, Seam};

// Layout =====

/// The base path the global mutex is taken for: Unix locks `coord_dir` itself, Windows
/// `coord_dir/global.lock`.
pub(super) fn mutex_base(coord_dir: &Path) -> PathBuf {
    coord_dir.join("global")
}

pub(super) fn running_dir(coord_dir: &Path) -> PathBuf {
    coord_dir.join("running")
}

fn lock_path(running: &Path, id: &str) -> PathBuf {
    running.join(format!("{id}.lock"))
}

fn json_path(running: &Path, id: &str) -> PathBuf {
    running.join(format!("{id}.json"))
}

/// What a registered test publishes. Unknown fields are ignored, so a later version can add some.
#[derive(Serialize, Deserialize)]
struct Entry {
    name: String,
    labels: Vec<String>,
    /// `to_storage`'s canonical form: `""` none, `"*"` all.
    filter: String,
}

// Ids =====

/// Hands out `<pid>-<counter>-<unix nanos>` ids. The counter advances per id, so an id that
/// collides with an existing file is replaced by asking again.
pub(super) struct IdGenerator {
    nanos: u128,
    counter: AtomicU64,
}

impl IdGenerator {
    pub(super) fn new() -> Self {
        Self {
            nanos: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            counter: AtomicU64::new(0),
        }
    }

    /// The id handed out for counter value `n`.
    pub(super) fn id_at(&self, n: u64) -> String {
        format!("{}-{n}-{}", std::process::id(), self.nanos)
    }

    fn next(&self) -> String {
        self.id_at(self.counter.fetch_add(1, Ordering::Relaxed))
    }
}

pub(super) static PROCESS_IDS: LazyLock<IdGenerator> = LazyLock::new(IdGenerator::new);

// Failures =====

fn or_panic<T>(action: &str, path: &Path, result: io::Result<T>) -> T {
    result.unwrap_or_else(|e| panic!("skuld: failed to {action} {path:?}: {e} ({DB_DIR_HINT})"))
}

fn moved_message(coord_dir: &Path, doing: &str) -> String {
    format!(
        "skuld: the coordination directory {coord_dir:?} moved while {doing}: it, or running/ in it, was \
         deleted or replaced, so the mutex or the entries no longer guard what other tests see ({DB_DIR_HINT})"
    )
}

// Files =====

/// Create `path`, which must not exist, readable and writable, mode 0644 whatever the umask (so
/// no other uid ever meets a file it cannot open), and on Windows without `FILE_SHARE_DELETE` so
/// it cannot be deleted or renamed while open.
fn create_new_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o644);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(super::lock::WINDOWS_SHARE_MODE);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // `mode` above was trimmed by the umask.
        file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
    }
    Ok(file)
}

/// Windows: hold `path` open, read-only and without `FILE_SHARE_DELETE`, so it cannot be deleted
/// or renamed. A recursive delete that carries on past errors would otherwise remove a live
/// `.json` and leave its `.lock`, which the scan ignores. Readers (the scan) are unaffected. Unix
/// has no such handle: a deleted entry is found at unregistration.
fn hold_open(path: &Path) -> io::Result<Option<File>> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .share_mode(super::lock::WINDOWS_SHARE_MODE)
            .open(path)
            .map(Some)
    }
    #[cfg(unix)]
    {
        let _ = path;
        Ok(None)
    }
}

fn remove_if_exists(path: &Path) {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => or_panic("remove", path, Err::<(), _>(e)),
        _ => {}
    }
}

/// Create `<id>.lock` in `running`. `AlreadyExists` if the id is taken.
///
/// Unix: through a `.lock.tmp` at 0644, so a crash never leaves a `.lock` another uid cannot
/// open. The existence check is race-free because every creator holds the mutex.
fn create_lock_file(running: &Path, id: &str) -> io::Result<File> {
    let lock = lock_path(running, id);
    #[cfg(unix)]
    {
        match lock.symlink_metadata() {
            Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let tmp = running.join(format!("{id}.lock.tmp"));
        let file = create_new_file(&tmp)?;
        std::fs::rename(&tmp, &lock)?;
        Ok(file)
    }
    #[cfg(windows)]
    {
        create_new_file(&lock)
    }
}

/// Create and lock the lock file under a fresh id; returns the id and the locked file.
fn create_lock(ids: &IdGenerator, running: &Path) -> (String, File) {
    loop {
        let id = ids.next();
        match create_lock_file(running, &id) {
            Ok(file) => {
                #[cfg(test)]
                test_hooks::run_seam(Seam::Created);
                or_panic("lock", &lock_path(running, &id), lock_exclusive(&file));
                return (id, file);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => or_panic("create", &lock_path(running, &id), Err(e)),
        }
    }
}

/// Write the entry to `<id>.json.tmp`, then rename it into place.
fn publish_entry(running: &Path, id: &str, entry: &Entry) {
    use std::io::Write;

    let tmp = running.join(format!("{id}.json.tmp"));
    let target = json_path(running, id);
    let bytes = serde_json::to_vec(entry).expect("an entry of strings serializes");
    let mut file = or_panic("create", &tmp, create_new_file(&tmp));
    or_panic("write", &tmp, file.write_all(&bytes));
    drop(file);
    or_panic("publish", &target, std::fs::rename(&tmp, &target));
}

// Scan =====

/// True when nobody holds `lock`, or it does not exist. Opened read-only: a 0644 lock owned by
/// another uid can still be try-locked, and the handle is closed before the caller deletes.
fn owner_gone(lock: &Path) -> bool {
    match File::open(lock) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => or_panic("open", lock, Err(e)),
        Ok(file) => match try_lock_exclusive(&file) {
            Ok(()) => true,
            Err(std::fs::TryLockError::WouldBlock) => false,
            Err(std::fs::TryLockError::Error(e)) => or_panic("try-lock", lock, Err(e)),
        },
    }
}

/// Delete what dead owners left and return the live entries. The caller holds the mutex.
fn scan(running: &Path) -> Vec<Entry> {
    let names: Vec<String> = or_panic("list", running, std::fs::read_dir(running))
        .map(|e| or_panic("list", running, e).file_name())
        // Not ours if not UTF-8.
        .filter_map(|n| n.into_string().ok())
        .collect();
    let mut live = Vec::new();
    for name in &names {
        let path = running.join(name);
        if name.ends_with(".tmp") {
            remove_if_exists(&path);
        } else if let Some(id) = name.strip_suffix(".json") {
            let lock = lock_path(running, id);
            if owner_gone(&lock) {
                remove_if_exists(&path);
                remove_if_exists(&lock);
            } else {
                let bytes = or_panic("read", &path, std::fs::read(&path));
                let entry = serde_json::from_slice(&bytes)
                    .unwrap_or_else(|e| panic!("skuld: unparseable coordination entry {path:?}: {e} ({DB_DIR_HINT})"));
                live.push(entry);
            }
        } else if let Some(id) = name.strip_suffix(".lock") {
            if !names.contains(&format!("{id}.json")) && owner_gone(&path) {
                remove_if_exists(&path);
            }
        }
    }
    live
}

// Blocking checks =====

/// Whether a test with `my_labels` and `my_serial_filter` (already canonical) can start, given
/// the live `entries`.
fn can_start(entries: &[Entry], my_labels: &[Label], my_serial_filter: &str) -> bool {
    // (a) Is a global-serial test running?
    if entries.iter().any(|e| e.filter == SERIAL_ALL) {
        return false;
    }

    // (b) Does any running serial test's filter match my labels?
    for entry in entries.iter().filter(|e| e.filter != SERIAL_NONE) {
        // New entries are canonical, so a parse failure is an entry from another version. Warn
        // and skip; it goes when its owner exits.
        match LabelFilter::parse(&entry.filter) {
            Ok(filter) => {
                if filter.matches(my_labels) {
                    return false;
                }
            }
            Err(e) => eprintln!(
                "[skuld] warning: skipping unparseable serial_filter {:?} of running test {:?}: {e}",
                entry.filter, entry.name
            ),
        }
    }

    // (c) If I'm global-serial, is anything running?
    if my_serial_filter == SERIAL_ALL {
        return entries.is_empty();
    }

    // (d) If I have a filter, does any running test's labels match it?
    //
    // `my_serial_filter` always arrives canonical, and `validate_serial_filters` proved it
    // parseable at startup. An Err here is a logic bug, not a runtime condition.
    if my_serial_filter != SERIAL_NONE {
        let filter = LabelFilter::parse(my_serial_filter)
            .expect("can_start: my_serial_filter must parse — guarded by validate_serial_filters");
        if entries.iter().any(|e| {
            let names: Vec<&str> = e.labels.iter().map(String::as_str).collect();
            filter.matches_names(&names)
        }) {
            return false;
        }
    }

    true
}

// Registration =====

/// A registered test. [`Self::unregister`] ends it; dropping without it only closes the handle,
/// which marks the entry dead for the next scan.
pub(super) struct Registration {
    coord_dir: PathBuf,
    id: String,
    /// `None` once unregistered.
    lock: Option<File>,
    /// Of the file `lock` has open, for checking that `running/<id>.lock` is still that file.
    lock_identity: FileIdentity,
    /// Windows: `<id>.json`, held open until [`Self::unregister`] (see [`hold_open`]).
    json_hold: Option<File>,
    /// The mutex's lock target, held open (and unlocked) since registration so that
    /// [`Self::unregister`] locks it again without opening anything: a test that has used up its
    /// file descriptors must still be able to unregister.
    mutex_target: File,
}

impl Registration {
    #[cfg(test)]
    pub(super) fn id(&self) -> &str {
        &self.id
    }

    /// Where this registration's `.json` entry lives.
    pub(super) fn json_path(&self) -> PathBuf {
        json_path(&running_dir(&self.coord_dir), &self.id)
    }

    /// Remove the entry and the lock file, under the mutex. Panics, saying the coordination
    /// directory moved, if the mutex split or `running/<id>.lock` or `running/<id>.json` is no
    /// longer what this registration made. A second call, after success, does nothing.
    pub(super) fn unregister(&mut self) {
        if self.lock.is_none() {
            return;
        }
        let coord_dir = self.coord_dir.clone();
        let running = running_dir(&coord_dir);
        let (lock, json) = (lock_path(&running, &self.id), json_path(&running, &self.id));
        // Opens nothing on Unix: `stat`, `metadata` and `remove_file` need no descriptor. (Windows
        // has no descriptor limit, and `target_has_split` there reopens the lock file.)
        with_locked_target(&mutex_base(&coord_dir), &self.mutex_target, |held| {
            #[cfg(test)]
            test_hooks::run_seam(Seam::Unregistering);
            let json_exists = or_panic("inspect", &json, json.try_exists());
            if held.target_has_split() || FileIdentity::of(&lock) != Some(self.lock_identity) || !json_exists {
                panic!("{}", moved_message(&coord_dir, "unregistering a test"));
            }
            drop(self.json_hold.take());
            or_panic("remove", &json, std::fs::remove_file(&json));
            let file = self.lock.take().expect("checked above");
            or_panic("unlock", &lock, unlock(&file));
            drop(file);
            or_panic("remove", &lock, std::fs::remove_file(&lock));
        });
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(file) = self.lock.take() {
            // Never panics. Unlocking first only hastens what closing does anyway.
            let _ = unlock(&file);
        }
    }
}

/// One attempt, under the mutex: `Some` if the test can start (now registered), `None` if it
/// must wait.
pub(super) fn try_register(
    ids: &IdGenerator,
    coord_dir: &Path,
    name: &str,
    labels: &[Label],
    storage_filter: &str,
) -> Option<Registration> {
    let running = running_dir(coord_dir);
    or_panic(
        "create the coordination directory",
        &running,
        super::db_dir::create_dir_all_open(&running),
    );
    let base = mutex_base(coord_dir);
    let mutex_target = open_lock_target(&base);
    let (id, lock, lock_identity, json_hold) = with_locked_target(&base, &mutex_target, |held| {
        if held.target_has_split() {
            panic!("{}", moved_message(coord_dir, "registering a test"));
        }
        let entries = scan(&running);
        if !can_start(&entries, labels, storage_filter) {
            return None;
        }
        Some(publish(held, ids, coord_dir, name, labels, storage_filter))
    })?;
    Some(Registration {
        coord_dir: coord_dir.to_path_buf(),
        id,
        lock: Some(lock),
        lock_identity,
        json_hold,
        mutex_target,
    })
}

/// Create, lock and publish the registration, returning its id, locked file, that file's
/// identity and the held `.json` (see [`hold_open`]). The caller holds the mutex and has decided the test can start.
fn publish(
    held: &GlobalLockHeld<'_>,
    ids: &IdGenerator,
    coord_dir: &Path,
    name: &str,
    labels: &[Label],
    storage_filter: &str,
) -> (String, File, FileIdentity, Option<File>) {
    let running = running_dir(coord_dir);
    let (id, file) = create_lock(ids, &running);
    let entry = Entry {
        name: name.to_owned(),
        labels: labels.iter().map(|l| l.name().to_owned()).collect(),
        filter: storage_filter.to_owned(),
    };
    publish_entry(&running, &id, &entry);
    let json_hold = or_panic("hold", &json_path(&running, &id), hold_open(&json_path(&running, &id)));
    #[cfg(test)]
    test_hooks::run_seam(Seam::Published);

    // A split after `try_register`'s check would have put the entry in a directory the mutex no
    // longer guards.
    let lock_identity = or_panic("inspect", &lock_path(&running, &id), FileIdentity::of_file(&file));
    if held.target_has_split() || FileIdentity::of(&lock_path(&running, &id)) != Some(lock_identity) {
        panic!("{}", moved_message(coord_dir, "registering a test"));
    }
    (id, file, lock_identity, json_hold)
}

/// [`register`] with the ids from `ids`.
pub(super) fn register_with(
    ids: &IdGenerator,
    coord_dir: &Path,
    name: &str,
    labels: &[Label],
    storage_filter: &str,
) -> Registration {
    let mut backoff = Duration::from_millis(10);
    let max_backoff = Duration::from_millis(200);
    let warn_after = Duration::from_secs(60);
    let started = Instant::now();
    let mut warned = false;
    let mut logged_first_wait = false;

    loop {
        if let Some(registration) = try_register(ids, coord_dir, name, labels, storage_filter) {
            return registration;
        }
        #[cfg(test)]
        test_hooks::signal_retry();
        if !logged_first_wait {
            logged_first_wait = true;
            skuld_debug_eprintln!(
                "coordination: {name} is blocked on a serial constraint and will wait \
                 (if this run used a generated nextest tool-config-file, this means \
                 nextest scheduled it concurrently with a conflicting test anyway — the \
                 config may be stale; re-run `cargo skuld nextest gen --check`)"
            );
        }
        if !warned && started.elapsed() > warn_after {
            warned = true;
            skuld_debug_eprintln!("coordination: {name} has been waiting >60s for serial constraints");
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(max_backoff);
    }
}

/// Block until the test may start, then register it. `coord_dir` is `<resolved>/.skuld`;
/// `storage_filter` is `to_storage`'s canonical form.
///
/// Retries uncapped, 10 ms to 200 ms apart.
pub(super) fn register(coord_dir: &Path, name: &str, labels: &[Label], storage_filter: &str) -> Registration {
    register_with(&PROCESS_IDS, coord_dir, name, labels, storage_filter)
}
