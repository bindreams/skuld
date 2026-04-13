//! Label types, filtering, validation, and module-level defaults.
//!
//! Labels are sentinel values created with [`new_label!`] and optionally
//! aliased with [`get_label!`]. Tests are filtered by the `SKULD_LABELS`
//! environment variable (comma-separated, include-only, union semantics).

#[cfg(test)]
mod label_tests;

use crate::TestDef;

// Label type =====

/// A test label. Created via [`new_label!`] (definition) or [`get_label!`] (reference).
///
/// ```ignore
/// skuld::new_label!(pub DOCKER, "docker");
/// skuld::get_label!(pub ALSO_DOCKER, "docker");
///
/// #[skuld::test(labels = [DOCKER])]
/// fn my_test() {}
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Label {
    name: &'static str,
}

impl Label {
    #[doc(hidden)]
    pub const fn __new(name: &'static str) -> Self {
        Self { name }
    }

    /// The string name of this label.
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

// Label registration =====

/// Whether a label entry is a definition ([`new_label!`]) or a reference ([`get_label!`]).
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelEntryKind {
    New,
    Get,
}

/// A registration entry submitted by [`new_label!`] or [`get_label!`] via `inventory`.
#[doc(hidden)]
pub struct LabelEntry {
    pub name: &'static str,
    pub kind: LabelEntryKind,
    pub file: &'static str,
    pub line: u32,
    pub column: u32,
}

inventory::collect!(LabelEntry);

/// Define a new label constant. Panics at startup if another `new_label!` with
/// the same name exists anywhere in the binary.
///
/// ```ignore
/// skuld::new_label!(pub DOCKER, "docker");
/// skuld::new_label!(INTERNAL_LABEL, "internal"); // private
/// ```
#[macro_export]
macro_rules! new_label {
    ($vis:vis $ident:ident, $name:literal) => {
        $vis const $ident: $crate::Label = $crate::Label::__new($name);

        $crate::inventory::submit!($crate::LabelEntry {
            name: $name,
            kind: $crate::LabelEntryKind::New,
            file: ::core::file!(),
            line: ::core::line!(),
            column: ::core::column!(),
        });
    };
}

/// Reference an existing label (defined elsewhere with [`new_label!`]).
/// Panics at startup if no corresponding `new_label!` exists in the binary.
///
/// ```ignore
/// skuld::get_label!(pub DOCKER, "docker"); // must have a new_label!("docker") somewhere
/// ```
#[macro_export]
macro_rules! get_label {
    ($vis:vis $ident:ident, $name:literal) => {
        $vis const $ident: $crate::Label = $crate::Label::__new($name);

        $crate::inventory::submit!($crate::LabelEntry {
            name: $name,
            kind: $crate::LabelEntryKind::Get,
            file: ::core::file!(),
            line: ::core::line!(),
            column: ::core::column!(),
        });
    };
}

// Label validation =====

/// Validate all label registrations. Called at the start of
/// [`TestRunner::run_tests()`](crate::runner::TestRunner::run_tests).
///
/// Panics if:
/// - Two `new_label!` entries share the same name.
/// - A `get_label!` entry has no corresponding `new_label!`.
pub(crate) fn validate_labels() {
    if let Err(msg) = check_label_registry() {
        panic!("{msg}");
    }
}

/// Inner validation that returns all error messages instead of panicking,
/// so unit tests can assert on specific failures.
///
/// Collects every error (duplicate definitions, orphan references) and
/// returns them joined, so that a single run surfaces all problems.
pub(crate) fn check_label_registry() -> Result<(), String> {
    use std::collections::HashMap;

    let mut definitions: HashMap<&str, Vec<&LabelEntry>> = HashMap::new();
    let mut references: Vec<&LabelEntry> = Vec::new();

    for entry in inventory::iter::<LabelEntry> {
        match entry.kind {
            LabelEntryKind::New => {
                definitions.entry(entry.name).or_default().push(entry);
            }
            LabelEntryKind::Get => {
                references.push(entry);
            }
        }
    }

    let mut errors: Vec<String> = Vec::new();

    let mut sorted_names: Vec<&&str> = definitions.keys().collect();
    sorted_names.sort();
    for name in sorted_names {
        let defs = &definitions[name];
        if defs.len() > 1 {
            let locations: Vec<String> = defs
                .iter()
                .map(|e| format!("  {}:{}:{}", e.file, e.line, e.column))
                .collect();
            errors.push(format!(
                "label {:?} defined multiple times with new_label!:\n{}",
                name,
                locations.join("\n")
            ));
        }
    }

    for entry in &references {
        if !definitions.contains_key(entry.name) {
            errors.push(format!(
                "get_label!({:?}) at {}:{}:{} has no corresponding new_label! definition",
                entry.name, entry.file, entry.line, entry.column
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("skuld: label validation failed:\n{}", errors.join("\n")))
    }
}

// Label filtering =====

/// Read label filter from the `SKULD_LABELS` environment variable.
///
/// - Unset → `None` (no filtering, all tests run)
/// - `""` → `Some(vec![])` (empty list, no tests match any label)
/// - `"docker,slow"` → `Some(vec!["docker", "slow"])` (include-only, union)
pub(crate) fn read_label_filter() -> Option<Vec<String>> {
    parse_label_filter(std::env::var("SKULD_LABELS").ok())
}

/// Parse an optional label filter value into a filter list.
///
/// Pure function extracted from [`read_label_filter`] for testability.
pub(crate) fn parse_label_filter(val: Option<String>) -> Option<Vec<String>> {
    val.map(|v| {
        v.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// Check whether a test with the given labels passes the label filter.
///
/// - `None` → all tests pass (no filtering active).
/// - `Some(&[])` → no tests pass (empty filter).
/// - `Some(&[..])` → test must have at least one label whose name is in the list.
pub(crate) fn label_matches(test_labels: &[Label], filter: Option<&[String]>) -> bool {
    match filter {
        None => true,
        Some(allowed) => test_labels.iter().any(|l| allowed.iter().any(|a| a == l.name())),
    }
}

// Module-level default labels =====

/// Default labels for all tests in a module. Registered by [`default_labels!`].
pub struct ModuleLabels {
    pub module: &'static str,
    pub labels: &'static [Label],
}

inventory::collect!(ModuleLabels);

/// Set default labels for all `#[skuld::test]` functions in the current module.
///
/// Tests that explicitly specify `labels = [...]` (including `labels = []`) are
/// not affected — explicit labels fully replace defaults.
///
/// ```ignore
/// skuld::new_label!(pub DOCKER, "docker");
/// skuld::new_label!(pub CONFORMANCE, "conformance");
/// skuld::default_labels!(DOCKER, CONFORMANCE);
///
/// #[skuld::test]                       // inherits [DOCKER, CONFORMANCE]
/// fn test_a() { ... }
///
/// #[skuld::test(labels = [DOCKER])]    // gets [DOCKER], not both
/// fn test_b() { ... }
///
/// #[skuld::test(labels = [])]          // gets nothing — explicit opt-out
/// fn test_c() { ... }
/// ```
#[macro_export]
macro_rules! default_labels {
    ($($label:path),+ $(,)?) => {
        $crate::inventory::submit!($crate::ModuleLabels {
            module: ::core::module_path!(),
            labels: &[$($label),+],
        });
    };
}

/// Resolve the effective labels for a test, applying module defaults if the test
/// did not explicitly specify `labels = [...]`.
pub(crate) fn resolve_labels(def: &TestDef, module_defaults: &[&ModuleLabels]) -> Vec<Label> {
    if def.labels_explicit {
        return def.labels.to_vec();
    }
    // Find the longest module prefix match.
    let default = module_defaults
        .iter()
        .filter(|m| def.module.starts_with(m.module))
        .max_by_key(|m| m.module.len());
    match default {
        Some(m) => m.labels.to_vec(),
        None => def.labels.to_vec(),
    }
}
