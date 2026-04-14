//! Label types, filtering, validation, and module-level defaults.
//!
//! Labels are sentinel values created with the [`macro@crate::label`] attribute
//! macro. Tests are filtered by the `SKULD_LABELS` environment variable
//! (boolean expression with `&`, `|`, `!`, and grouping, matched
//! case-insensitively).

#[cfg(test)]
mod label_tests;

use crate::TestDef;

// Label type =====

/// A test label. Created via the [`macro@crate::label`] attribute macro.
///
/// ```ignore
/// #[skuld::label]
/// pub const DOCKER: skuld::Label;
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
        validate_label_name(name);
        // Canonical-lowercase invariant is a hard contract, not a sanity
        // check: the `#[skuld::label]` macro lowercases at expansion time,
        // and label matching everywhere else (SQL, hash buckets, filter
        // parsing) assumes stored names are already lowercase. A stray
        // uppercase name silently matches nothing — fail loud in release
        // builds too.
        assert!(
            contains_no_ascii_uppercase(name),
            "Label::__new requires a canonical-lowercase name"
        );
        Self { name }
    }

    /// The string name of this label.
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

/// Stdlib `str::bytes().all(|b| !b.is_ascii_uppercase())` is not const-stable;
/// hand-rolled here so [`Label::__new`] can be a `const fn`.
const fn contains_no_ascii_uppercase(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] >= b'A' && bytes[i] <= b'Z' {
            return false;
        }
        i += 1;
    }
    true
}

impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

// Label registration =====

/// A registration entry submitted by the [`macro@crate::label`] attribute
/// macro via `inventory`. Used at startup to detect duplicate labels.
#[doc(hidden)]
pub struct LabelEntry {
    pub name: &'static str,
    pub file: &'static str,
    pub line: u32,
    pub column: u32,
}

inventory::collect!(LabelEntry);

// Label validation =====

/// Validate that a label name follows Rust identifier rules (ASCII subset).
///
/// Must start with `[a-zA-Z_]`, followed by `[a-zA-Z0-9_]`.
/// Panics with a descriptive message. When called from a const context
/// (via [`Label::__new`] inside the attribute macro), this becomes a
/// compile-time error.
pub(crate) const fn validate_label_name(name: &str) {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        panic!("invalid label name: must not be empty");
    }
    let first = bytes[0];
    if !((first >= b'a' && first <= b'z') || (first >= b'A' && first <= b'Z') || first == b'_') {
        panic!("invalid label name: must start with an ASCII letter or underscore");
    }
    let mut i = 1;
    while i < bytes.len() {
        let b = bytes[i];
        if !((b >= b'a' && b <= b'z') || (b >= b'A' && b <= b'Z') || (b >= b'0' && b <= b'9') || b == b'_') {
            panic!("invalid label name: must contain only ASCII letters, digits, and underscores");
        }
        i += 1;
    }
}

/// Validate all label registrations. Called at the start of
/// [`TestRunner::run_tests()`](crate::runner::TestRunner::run_tests).
///
/// Panics if two `#[skuld::label]` declarations (anywhere in the binary,
/// including in different crates) produce the same lowercased name.
pub(crate) fn validate_labels() {
    if let Err(msg) = check_label_registry() {
        panic!("{msg}");
    }
}

/// Validate all serial filter expressions on registered tests and fixtures.
/// Called at startup to catch malformed expressions before any test runs.
pub(crate) fn validate_serial_filters() {
    let mut errors: Vec<String> = Vec::new();

    for def in inventory::iter::<crate::TestDef> {
        if !def.serial.is_empty() && def.serial != crate::coordination::SERIAL_ALL {
            if let Err(e) = LabelFilter::parse(def.serial) {
                errors.push(format!(
                    "test {:?}: invalid serial filter {:?}: {e}",
                    def.name, def.serial
                ));
            }
        }
    }

    for def in inventory::iter::<crate::fixture::FixtureDef> {
        if !def.serial.is_empty() && def.serial != crate::coordination::SERIAL_ALL {
            if let Err(e) = LabelFilter::parse(def.serial) {
                errors.push(format!(
                    "fixture {:?}: invalid serial filter {:?}: {e}",
                    def.name, def.serial
                ));
            }
        }
    }

    if !errors.is_empty() {
        panic!("skuld: serial filter validation failed:\n{}", errors.join("\n"));
    }
}

/// Inner validation that returns the error message instead of panicking, so
/// unit tests can assert on specific failures.
///
/// Buckets all registered labels by name and returns an error for every
/// bucket with more than one entry, including every source location so a
/// single run surfaces all duplicates.
pub(crate) fn check_label_registry() -> Result<(), String> {
    use std::collections::HashMap;

    let mut definitions: HashMap<&str, Vec<&LabelEntry>> = HashMap::new();
    for entry in inventory::iter::<LabelEntry> {
        definitions.entry(entry.name).or_default().push(entry);
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
                "label {:?} declared multiple times:\n{}",
                name,
                locations.join("\n")
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

    /// Compile this expression to a SQL WHERE fragment for correlated subqueries.
    ///
    /// Assumes the `running` table is aliased as `r` and `labels` has columns
    /// `running_id` and `label`.
    pub(crate) fn to_sql(&self) -> String {
        match self {
            LabelExpr::Label(name) => {
                format!("EXISTS (SELECT 1 FROM labels WHERE running_id = r.id AND label = '{name}')")
            }
            LabelExpr::Not(inner) => format!("NOT ({})", inner.to_sql()),
            LabelExpr::And(a, b) => format!("({} AND {})", a.to_sql(), b.to_sql()),
            LabelExpr::Or(a, b) => format!("({} OR {})", a.to_sql(), b.to_sql()),
        }
    }

    /// Write the expression in human-readable form, adding parentheses only where
    /// needed by operator precedence (OR=0, AND=1, NOT/Label=2).
    fn fmt_with_prec(&self, f: &mut std::fmt::Formatter<'_>, min_prec: u8) -> std::fmt::Result {
        let my_prec = match self {
            LabelExpr::Or(_, _) => 0,
            LabelExpr::And(_, _) => 1,
            LabelExpr::Not(_) | LabelExpr::Label(_) => 2,
        };
        let needs_parens = my_prec < min_prec;
        if needs_parens {
            write!(f, "(")?;
        }
        match self {
            LabelExpr::Label(name) => write!(f, "{name}")?,
            LabelExpr::Not(inner) => {
                write!(f, "!")?;
                inner.fmt_with_prec(f, 2)?;
            }
            LabelExpr::And(a, b) => {
                a.fmt_with_prec(f, 1)?;
                write!(f, " & ")?;
                b.fmt_with_prec(f, 1)?;
            }
            LabelExpr::Or(a, b) => {
                a.fmt_with_prec(f, 0)?;
                write!(f, " | ")?;
                b.fmt_with_prec(f, 0)?;
            }
        }
        if needs_parens {
            write!(f, ")")?;
        }
        Ok(())
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
        Rule::label => Ok(LabelExpr::Label(pair.as_str().to_ascii_lowercase())),
        _ => Err(format!("unexpected rule: {:?}", pair.as_rule())),
    }
}

/// Read label filter from the `SKULD_LABELS` environment variable.
///
/// - Unset → `None` (no filtering, all tests run).
/// - `""` (empty / whitespace-only) → panics (invalid expression).
/// - Non-empty → parses as a boolean expression; panics on malformed input.
pub(crate) fn read_label_filter() -> Option<LabelFilter> {
    let val = std::env::var("SKULD_LABELS").ok()?;
    match LabelFilter::parse(&val) {
        Ok(filter) => Some(filter),
        Err(e) => panic!("skuld: SKULD_LABELS: {e}"),
    }
}

// Label filter type =====

/// A boolean expression over label names that can be matched against a set of
/// labels to determine whether a test should be selected.
///
/// The simplest filter is a single [`Label`]; complex filters are built
/// with `&` (AND), `|` (OR), and `!` (NOT) operators.
///
/// String-based filters ([`LabelFilter::parse`], `SKULD_LABELS`,
/// `#[skuld::test(serial = ...)]`) match label names case-insensitively.
///
/// ```ignore
/// #[skuld::label] pub const DOCKER: skuld::Label;
/// #[skuld::label] pub const FAST: skuld::Label;
///
/// // A single label is a filter:
/// let f: skuld::LabelFilter = DOCKER.into();
///
/// // Compose with operators:
/// let f = DOCKER & !FAST;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelFilter {
    pub(crate) expr: LabelExpr,
}

impl LabelFilter {
    /// Parse a label filter expression from a string.
    pub fn parse(input: &str) -> Result<Self, String> {
        parse_label_expr(input).map(|expr| Self { expr })
    }

    /// Evaluate the filter against a set of test labels.
    pub fn matches(&self, labels: &[Label]) -> bool {
        self.expr.matches(labels)
    }

    /// Compile the filter to a SQL WHERE fragment for correlated subqueries.
    ///
    /// The result is a SQL expression suitable for:
    /// ```sql
    /// SELECT EXISTS (SELECT 1 FROM running r WHERE <result>)
    /// ```
    pub fn to_sql(&self) -> String {
        self.expr.to_sql()
    }
}

impl std::fmt::Display for LabelFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.expr.fmt_with_prec(f, 0)
    }
}

impl From<Label> for LabelFilter {
    fn from(label: Label) -> Self {
        Self {
            expr: LabelExpr::Label(label.name().to_string()),
        }
    }
}

// Operator overloads for LabelFilter composition -----

impl std::ops::Not for LabelFilter {
    type Output = LabelFilter;
    fn not(self) -> LabelFilter {
        LabelFilter {
            expr: LabelExpr::Not(Box::new(self.expr)),
        }
    }
}

impl std::ops::Not for Label {
    type Output = LabelFilter;
    fn not(self) -> LabelFilter {
        !LabelFilter::from(self)
    }
}

macro_rules! impl_filter_binop {
    ($trait:ident, $method:ident, $variant:ident) => {
        impl std::ops::$trait for LabelFilter {
            type Output = LabelFilter;
            fn $method(self, rhs: LabelFilter) -> LabelFilter {
                LabelFilter {
                    expr: LabelExpr::$variant(Box::new(self.expr), Box::new(rhs.expr)),
                }
            }
        }

        impl std::ops::$trait<LabelFilter> for Label {
            type Output = LabelFilter;
            fn $method(self, rhs: LabelFilter) -> LabelFilter {
                LabelFilter::from(self).$method(rhs)
            }
        }

        impl std::ops::$trait<Label> for LabelFilter {
            type Output = LabelFilter;
            fn $method(self, rhs: Label) -> LabelFilter {
                self.$method(LabelFilter::from(rhs))
            }
        }

        impl std::ops::$trait for Label {
            type Output = LabelFilter;
            fn $method(self, rhs: Label) -> LabelFilter {
                LabelFilter::from(self).$method(LabelFilter::from(rhs))
            }
        }
    };
}

impl_filter_binop!(BitAnd, bitand, And);
impl_filter_binop!(BitOr, bitor, Or);

// Module-level default labels =====

/// Default labels for all tests in a module. Registered by [`crate::default_labels!`].
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
/// #[skuld::label] pub const DOCKER: skuld::Label;
/// #[skuld::label] pub const CONFORMANCE: skuld::Label;
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
