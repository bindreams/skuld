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

/// Names that the filter grammar parses as boolean literals (`Expr::Const`)
/// rather than as label terminals. Single source of truth for both
/// [`validate_label_name`] and the pest grammar (`bool_lit` rule).
pub(crate) const RESERVED_LABEL_NAMES: &[&str] = &["true", "false"];

const fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Validate that a label name follows Rust identifier rules (ASCII subset)
/// and is not a reserved name (`true`, `false`).
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
    let mut j = 0;
    while j < RESERVED_LABEL_NAMES.len() {
        if bytes_eq(bytes, RESERVED_LABEL_NAMES[j].as_bytes()) {
            panic!("invalid label name: \"true\" and \"false\" are reserved by the filter grammar");
        }
        j += 1;
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

use boolean_expression::{Expr, BDD};
use parser::Rule;

/// A boolean expression over test labels, in canonical form.
///
/// Type alias for `boolean_expression::Expr<String>`. Wherever a `LabelExpr`
/// crosses a public-ish API boundary (e.g. inside [`LabelFilter`]) it is the
/// output of [`canonicalize`] — BDD-simplified with alphabetical variable
/// ordering, then sort-normalized so structural equality coincides with
/// semantic equivalence.
pub(crate) type LabelExpr = Expr<String>;

// Canonicalization =====

/// Collect the names of every `Terminal` in `expr`. Constants contribute none.
fn collect_terminals(expr: &LabelExpr) -> Vec<&str> {
    fn walk<'a>(e: &'a LabelExpr, out: &mut Vec<&'a str>) {
        match e {
            Expr::Terminal(t) => out.push(t.as_str()),
            Expr::Const(_) => {}
            Expr::Not(x) => walk(x, out),
            Expr::And(a, b) | Expr::Or(a, b) => {
                walk(a, out);
                walk(b, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(expr, &mut out);
    out
}

/// Reduce an arbitrary `Expr<String>` to its canonical form:
/// 1. Collect terminals and seed a `BDD` with them in alphabetical order so the
///    resulting ROBDD is canonical given the input function.
/// 2. Round-trip through `BDD::to_expr` to drop unused variables and produce a
///    deterministic SOP-like `Expr<String>`.
/// 3. Apply [`sort_children`] so any residual ordering jitter from the BDD's
///    cubelist insertion order is eliminated.
///
/// The closure passed to `evaluate_with` is never called when there are no
/// terminals: pure-constant inputs reduce via the crate's `Expr::Const` arms.
pub(crate) fn canonicalize(expr: LabelExpr) -> LabelExpr {
    let mut terms: Vec<&str> = collect_terminals(&expr);
    terms.sort_unstable();
    terms.dedup();
    if terms.is_empty() {
        return Expr::Const(expr.evaluate_with(|_: &String| false));
    }
    let mut bdd: BDD<String> = BDD::new();
    for t in &terms {
        bdd.terminal(t.to_string());
    }
    let func = bdd.from_expr(&expr);
    sort_children(bdd.to_expr(func))
}

/// Recursively put each `Or`/`And` node's children in lexicographic order of
/// their `Display` rendering. Applied after `BDD::to_expr` as a defensive
/// canonicalization step — see "Why canonicity holds" in the design plan.
fn sort_children(expr: LabelExpr) -> LabelExpr {
    match expr {
        Expr::Not(x) => Expr::Not(Box::new(sort_children(*x))),
        Expr::And(a, b) => {
            let (lo, hi) = sort_pair(sort_children(*a), sort_children(*b));
            Expr::And(Box::new(lo), Box::new(hi))
        }
        Expr::Or(a, b) => {
            let (lo, hi) = sort_pair(sort_children(*a), sort_children(*b));
            Expr::Or(Box::new(lo), Box::new(hi))
        }
        leaf @ (Expr::Terminal(_) | Expr::Const(_)) => leaf,
    }
}

fn sort_pair(a: LabelExpr, b: LabelExpr) -> (LabelExpr, LabelExpr) {
    if format_expr(&a) <= format_expr(&b) {
        (a, b)
    } else {
        (b, a)
    }
}

// Display, evaluation, SQL =====

/// Evaluate `expr` against a set of test labels. A label is "present" iff its
/// name matches a `Terminal` in `expr`; absent labels evaluate to `false`,
/// preserving the original `!docker` ⇒ true on empty-labels semantics.
pub(crate) fn matches_expr(expr: &LabelExpr, labels: &[Label]) -> bool {
    expr.evaluate_with(|t: &String| labels.iter().any(|l| l.name() == t))
}

/// Compile `expr` to a SQL WHERE fragment for correlated subqueries on the
/// `running` table (alias `r`) and `labels` table.
pub(crate) fn to_sql_expr(expr: &LabelExpr) -> String {
    match expr {
        Expr::Terminal(name) => {
            format!("EXISTS (SELECT 1 FROM labels WHERE running_id = r.id AND label = '{name}')")
        }
        Expr::Const(true) => "1=1".to_string(),
        Expr::Const(false) => "1=0".to_string(),
        Expr::Not(inner) => format!("NOT ({})", to_sql_expr(inner)),
        Expr::And(a, b) => format!("({} AND {})", to_sql_expr(a), to_sql_expr(b)),
        Expr::Or(a, b) => format!("({} OR {})", to_sql_expr(a), to_sql_expr(b)),
    }
}

/// Write `expr` in human-readable form, adding parentheses only where required
/// by operator precedence (OR=0, AND=1, NOT/leaf=2).
fn fmt_expr(expr: &LabelExpr, f: &mut std::fmt::Formatter<'_>, min_prec: u8) -> std::fmt::Result {
    let my_prec = match expr {
        Expr::Or(_, _) => 0,
        Expr::And(_, _) => 1,
        Expr::Not(_) | Expr::Terminal(_) | Expr::Const(_) => 2,
    };
    let needs_parens = my_prec < min_prec;
    if needs_parens {
        write!(f, "(")?;
    }
    match expr {
        Expr::Terminal(name) => write!(f, "{name}")?,
        Expr::Const(true) => write!(f, "true")?,
        Expr::Const(false) => write!(f, "false")?,
        Expr::Not(inner) => {
            write!(f, "!")?;
            fmt_expr(inner, f, 2)?;
        }
        Expr::And(a, b) => {
            fmt_expr(a, f, 1)?;
            write!(f, " & ")?;
            fmt_expr(b, f, 1)?;
        }
        Expr::Or(a, b) => {
            fmt_expr(a, f, 0)?;
            write!(f, " | ")?;
            fmt_expr(b, f, 0)?;
        }
    }
    if needs_parens {
        write!(f, ")")?;
    }
    Ok(())
}

fn format_expr(expr: &LabelExpr) -> String {
    struct D<'a>(&'a LabelExpr);
    impl std::fmt::Display for D<'_> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            fmt_expr(self.0, f, 0)
        }
    }
    D(expr).to_string()
}

// Parsing =====

/// Parse a label filter expression string into a CANONICAL `LabelExpr`.
///
/// The returned expression has been BDD-simplified and sort-normalized; two
/// semantically-equivalent inputs produce structurally identical outputs.
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

    let raw = build_expr(expr_pair)?;
    Ok(canonicalize(raw))
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
                left = Expr::Or(Box::new(left), Box::new(build_expr(right_pair)?));
            }
            Ok(left)
        }
        Rule::and_expr => {
            // and_expr = { not_expr ~ ("&" ~ not_expr)* }
            let mut inner = pair.into_inner();
            let mut left = build_expr(inner.next().unwrap())?;
            for right_pair in inner {
                left = Expr::And(Box::new(left), Box::new(build_expr(right_pair)?));
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
            Ok(Expr::Not(Box::new(build_expr(inner)?)))
        }
        Rule::primary => {
            // primary = { "(" ~ expr ~ ")" | bool_lit | label }
            let child = pair.into_inner().next().unwrap();
            build_expr(child)
        }
        Rule::bool_lit => Ok(Expr::Const(pair.as_str().eq_ignore_ascii_case("true"))),
        Rule::label => Ok(Expr::Terminal(pair.as_str().to_ascii_lowercase())),
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
/// with `&` (AND), `|` (OR), and `!` (NOT) operators. The grammar also
/// accepts the literal constants `true` and `false` (the names `"true"` and
/// `"false"` are reserved and may not be used as label names).
///
/// Filters are stored in a canonical form: two semantically-equivalent
/// expressions compare equal under `==`, dedup automatically when merged,
/// and round-trip through `Display`/`parse`. For example,
/// `LabelFilter::parse("a & b") == LabelFilter::parse("b & a")`, and the
/// merged form of `(a) | (a) | (b)` displays as `a | b`.
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LabelFilter {
    pub(crate) expr: LabelExpr,
}

impl LabelFilter {
    /// Parse a label filter expression from a string. The result is in
    /// canonical form (BDD-simplified, sort-normalized).
    pub fn parse(input: &str) -> Result<Self, String> {
        parse_label_expr(input).map(|expr| Self { expr })
    }

    /// Evaluate the filter against a set of test labels.
    pub fn matches(&self, labels: &[Label]) -> bool {
        matches_expr(&self.expr, labels)
    }

    /// Compile the filter to a SQL WHERE fragment for correlated subqueries.
    ///
    /// The result is a SQL expression suitable for:
    /// ```sql
    /// SELECT EXISTS (SELECT 1 FROM running r WHERE <result>)
    /// ```
    pub fn to_sql(&self) -> String {
        to_sql_expr(&self.expr)
    }

    /// Whether the canonical form of this filter is `Const(true)` — i.e. the
    /// filter is satisfied by every possible label set. Used by
    /// `coordination::to_storage` to collapse tautologies into the `*` sentinel.
    pub(crate) fn is_tautology(&self) -> bool {
        matches!(self.expr, Expr::Const(true))
    }

    /// Whether the canonical form of this filter is `Const(false)` — i.e. no
    /// label set satisfies it. Used by `coordination::to_storage` to collapse
    /// contradictions into the `""` (non-serial) sentinel.
    pub(crate) fn is_contradiction(&self) -> bool {
        matches!(self.expr, Expr::Const(false))
    }
}

impl std::fmt::Display for LabelFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt_expr(&self.expr, f, 0)
    }
}

impl From<Label> for LabelFilter {
    fn from(label: Label) -> Self {
        // A single terminal is already canonical (no simplification possible
        // and no children to sort). `validate_label_name` rejects "true" /
        // "false" so this Terminal can never alias a `Const`.
        Self {
            expr: Expr::Terminal(label.name().to_string()),
        }
    }
}

// Compile-time assertion that LabelFilter remains thread-safe across crate
// upgrades. The runtime carries filters across thread boundaries (test
// runner, coordination DB queries) and must not silently lose Send/Sync.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<LabelFilter>();
};

// Operator overloads for LabelFilter composition -----
//
// Each operator builds a non-canonical Expr from the inputs and runs it
// through `canonicalize` once. For typical chained usage (≤10 operators) the
// total cost is negligible and avoids the alternative of deferring
// canonicalization to a separate "finalize" step.

impl std::ops::Not for LabelFilter {
    type Output = LabelFilter;
    fn not(self) -> LabelFilter {
        LabelFilter {
            expr: canonicalize(Expr::Not(Box::new(self.expr))),
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
                    expr: canonicalize(Expr::$variant(Box::new(self.expr), Box::new(rhs.expr))),
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
