//! The registry of running tests, kept on OS file locks.
//!
//! `coord_dir` (`<resolved>/.skuld`) holds the global mutex (see [`super::lock`]) and `running/`,
//! which holds one file per registered test: `<id>.json`, the [`Entry`]. The owner holds it for
//! the registration's lifetime, and whether another process can still see it held is its
//! liveness, not a PID.
//!
//! - Unix: liveness is the owner's exclusive `flock` on the entry, taken before it is renamed into
//!   place from `<id>.json.tmp`, so a scan never finds a complete entry that is not locked. The
//!   kernel releases the lock when the owner dies; a scan tells by trying to take it.
//! - Windows: liveness is the owner's write handle on the entry, opened with `FILE_SHARE_READ |
//!   FILE_SHARE_WRITE` only, so the entry cannot be deleted while the test runs. A scan tells by
//!   opening the entry for reading with `FILE_SHARE_READ` only, which a write handle refuses; an
//!   open that succeeds means no handle has it open for writing, and the scan deletes the entry.
//!   A refusal is taken for an owner, so another program holding an entry open with no sharing
//!   also reads as one (see `read_if_live`). There is no lock: Windows locks are mandatory and
//!   would block a scan's read.
//!
//! Where each step sits:
//!
//! - Everything that creates or deletes in `running/` does so holding the mutex, so a scan's
//!   deletions and a creator's existence check never race. A waiting test holds no files.
//! - Unix locks the entry before renaming it into place, so no scan finds a complete entry that is
//!   not locked. A crash before the rename leaves a `.tmp`, which the next scan deletes.
//! - Windows probes an entry and only then deletes it: under the mutex no owner can appear between
//!   the two.
//! - Unix reaches `running/` through the mutex's open directory descriptor, never by path, so a
//!   test whose mutex has split (the directory renamed away and replaced) cannot list or delete in
//!   the replacement. The split checks then report the move.
//! - Unregistration locks the mutex target registration kept open, so it opens no new descriptor
//!   and works with none left.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::File;
#[cfg(windows)]
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::file_identity::FileIdentity;
#[cfg(unix)]
use super::lock::{lock_exclusive, try_lock_exclusive};
use super::lock::{open_lock_target, with_locked_target, GlobalLockHeld};
use super::{skuld_debug_eprintln, DB_DIR_HINT, SERIAL_ALL, SERIAL_NONE};
use crate::label::{Label, LabelFilter};

#[cfg(test)]
use super::test_hooks::{self, Seam};

// Layout =====

pub(super) fn running_dir(coord_dir: &Path) -> PathBuf {
    coord_dir.join("running")
}

/// The file name of the entry of `id` in `running/`.
fn entry_name(id: &str) -> String {
    format!("{id}.json")
}

/// The path of `running/<name>`, for messages and, on Windows, for the filesystem calls.
fn entry_file(held: &GlobalLockHeld<'_>, name: &str) -> PathBuf {
    running_dir(held.coord_dir()).join(name)
}

/// `running/<name>` as the mutex's directory descriptor resolves it.
#[cfg(unix)]
fn rel(name: &str) -> String {
    format!("running/{name}")
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

/// `result`'s value, or a panic naming `action` and the file; `path` is built only on a failure.
fn or_panic<T, P: AsRef<Path>>(action: &str, path: impl FnOnce() -> P, result: io::Result<T>) -> T {
    result.unwrap_or_else(|e| panic!("skuld: failed to {action} {:?}: {e} ({DB_DIR_HINT})", path().as_ref()))
}

/// `entry` is the registration's entry, when there is one yet; `inspect_error`, why the mutex's
/// target could not be inspected, when it could not.
fn moved_message(coord_dir: &Path, entry: Option<&Path>, doing: &str, inspect_error: Option<&io::Error>) -> String {
    let lost = match entry {
        Some(entry) => format!("the mutex's target or the entry {entry:?}"),
        None => "the mutex's target".to_owned(),
    };
    let consequence = "the mutex or the entries no longer guard what other tests see";
    match inspect_error {
        None => format!(
            "skuld: the coordination directory {coord_dir:?} moved while {doing}: {lost} was deleted, \
             replaced, or its directory moved, so {consequence} ({DB_DIR_HINT})"
        ),
        Some(e) => format!(
            "skuld: the coordination directory {coord_dir:?} could not be checked while {doing}: \
             inspecting the mutex's target {:?} failed: {e} ({DB_DIR_HINT})",
            super::lock::lock_path(coord_dir)
        ),
    }
}

/// Panic with [`moved_message`] if the mutex has split, or its target cannot be inspected.
fn assert_not_split(held: &GlobalLockHeld<'_>, entry: Option<&Path>, doing: &str) {
    match held.target_has_split() {
        Ok(false) => {}
        Ok(true) => panic!("{}", moved_message(held.coord_dir(), entry, doing, None)),
        Err(e) => panic!("{}", moved_message(held.coord_dir(), entry, doing, Some(&e))),
    }
}

// Warnings =====

/// `warning` the first time `warned` sees that text, `None` after.
pub(super) fn warning_once(warned: &mut HashSet<String>, warning: String) -> Option<String> {
    warned.insert(warning.clone()).then_some(warning)
}

/// Print `warning` to stderr, once per process however often the scan meets the cause.
fn warn_once(warning: String) {
    static WARNED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

    let mut warned = WARNED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(warning) = warning_once(warned.get_or_insert_with(HashSet::new), warning) {
        eprintln!("{warning}");
    }
}

// Files =====

/// Create `path`, which must not exist, readable and writable, without `FILE_SHARE_DELETE` so it
/// cannot be deleted or renamed while open.
#[cfg(windows)]
fn create_new_file(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(super::lock::WINDOWS_SHARE_MODE)
        .open(path)
}

/// Create `path`, resolved against `dir`, which must not exist, readable and writable, mode 0644
/// whatever the umask (so no other uid ever meets a file it cannot open).
#[cfg(unix)]
fn create_new_file_at(dir: &File, path: &str) -> io::Result<File> {
    use rustix::fs::{fchmod, openat, Mode, OFlags};

    let mode = Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::ROTH;
    let flags = OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC;
    let fd = openat(dir, path, flags, mode)?;
    // `mode` is masked by the umask; force 0644.
    fchmod(&fd, mode)?;
    Ok(File::from(fd))
}

/// Create the entry of `id` in `running/` holding `bytes`, and keep it held. `AlreadyExists` if the
/// id is taken.
///
/// Unix: written to `<id>.json.tmp` at 0644 and locked, then renamed into place, so a crash never
/// leaves an entry another uid cannot open or one that is not held. The existence check is
/// race-free because every creator holds the mutex.
fn create_entry_file(held: &GlobalLockHeld<'_>, id: &str, bytes: &[u8]) -> io::Result<File> {
    let name = entry_name(id);
    #[cfg(unix)]
    let (mut file, written) = {
        use rustix::fs::{statat, AtFlags};

        match statat(held.dir(), rel(&name), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
            Err(rustix::io::Errno::NOENT) => {}
            Err(e) => return Err(e.into()),
        }
        let tmp = format!("{name}.tmp");
        (create_new_file_at(held.dir(), &rel(&tmp))?, tmp)
    };
    #[cfg(windows)]
    let (mut file, written) = (create_new_file(&entry_file(held, &name))?, name.clone());

    #[cfg(test)]
    test_hooks::run_seam(Seam::Created);
    or_panic("write", || entry_file(held, &written), file.write_all(bytes));
    #[cfg(unix)]
    {
        or_panic("lock", || entry_file(held, &written), lock_exclusive(&file));
        let renamed = rustix::fs::renameat(held.dir(), rel(&written), held.dir(), rel(&name));
        or_panic("publish", || entry_file(held, &name), renamed.map_err(io::Error::from));
    }
    Ok(file)
}

/// Create the entry under a fresh id; returns the id and the held file.
fn create_entry(held: &GlobalLockHeld<'_>, ids: &IdGenerator, entry: &Entry) -> (String, File) {
    let bytes = serde_json::to_vec(entry).expect("an entry of strings serializes");
    loop {
        let id = ids.next();
        match create_entry_file(held, &id, &bytes) {
            Ok(file) => return (id, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => or_panic("create", || entry_file(held, &entry_name(&id)), Err(e)),
        }
    }
}

/// The identity of the file `running/<name>` names now.
fn entry_identity(held: &GlobalLockHeld<'_>, name: &str) -> io::Result<FileIdentity> {
    #[cfg(unix)]
    {
        FileIdentity::try_of_at(held.dir(), Path::new(&rel(name)))
    }
    #[cfg(windows)]
    {
        FileIdentity::try_of(&entry_file(held, name))
    }
}

/// Panic, saying the coordination directory moved while `doing`, if the mutex has split or
/// `running/<name>` is no longer the file `identity` was taken of. A mutex target or entry that
/// cannot be inspected for any other reason panics with that error.
fn assert_unmoved(held: &GlobalLockHeld<'_>, name: &str, identity: FileIdentity, doing: &str) {
    let path = entry_file(held, name);
    assert_not_split(held, Some(&path), doing);
    let moved = match entry_identity(held, name) {
        Ok(now) => now != identity,
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => or_panic("inspect", || &path, Err::<bool, _>(e)),
    };
    if moved {
        panic!("{}", moved_message(held.coord_dir(), Some(&path), doing, None));
    }
}

// Scan =====

/// The names in `running/`, those that are not UTF-8 left out (not ours). Errors with `NotFound`
/// if `running/` is gone.
#[cfg(unix)]
fn list_names(held: &GlobalLockHeld<'_>) -> io::Result<Vec<String>> {
    use rustix::fs::{openat, Dir, Mode, OFlags};

    let fd = openat(
        held.dir(),
        "running",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut names = Vec::new();
    for entry in Dir::new(fd)? {
        if let Ok(name) = entry?.file_name().to_str() {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

#[cfg(windows)]
fn list_names(held: &GlobalLockHeld<'_>) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(running_dir(held.coord_dir()))? {
        if let Ok(name) = entry?.file_name().into_string() {
            names.push(name);
        }
    }
    Ok(names)
}

/// The entry `running/<name>` if its owner is alive; if it is dead, delete it and return `None`.
/// An entry that does not parse panics, naming the file.
///
/// Unix: open read-only (so another uid's 0644 entry works too) and try-lock; only a dead owner's
/// entry can be locked.
#[cfg(unix)]
fn read_if_live(held: &GlobalLockHeld<'_>, name: &str) -> Option<Entry> {
    use rustix::fs::{openat, Mode, OFlags};
    use std::io::Read;

    let path = || entry_file(held, name);
    let mut file = match openat(held.dir(), rel(name), OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty()) {
        Err(rustix::io::Errno::NOENT) => return None,
        Err(e) => or_panic("open", path, Err(e.into())),
        Ok(fd) => File::from(fd),
    };
    match try_lock_exclusive(&file) {
        Ok(()) => {
            remove_entry(held, name);
            None
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut bytes = Vec::new();
            or_panic("read", path, file.read_to_end(&mut bytes));
            Some(parse_entry(path, &bytes))
        }
        Err(std::fs::TryLockError::Error(e)) => or_panic("try-lock", path, Err(e)),
    }
}

fn parse_entry<P: AsRef<Path>>(path: impl FnOnce() -> P, bytes: &[u8]) -> Entry {
    serde_json::from_slice(bytes).unwrap_or_else(|e| {
        panic!(
            "skuld: unparseable coordination entry {:?}: {e} ({DB_DIR_HINT})",
            path().as_ref()
        )
    })
}

/// What the Windows scan concludes about an entry.
#[cfg(windows)]
pub(super) enum Verdict {
    /// Not live: no entry, one being torn down, or a dead one that another program stops us
    /// deleting (still present, and the next scan retries). Not the access-denied case, which is
    /// a `Fail`.
    Gone,
    /// An owner holds it for writing: read and parse it.
    Live,
    /// Nobody holds it for writing: delete it.
    Dead,
    /// None of these; the scan fails with this.
    Fail(io::Error),
}

#[cfg(windows)]
fn is_os_error(e: &io::Error, code: windows::Win32::Foundation::WIN32_ERROR) -> bool {
    e.raw_os_error() == Some(code.0 as i32)
}

#[cfg(windows)]
impl Entry {
    /// What the scan makes of a live entry it cannot read, another program holding it open or
    /// locked: its constraints are unknown, so it blocks every registration, like a global-serial
    /// test.
    fn unreadable(path: &Path) -> Self {
        Self {
            name: path.display().to_string(),
            labels: Vec::new(),
            filter: SERIAL_ALL.to_owned(),
        }
    }
}

/// The warning for the unreadable entry `path`.
#[cfg(windows)]
pub(super) fn unreadable_warning(path: &Path) -> String {
    format!(
        "[skuld] warning: another program holds the coordination entry {} open or locked, so every test \
         waits until it closes it",
        path.display()
    )
}

/// Classify `result`, the result of one filesystem attempt on an entry: the scan's probe, an open
/// for reading with `FILE_SHARE_READ` only (a write handle, which only a live owner has, refuses
/// it with a sharing violation, so an `Ok` proves there is no owner), or the delete of an entry
/// with no owner. `on_ok` and `on_sharing_violation` say what those mean for the attempt at hand.
///
/// `ERROR_ACCESS_DENIED` is either an entry that is delete-pending (deleted, but some process
/// still has it open), which `pending` tells, so it is being torn down and gone; or this account
/// has no permission to the entry, which fails loudly.
#[cfg(windows)]
fn classify(
    result: io::Result<()>,
    on_ok: Verdict,
    on_sharing_violation: Verdict,
    pending: impl FnOnce() -> io::Result<bool>,
) -> Verdict {
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};

    let e = match result {
        Ok(()) => return on_ok,
        Err(e) => e,
    };
    if e.kind() == io::ErrorKind::NotFound {
        Verdict::Gone
    } else if is_os_error(&e, ERROR_SHARING_VIOLATION) {
        on_sharing_violation
    } else if is_os_error(&e, ERROR_ACCESS_DENIED) {
        match pending() {
            Ok(true) => Verdict::Gone,
            Ok(false) => Verdict::Fail(e),
            // The last handle closed since the denial.
            Err(check) if check.kind() == io::ErrorKind::NotFound => Verdict::Gone,
            Err(check) => Verdict::Fail(io::Error::other(format!(
                "{e}; checking whether it is delete-pending: {check}"
            ))),
        }
    } else {
        Verdict::Fail(e)
    }
}

/// [`classify`] for the probe: no error means nobody has the entry open for writing (`Dead`), a
/// sharing violation that somebody does (`Live`).
#[cfg(windows)]
pub(super) fn classify_probe(result: io::Result<()>, pending: impl FnOnce() -> io::Result<bool>) -> Verdict {
    classify(result, Verdict::Dead, Verdict::Live, pending)
}

/// [`classify`] for deleting an entry nobody has open for writing: deleted, or kept open by
/// another program (a sharing violation), it is `Gone` for this scan.
#[cfg(windows)]
pub(super) fn classify_removal(result: io::Result<()>, pending: impl FnOnce() -> io::Result<bool>) -> Verdict {
    classify(result, Verdict::Gone, Verdict::Gone, pending)
}

/// Delete `running/<name>`, which has no owner: its registration closed its hold, or its owner
/// died. Already gone is fine. Windows: another program holding it open (an antivirus scanner,
/// say) is not a failure either: the entry is dead all the same and the next scan deletes it.
#[cfg(windows)]
fn remove_entry(held: &GlobalLockHeld<'_>, name: &str) {
    use windows::Win32::Foundation::ERROR_SHARING_VIOLATION;

    let path = entry_file(held, name);
    let result = std::fs::remove_file(&path);
    if matches!(&result, Err(e) if is_os_error(e, ERROR_SHARING_VIOLATION)) {
        skuld_debug_eprintln!("{path:?} is held open by another program, so it stays until the next scan");
    }
    if let Verdict::Fail(e) = classify_removal(result, || crate::win_nt::is_delete_pending(&path)) {
        or_panic("remove", || &path, Err::<(), _>(e));
    }
}

#[cfg(unix)]
fn remove_entry(held: &GlobalLockHeld<'_>, name: &str) {
    use rustix::fs::{unlinkat, AtFlags};

    match unlinkat(held.dir(), rel(name), AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => {}
        Err(e) => or_panic("remove", || entry_file(held, name), Err::<(), _>(e.into())),
    }
}

/// See the Unix variant. Windows: probe the entry with a share mode that a write handle refuses
/// (see [`classify`]). An owner's write handle is what makes an entry live, so the entry of a
/// crashed owner opens and is deleted; a live one is read and parsed.
///
/// Another program may hold an entry open with no sharing, or lock it, which refuses the probe
/// like an owner does and refuses the read too. Its constraints are unknown, so it counts as a live
/// global-serial entry until that program closes it.
#[cfg(windows)]
fn read_if_live(held: &GlobalLockHeld<'_>, name: &str) -> Option<Entry> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Foundation::{ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION};
    use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;

    let path = &*entry_file(held, name);
    let probe = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(path)
        // Closed here, before the delete.
        .map(drop);
    match classify_probe(probe, || crate::win_nt::is_delete_pending(path)) {
        Verdict::Gone => None,
        Verdict::Live => match std::fs::read(path) {
            Ok(bytes) => Some(parse_entry(|| path, &bytes)),
            Err(e) if is_os_error(&e, ERROR_SHARING_VIOLATION) || is_os_error(&e, ERROR_LOCK_VIOLATION) => {
                warn_once(unreadable_warning(path));
                Some(Entry::unreadable(path))
            }
            Err(e) => or_panic("read", || path, Err(e)),
        },
        Verdict::Dead => {
            remove_entry(held, name);
            None
        }
        Verdict::Fail(e) => or_panic("open", || path, Err(e)),
    }
}

/// Delete what dead owners left and return the live entries. The caller holds the mutex. Errors
/// with `NotFound` if `running/` is gone.
fn scan(held: &GlobalLockHeld<'_>) -> io::Result<Vec<Entry>> {
    let mut live = Vec::new();
    for name in list_names(held)? {
        if name.ends_with(".tmp") {
            // Only an owner holding the mutex has one, so a scan seeing it means a crash left it.
            remove_entry(held, &name);
        } else if name.ends_with(".json") {
            live.extend(read_if_live(held, &name));
        }
    }
    Ok(live)
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
            Err(e) => warn_once(format!(
                "[skuld] warning: skipping unparseable serial_filter {:?} of running test {:?}: {e}",
                entry.filter, entry.name
            )),
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

/// A registered test. [`Self::unregister`] ends it; dropping without it closes the hold, which
/// marks the entry dead for the next scan once every fd sharing its lock has closed.
pub(super) struct Registration {
    coord_dir: PathBuf,
    id: String,
    /// The entry, held open (and on Unix locked); `None` once unregistered.
    hold: Option<File>,
    /// Of the file `hold` has open, for checking that `running/<id>.json` is still that file.
    entry_identity: FileIdentity,
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

    /// Remove the entry, under the mutex. Panics, saying the coordination directory moved, if the
    /// mutex split or `running/<id>.json` is no longer the file this registration made. A second
    /// call, after success, does nothing.
    pub(super) fn unregister(&mut self) {
        if self.hold.is_none() {
            return;
        }
        let name = entry_name(&self.id);
        with_locked_target(&self.coord_dir, &self.mutex_target, |held| {
            #[cfg(test)]
            test_hooks::run_seam(Seam::Unregistering);
            assert_unmoved(held, &name, self.entry_identity, "unregistering a test");
            // Closed first: Windows refuses to delete a file its own handle holds open.
            drop(self.hold.take().expect("checked above"));
            remove_entry(held, &name);
        });
    }
}

/// One attempt, under the mutex: `Some` if the test can start (now registered), `None` if other
/// tests block it. Panics if the coordination directory moved or cannot be used.
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
        || &running,
        super::db_dir::create_dir_all_open(&running),
    );
    #[cfg(test)]
    test_hooks::run_seam(Seam::RunningCreated);
    let mutex_target = open_lock_target(coord_dir);
    #[cfg(test)]
    test_hooks::run_seam(Seam::TargetOpened);
    let started = with_locked_target(coord_dir, &mutex_target, |held| {
        assert_not_split(held, None, "registering a test");
        #[cfg(test)]
        test_hooks::run_seam(Seam::Scanning);
        let entries = or_panic("list", || &running, scan(held));
        can_start(&entries, labels, storage_filter).then(|| publish(held, ids, name, labels, storage_filter))
    });
    started.map(|(id, hold, entry_identity)| Registration {
        coord_dir: coord_dir.to_path_buf(),
        id,
        hold: Some(hold),
        entry_identity,
        mutex_target,
    })
}

/// Create, hold and publish the entry, returning its id, held file and that file's identity. The
/// caller holds the mutex and has decided the test can start.
fn publish(
    held: &GlobalLockHeld<'_>,
    ids: &IdGenerator,
    name: &str,
    labels: &[Label],
    storage_filter: &str,
) -> (String, File, FileIdentity) {
    let entry = Entry {
        name: name.to_owned(),
        labels: labels.iter().map(|l| l.name().to_owned()).collect(),
        filter: storage_filter.to_owned(),
    };
    let (id, file) = create_entry(held, ids, &entry);
    #[cfg(test)]
    test_hooks::run_seam(Seam::Published);

    // A split after the check before scanning would have put the entry in a directory the mutex no
    // longer guards.
    let name = entry_name(&id);
    let identity = or_panic("inspect", || entry_file(held, &name), FileIdentity::of_file(&file));
    assert_unmoved(held, &name, identity, "registering a test");
    (id, file, identity)
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
