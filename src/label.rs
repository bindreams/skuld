//! Label filtering and module-level default labels.
//!
//! Tests can be labeled for selective execution via `--label` CLI arguments.
//! Labels support inclusion (`--label docker`) and exclusion (`--label !slow`),
//! with comma-separated values (`--label=docker,!slow`).

use crate::TestDef;

// Label selectors =====================================================================================

pub(crate) enum LabelSelector {
    Include(String),
    Exclude(String),
}

/// Extract `--label` arguments from the process args, returning the selectors
/// and the remaining args (for libtest-mimic).
///
/// Supports `--label docker`, `--label=docker,!slow`, and comma-separated values.
/// Use `!label` to exclude.
pub(crate) fn extract_label_filters() -> (Vec<LabelSelector>, Vec<String>) {
    let args: Vec<String> = std::env::args().collect();
    let mut selectors = Vec::new();
    let mut remaining = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--label" {
            i += 1;
            if i < args.len() {
                parse_label_arg(&args[i], &mut selectors);
            }
        } else if let Some(val) = args[i].strip_prefix("--label=") {
            parse_label_arg(val, &mut selectors);
        } else {
            remaining.push(args[i].clone());
        }
        i += 1;
    }

    (selectors, remaining)
}

fn parse_label_arg(val: &str, selectors: &mut Vec<LabelSelector>) {
    for part in val.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(label) = part.strip_prefix('!') {
            selectors.push(LabelSelector::Exclude(label.to_string()));
        } else {
            selectors.push(LabelSelector::Include(part.to_string()));
        }
    }
}

/// Check whether a test with the given labels passes the label filter.
///
/// - Includes form a union: test must match ANY include.
/// - Excludes subtract: test must not match ANY exclude.
/// - No includes → all tests included by default.
/// - No selectors → all tests pass.
pub(crate) fn label_matches(test_labels: &[&str], selectors: &[LabelSelector]) -> bool {
    let has_includes = selectors.iter().any(|s| matches!(s, LabelSelector::Include(_)));

    let included = if has_includes {
        selectors.iter().any(|s| match s {
            LabelSelector::Include(l) => test_labels.contains(&l.as_str()),
            LabelSelector::Exclude(_) => false,
        })
    } else {
        true
    };

    let excluded = selectors.iter().any(|s| match s {
        LabelSelector::Exclude(l) => test_labels.contains(&l.as_str()),
        LabelSelector::Include(_) => false,
    });

    included && !excluded
}

// Module-level default labels =========================================================================

/// Default labels for all tests in a module. Registered by [`default_labels!`].
pub struct ModuleLabels {
    pub module: &'static str,
    pub labels: &'static [&'static str],
}

inventory::collect!(ModuleLabels);

/// Set default labels for all `#[skuld::test]` functions in the current module.
///
/// Tests that explicitly specify `labels = [...]` (including `labels = []`) are
/// not affected — explicit labels fully replace defaults.
///
/// ```ignore
/// skuld::default_labels!(docker, conformance);
///
/// #[skuld::test]                    // inherits [docker, conformance]
/// fn test_a() { ... }
///
/// #[skuld::test(labels = [slow])]   // gets [slow], not [docker, conformance, slow]
/// fn test_b() { ... }
///
/// #[skuld::test(labels = [])]       // gets nothing — explicit opt-out
/// fn test_c() { ... }
/// ```
#[macro_export]
macro_rules! default_labels {
    ($($label:ident),+ $(,)?) => {
        $crate::inventory::submit!($crate::ModuleLabels {
            module: ::core::module_path!(),
            labels: &[$(::core::stringify!($label)),+],
        });
    };
}

/// Resolve the effective labels for a test, applying module defaults if the test
/// did not explicitly specify `labels = [...]`.
pub(crate) fn resolve_labels(def: &TestDef, module_defaults: &[&ModuleLabels]) -> Vec<String> {
    if def.labels_explicit {
        return def.labels.iter().map(|s| s.to_string()).collect();
    }
    // Find the longest module prefix match.
    let default = module_defaults
        .iter()
        .filter(|m| def.module.starts_with(m.module))
        .max_by_key(|m| m.module.len());
    match default {
        Some(m) => m.labels.iter().map(|s| s.to_string()).collect(),
        None => def.labels.iter().map(|s| s.to_string()).collect(),
    }
}
