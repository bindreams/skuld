//! Name-based fixture system with three scopes: Variable, Test, and Process.
//!
//! Fixtures are registered by name via `#[skuld::fixture]` and looked up at
//! runtime via [`fixture_get`] or the convenience [`fixture`]. Each fixture has a
//! scope that controls its lifetime:
//!
//! - **Variable** (default): fresh instance per request, dropped when the handle drops.
//! - **Test**: cached per test scope, dropped when the test ends.
//! - **Process**: cached globally, dropped when the test runner finishes (LIFO).
//!
//! Scope dependency rule: a fixture may only depend on fixtures of the **same or
//! wider** scope (Variable < Test < Process).

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use crate::RequireFn;

// Scope =======================================================================================

/// Fixture lifetime scope, ordered from narrow to wide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FixtureScope {
    /// Fresh instance every request. Dropped when the [`FixtureHandle`] drops.
    Variable = 0,
    /// Cached per test. Dropped when the test scope ends.
    Test = 1,
    /// Cached per process. Dropped when [`cleanup_process_fixtures`] runs.
    Process = 2,
}

// FixtureRef (thin + fat pointer support) =====================================================

/// Opaque wrapper storing a reference as raw bytes.
///
/// Supports both thin pointers (`&Counter`) and fat pointers (`&Path`, `&str`)
/// by copying the reference representation into a fixed-size buffer.
pub struct FixtureRef {
    words: [usize; 2],
    size: usize,
}

impl FixtureRef {
    /// Store a reference of any type (thin or fat).
    pub fn from_ref<T: ?Sized>(r: &T) -> Self {
        let size = std::mem::size_of::<&T>();
        debug_assert!(size <= std::mem::size_of::<[usize; 2]>());
        let mut words = [0usize; 2];
        // SAFETY: we copy exactly `size` bytes from a valid reference into `words`.
        unsafe {
            std::ptr::copy_nonoverlapping(&r as *const &T as *const u8, words.as_mut_ptr().cast::<u8>(), size);
        }
        FixtureRef { words, size }
    }

    /// Reconstitute the stored reference.
    ///
    /// The reference is reconstructed by copying the stored bytes back into a
    /// reference value. The `FixtureRef` is consumed (not borrowed) so the
    /// returned reference has an unbounded lifetime — the caller must ensure
    /// the underlying value is still alive.
    ///
    /// # Safety
    ///
    /// `T` must match the type used in [`from_ref`](Self::from_ref), and the
    /// underlying value must still be alive for the lifetime of the returned ref.
    pub unsafe fn cast<T: ?Sized + 'static>(self) -> &'static T {
        debug_assert_eq!(self.size, std::mem::size_of::<&T>());
        std::ptr::read(self.words.as_ptr().cast::<&T>())
    }
}

// FixtureHandle ===============================================================================

/// Handle returned by [`fixture_get`]. For Variable scope, dropping the handle
/// drops the fixture value. For Test/Process scope, dropping is a no-op.
pub struct FixtureHandle {
    /// `Some` for Variable scope (handle owns the value), `None` for cached.
    _owner: Option<Box<dyn Any + Send + Sync>>,
    fixture_ref: FixtureRef,
}

impl FixtureHandle {
    /// Extract the stored reference.
    ///
    /// The reference is valid as long as this handle is alive. For Variable scope,
    /// the handle owns the value. For Test/Process scope, the value lives in a cache.
    ///
    /// # Safety
    ///
    /// `T` must match the type (or Deref::Target) of the fixture value.
    pub unsafe fn as_ref<T: ?Sized + 'static>(&self) -> &T {
        // Re-read from the stored words (same data, not borrowing self).
        debug_assert_eq!(self.fixture_ref.size, std::mem::size_of::<&T>());
        std::ptr::read(self.fixture_ref.words.as_ptr().cast::<&T>())
    }
}

// FixtureDef ==================================================================================

/// Descriptor for a registered fixture. Created by `#[skuld::fixture]` and
/// collected via [`inventory`].
pub struct FixtureDef {
    /// Fixture name (function name or `name = "..."` override).
    pub name: &'static str,
    /// Lifetime scope.
    pub scope: FixtureScope,
    /// Runtime preconditions inherited by any test that uses this fixture.
    pub requires: &'static [RequireFn],
    /// Names of fixtures this one depends on (from `#[fixture]` params).
    pub deps: &'static [&'static str],
    /// Factory: create a new instance boxed as `dyn Any`.
    pub setup: fn() -> Result<Box<dyn Any + Send + Sync>, String>,
    /// Cast a stored value to a target TypeId. Returns `None` if the target type
    /// is not supported (neither the fixture's own type nor its Deref::Target).
    pub cast: fn(&(dyn Any + Send + Sync), TypeId) -> Option<FixtureRef>,
    /// Human-readable type name for error messages.
    pub type_name: &'static str,
}

inventory::collect!(FixtureDef);

// Global registry =============================================================================

/// Lazily-built index from fixture name to its definition. Validated on first
/// access: duplicate names, dependency cycles, and scope violations are panics.
pub fn fixture_registry() -> &'static HashMap<&'static str, &'static FixtureDef> {
    static REGISTRY: OnceLock<HashMap<&'static str, &'static FixtureDef>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut map = HashMap::new();
        for def in inventory::iter::<FixtureDef> {
            if let Some(prev) = map.insert(def.name, def) {
                panic!(
                    "duplicate fixture name {:?}: registered by {} and {}",
                    def.name, prev.type_name, def.type_name
                );
            }
        }
        validate_fixture_graph(&map);
        map
    })
}

fn validate_fixture_graph(registry: &HashMap<&str, &FixtureDef>) {
    for def in registry.values() {
        for &dep_name in def.deps {
            // Missing dependency.
            let dep = registry.get(dep_name).unwrap_or_else(|| {
                panic!(
                    "fixture {:?} depends on {:?} which is not registered",
                    def.name, dep_name
                );
            });
            // Scope violation: fixture depends on a narrower-scoped fixture.
            if dep.scope < def.scope {
                panic!(
                    "fixture {:?} ({:?}) depends on {:?} ({:?}): \
                     a fixture can only depend on same or wider scope",
                    def.name, def.scope, dep_name, dep.scope
                );
            }
        }
    }
    // Cycle detection (DFS).
    for &name in registry.keys() {
        let mut visiting = HashSet::new();
        detect_cycle(name, registry, &mut visiting, &mut HashSet::new());
    }
}

fn detect_cycle(
    name: &str,
    registry: &HashMap<&str, &FixtureDef>,
    visiting: &mut HashSet<String>,
    visited: &mut HashSet<String>,
) {
    if visited.contains(name) {
        return;
    }
    if !visiting.insert(name.to_string()) {
        panic!("fixture dependency cycle detected involving {name:?}");
    }
    if let Some(def) = registry.get(name) {
        for &dep in def.deps {
            detect_cycle(dep, registry, visiting, visited);
        }
    }
    visiting.remove(name);
    visited.insert(name.to_string());
}

// Process fixture storage =====================================================================

struct ProcessEntry {
    #[allow(dead_code)] // retained for debug inspection
    name: &'static str,
    ptr: *mut (dyn Any + Send + Sync),
}

// SAFETY: the stored dyn Any is Send+Sync (enforced by the setup return type).
unsafe impl Send for ProcessEntry {}
unsafe impl Sync for ProcessEntry {}

struct ProcessFixtureStore {
    index: HashMap<&'static str, usize>,
    entries: Vec<ProcessEntry>,
}

static PROCESS_FIXTURES: OnceLock<Mutex<ProcessFixtureStore>> = OnceLock::new();

fn process_store() -> &'static Mutex<ProcessFixtureStore> {
    PROCESS_FIXTURES.get_or_init(|| {
        Mutex::new(ProcessFixtureStore {
            index: HashMap::new(),
            entries: Vec::new(),
        })
    })
}

// Test fixture storage (thread-local) =========================================================

struct TestEntry {
    name: &'static str,
    ptr: *mut (dyn Any + Send + Sync),
}

thread_local! {
    static TEST_FIXTURES: RefCell<Vec<TestEntry>> = const { RefCell::new(Vec::new()) };
}

// Test scope ==================================================================================

/// RAII guard for a test scope. On drop: reclaims per-test fixtures (LIFO) and
/// clears the current test context.
pub struct TestScope {
    _private: (),
}

impl Drop for TestScope {
    fn drop(&mut self) {
        TEST_FIXTURES.with(|cell| {
            let mut entries = cell.borrow_mut();
            while let Some(entry) = entries.pop() {
                // SAFETY: we leaked this Box in get_or_create_test, and no
                // references to it outlive this scope (handles are dropped before
                // the scope guard).
                unsafe {
                    let _ = Box::from_raw(entry.ptr);
                }
            }
        });
        crate::CURRENT_TEST.set(None);
    }
}

/// Enter a test scope. Returns a guard that cleans up per-test fixtures on drop.
///
/// # Panics
///
/// If a test scope is already active on this thread (no nesting).
pub fn enter_test_scope(name: &'static str, module_path: &'static str) -> TestScope {
    debug_assert!(
        crate::CURRENT_TEST.get().is_none(),
        "nested test scopes are not supported"
    );
    crate::CURRENT_TEST.set(Some(crate::CurrentTest { name, module_path }));
    TestScope { _private: () }
}

// Public API ==================================================================================

/// Eagerly initialize a process-scoped fixture by name.
///
/// Call this from `main()` before running tests to pre-build expensive resources
/// (e.g. Docker images). Does nothing if the fixture is already initialized or
/// if the fixture is not process-scoped.
///
/// # Panics
///
/// - No fixture registered with `name`.
/// - Fixture setup returns `Err`.
pub fn warm_up(name: &str) {
    let registry = fixture_registry();
    let def = registry
        .get(name)
        .unwrap_or_else(|| panic!("no fixture registered with name {name:?}"));
    if def.scope == FixtureScope::Process {
        ensure_process_fixture(def);
    }
}

/// Look up a fixture by name and return a handle.
///
/// The handle's lifetime depends on the fixture's scope:
/// - **Variable**: handle owns the value; dropping it drops the fixture.
/// - **Test**: value cached in the test scope; handle is a borrowed reference.
/// - **Process**: value cached globally; handle is a borrowed reference.
///
/// # Panics
///
/// - No test scope is active on this thread (Variable/Test scope only).
/// - No fixture registered with `name`.
/// - The fixture's `cast` function does not support `target`.
/// - Fixture setup returns `Err`.
pub fn fixture_get(name: &str, target: TypeId) -> FixtureHandle {
    // Process fixtures don't require a test scope (they're global singletons).
    // Variable and Test fixtures do.
    let registry = fixture_registry();
    let def = registry
        .get(name)
        .unwrap_or_else(|| panic!("no fixture registered with name {name:?}"));
    if def.scope != FixtureScope::Process {
        assert!(
            crate::CURRENT_TEST.get().is_some(),
            "skuld::fixture_get({name:?}) called outside a test scope"
        );
    }

    match def.scope {
        FixtureScope::Variable => get_variable(def, target),
        FixtureScope::Test => get_or_create_test(def, target),
        FixtureScope::Process => get_or_create_process(def, target),
    }
}

/// Convenience: look up a Test/Process-scoped fixture by name and return `&T`.
///
/// For Variable-scoped fixtures, use [`fixture_get`] instead (you need to hold
/// the handle to keep the value alive).
///
/// # Panics
///
/// Same as [`fixture_get`], plus panics if the fixture is Variable-scoped.
pub fn fixture<T: Any + Send + Sync>(name: &str) -> &T {
    assert!(
        crate::CURRENT_TEST.get().is_some(),
        "skuld::fixture({name:?}) called outside a test scope"
    );

    let registry = fixture_registry();
    let def = registry
        .get(name)
        .unwrap_or_else(|| panic!("no fixture registered with name {name:?}"));
    assert!(
        def.scope != FixtureScope::Variable,
        "skuld::fixture({name:?}): use fixture_get() for Variable-scoped fixtures"
    );

    // For cached scopes, the value is leaked (Box::into_raw) and lives until
    // cleanup, so we can return &T directly without going through FixtureHandle.
    let any_ref: &(dyn Any + Send + Sync) = match def.scope {
        FixtureScope::Test => ensure_test_fixture(def),
        FixtureScope::Process => ensure_process_fixture(def),
        FixtureScope::Variable => unreachable!(),
    };

    let fixture_ref = (def.cast)(any_ref, TypeId::of::<T>())
        .unwrap_or_else(|| panic!("fixture {:?} ({}) cannot provide target type", def.name, def.type_name));
    // SAFETY: the FixtureRef was produced from a leaked Box that lives in the
    // cache. The reference outlives any test body or handle.
    unsafe { fixture_ref.cast::<T>() }
}

/// Ensure a test-scoped fixture exists and return a reference to it.
fn ensure_test_fixture(def: &FixtureDef) -> &(dyn Any + Send + Sync) {
    let existing_ptr = TEST_FIXTURES.with(|cell| {
        let entries = cell.borrow();
        entries
            .iter()
            .find(|e| e.name == def.name)
            .map(|e| e.ptr as *const (dyn Any + Send + Sync))
    });

    if let Some(ptr) = existing_ptr {
        return unsafe { &*ptr };
    }

    let boxed = (def.setup)().unwrap_or_else(|e| panic!("fixture {:?} setup failed: {e}", def.name));
    let raw = Box::into_raw(boxed);

    TEST_FIXTURES.with(|cell| {
        cell.borrow_mut().push(TestEntry {
            name: def.name,
            ptr: raw,
        });
    });

    unsafe { &*raw }
}

/// Ensure a process-scoped fixture exists and return a reference to it.
fn ensure_process_fixture(def: &FixtureDef) -> &(dyn Any + Send + Sync) {
    let store_mutex = process_store();

    // Fast path.
    {
        let store = store_mutex.lock().unwrap();
        if let Some(&idx) = store.index.get(def.name) {
            let ptr = store.entries[idx].ptr as *const (dyn Any + Send + Sync);
            return unsafe { &*ptr };
        }
    }

    let boxed = (def.setup)().unwrap_or_else(|e| panic!("fixture {:?} setup failed: {e}", def.name));
    let raw = Box::into_raw(boxed);

    let mut store = store_mutex.lock().unwrap();
    if let Some(&idx) = store.index.get(def.name) {
        unsafe {
            let _ = Box::from_raw(raw);
        }
        let ptr = store.entries[idx].ptr as *const (dyn Any + Send + Sync);
        return unsafe { &*ptr };
    }

    let idx = store.entries.len();
    store.entries.push(ProcessEntry {
        name: def.name,
        ptr: raw,
    });
    store.index.insert(def.name, idx);

    unsafe { &*raw }
}

fn get_variable(def: &FixtureDef, target: TypeId) -> FixtureHandle {
    let boxed = (def.setup)().unwrap_or_else(|e| panic!("fixture {:?} setup failed: {e}", def.name));
    let fixture_ref = (def.cast)(&*boxed, target)
        .unwrap_or_else(|| panic!("fixture {:?} ({}) cannot provide target type", def.name, def.type_name));
    FixtureHandle {
        _owner: Some(boxed),
        fixture_ref,
    }
}

fn get_or_create_test(def: &FixtureDef, target: TypeId) -> FixtureHandle {
    let any_ref = ensure_test_fixture(def);
    let fixture_ref = (def.cast)(any_ref, target)
        .unwrap_or_else(|| panic!("fixture {:?} ({}) cannot provide target type", def.name, def.type_name));
    FixtureHandle {
        _owner: None,
        fixture_ref,
    }
}

fn get_or_create_process(def: &FixtureDef, target: TypeId) -> FixtureHandle {
    let any_ref = ensure_process_fixture(def);
    let fixture_ref = (def.cast)(any_ref, target)
        .unwrap_or_else(|| panic!("fixture {:?} ({}) cannot provide target type", def.name, def.type_name));
    FixtureHandle {
        _owner: None,
        fixture_ref,
    }
}

// Cleanup =====================================================================================

/// Reclaim all process-scoped fixtures in reverse creation order (LIFO).
///
/// Called from [`TestRunner::run_tests`](crate::TestRunner::run_tests) after
/// all tests have finished.
pub fn cleanup_process_fixtures() {
    if let Some(mutex) = PROCESS_FIXTURES.get() {
        let mut store = mutex.lock().unwrap();
        while let Some(entry) = store.entries.pop() {
            // SAFETY: we leaked this Box in get_or_create_process. All test
            // handles have been dropped, so no references remain.
            unsafe {
                let _ = Box::from_raw(entry.ptr);
            }
        }
        store.index.clear();
    }
}

// Requirement collection ======================================================================

/// Transitively collect all [`RequireFn`]s from a set of fixture names and
/// their dependencies. Used for precondition checks before running tests.
pub fn collect_fixture_requires(names: &[&str]) -> Vec<RequireFn> {
    let registry = fixture_registry();
    let mut result = Vec::new();
    let mut visited = HashSet::new();
    for &name in names {
        collect_requires_recursive(name, registry, &mut result, &mut visited);
    }
    result
}

fn collect_requires_recursive(
    name: &str,
    registry: &HashMap<&str, &FixtureDef>,
    result: &mut Vec<RequireFn>,
    visited: &mut HashSet<String>,
) {
    if !visited.insert(name.to_string()) {
        return;
    }
    if let Some(def) = registry.get(name) {
        result.extend_from_slice(def.requires);
        for &dep in def.deps {
            collect_requires_recursive(dep, registry, result, visited);
        }
    }
}
