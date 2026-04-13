//! Label types, filtering, validation, and module-level defaults.
//!
//! Labels are sentinel values created with [`new_label!`] and optionally
//! aliased with [`get_label!`]. Tests are filtered by the `SKULD_LABELS`
//! environment variable (boolean expression with `&`, `|`, `!`, and grouping).

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

    // Validate that label names are filterable by the SKULD_LABELS expression grammar.
    // The grammar's `label` rule accepts [A-Za-z0-9_-]+ only.
    for entry in inventory::iter::<LabelEntry> {
        if entry.kind != LabelEntryKind::New {
            continue;
        }
        if entry.name.is_empty()
            || !entry
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            errors.push(format!(
                "label {:?} at {}:{}:{} contains characters not supported by SKULD_LABELS filtering \
                 (only ASCII alphanumeric, '_', and '-' are allowed)",
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

mod parser {
    use pest_derive::Parser;

    #[derive(Parser)]
    #[grammar = "label.pest"]
    pub(super) struct LabelExprParser;
}

use parser::Rule;

/// A boolean expression over test labels.
///
/// Parsed from the `SKULD_LABELS` environment variable by [`parse_label_expr`].
/// Supports `&` (AND), `|` (OR), `!` (NOT), and parenthesized grouping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LabelExpr {
    /// Matches if the test has this label.
    Label(String),
    /// Logical NOT: matches if the inner expression does not match.
    Not(Box<LabelExpr>),
    /// Logical AND: matches if both sides match.
    And(Box<LabelExpr>, Box<LabelExpr>),
    /// Logical OR: matches if either side matches.
    Or(Box<LabelExpr>, Box<LabelExpr>),
}

impl LabelExpr {
    /// Evaluate this expression against a set of test labels.
    pub(crate) fn matches(&self, test_labels: &[Label]) -> bool {
        match self {
            LabelExpr::Label(name) => test_labels.iter().any(|l| l.name() == name),
            LabelExpr::Not(inner) => !inner.matches(test_labels),
            LabelExpr::And(left, right) => left.matches(test_labels) && right.matches(test_labels),
            LabelExpr::Or(left, right) => left.matches(test_labels) || right.matches(test_labels),
        }
    }
}

/// Parse a label filter expression string into an AST.
///
/// Returns `Err` with a human-readable message on malformed input.
pub(crate) fn parse_label_expr(input: &str) -> Result<LabelExpr, String> {
    use pest::Parser;

    let pairs =
        parser::LabelExprParser::parse(Rule::input, input).map_err(|e| format!("invalid label expression: {e}"))?;

    let input_pair = pairs
        .into_iter()
        .next()
        .expect("pest grammar guarantees an input rule on successful parse");
    // input = { SOI ~ expr ~ EOI } — skip SOI, take expr, skip EOI.
    let expr_pair = input_pair
        .into_inner()
        .find(|p| p.as_rule() == Rule::expr)
        .expect("pest grammar guarantees input rule contains expr");

    build_expr(expr_pair)
}

fn build_expr(pair: pest::iterators::Pair<'_, Rule>) -> Result<LabelExpr, String> {
    match pair.as_rule() {
        Rule::expr => {
            // expr = { or_expr }
            build_expr(pair.into_inner().next().unwrap())
        }
        Rule::or_expr => {
            // or_expr = { and_expr ~ ("|" ~ and_expr)* }
            let mut inner = pair.into_inner();
            let mut left = build_expr(inner.next().unwrap())?;
            for right_pair in inner {
                left = LabelExpr::Or(Box::new(left), Box::new(build_expr(right_pair)?));
            }
            Ok(left)
        }
        Rule::and_expr => {
            // and_expr = { not_expr ~ ("&" ~ not_expr)* }
            let mut inner = pair.into_inner();
            let mut left = build_expr(inner.next().unwrap())?;
            for right_pair in inner {
                left = LabelExpr::And(Box::new(left), Box::new(build_expr(right_pair)?));
            }
            Ok(left)
        }
        Rule::not_expr => {
            // not_expr = { neg | primary }
            build_expr(pair.into_inner().next().unwrap())
        }
        Rule::neg => {
            // neg = { "!" ~ not_expr }
            let inner = pair.into_inner().next().unwrap();
            Ok(LabelExpr::Not(Box::new(build_expr(inner)?)))
        }
        Rule::primary => {
            // primary = { "(" ~ expr ~ ")" | label }
            let child = pair.into_inner().next().unwrap();
            build_expr(child)
        }
        Rule::label => Ok(LabelExpr::Label(pair.as_str().to_string())),
        _ => Err(format!("unexpected rule: {:?}", pair.as_rule())),
    }
}

/// Read label filter from the `SKULD_LABELS` environment variable.
///
/// - Unset → `None` (no filtering, all tests run).
/// - `""` (empty / whitespace-only) → panics (invalid expression).
/// - Non-empty → parses as a boolean expression; panics on malformed input.
pub(crate) fn read_label_filter() -> Option<LabelExpr> {
    let val = std::env::var("SKULD_LABELS").ok()?;
    match parse_label_expr(&val) {
        Ok(expr) => Some(expr),
        Err(e) => panic!("skuld: SKULD_LABELS: {e}"),
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
