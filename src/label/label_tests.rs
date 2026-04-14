use super::*;

// Label type =====

#[test]
fn label_equality_by_name() {
    let a = Label::__new("docker");
    let b = Label::__new("docker");
    let c = Label::__new("slow");
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn label_name_accessor() {
    let l = Label::__new("smoke");
    assert_eq!(l.name(), "smoke");
}

#[test]
fn label_display() {
    let l = Label::__new("integration");
    assert_eq!(format!("{l}"), "integration");
}

// parse_label_expr =====
//
// `parse_label_expr` returns a CANONICAL `LabelExpr` (= `Expr<String>` from
// the boolean_expression crate), so structural-equality tests against
// hand-built ASTs only work for already-canonical inputs (single terminals,
// flat AND/OR chains where alphabetical ordering already holds, etc.).
// Behavioral assertions go through `matches`, which works for any input.

fn term(name: &str) -> LabelExpr {
    LabelExpr::Terminal(name.into())
}

#[test]
fn parse_bare_label() {
    assert_eq!(parse_label_expr("unit").unwrap(), term("unit"));
}

#[test]
fn parse_or_matches_function() {
    let a = Label::__new("a");
    let b = Label::__new("b");
    let c = Label::__new("c");
    let f = parse_label_expr("a|b").unwrap();
    assert!(matches_expr(&f, &[a]));
    assert!(matches_expr(&f, &[b]));
    assert!(!matches_expr(&f, &[c]));
}

#[test]
fn parse_and_matches_function() {
    let a = Label::__new("a");
    let b = Label::__new("b");
    let f = parse_label_expr("a&b").unwrap();
    assert!(matches_expr(&f, &[a, b]));
    assert!(!matches_expr(&f, &[a]));
    assert!(!matches_expr(&f, &[b]));
}

#[test]
fn parse_not_matches_function() {
    let a = Label::__new("a");
    let b = Label::__new("b");
    let f = parse_label_expr("!a").unwrap();
    assert!(!matches_expr(&f, &[a]));
    assert!(matches_expr(&f, &[b]));
    assert!(matches_expr(&f, &[]));
}

#[test]
fn parse_precedence_not_over_and() {
    // !a&b should match exactly when a is absent and b is present.
    let a = Label::__new("a");
    let b = Label::__new("b");
    let f = parse_label_expr("!a&b").unwrap();
    assert!(matches_expr(&f, &[b]));
    assert!(!matches_expr(&f, &[a]));
    assert!(!matches_expr(&f, &[a, b]));
    assert!(!matches_expr(&f, &[]));
}

#[test]
fn parse_precedence_and_over_or() {
    // a|b&c should match a alone or b&c together, but not b alone.
    let a = Label::__new("a");
    let b = Label::__new("b");
    let c = Label::__new("c");
    let f = parse_label_expr("a|b&c").unwrap();
    assert!(matches_expr(&f, &[a]));
    assert!(matches_expr(&f, &[b, c]));
    assert!(!matches_expr(&f, &[b]));
    assert!(!matches_expr(&f, &[c]));
}

#[test]
fn parse_grouping_overrides_precedence() {
    // (a|b)&c requires c plus one of {a, b}.
    let a = Label::__new("a");
    let b = Label::__new("b");
    let c = Label::__new("c");
    let f = parse_label_expr("(a|b)&c").unwrap();
    assert!(matches_expr(&f, &[a, c]));
    assert!(matches_expr(&f, &[b, c]));
    assert!(!matches_expr(&f, &[a]));
    assert!(!matches_expr(&f, &[c]));
}

#[test]
fn parse_whitespace_ignored() {
    let a = Label::__new("a");
    let b = Label::__new("b");
    let f = parse_label_expr("  a  |  b  ").unwrap();
    assert!(matches_expr(&f, &[a]));
    assert!(matches_expr(&f, &[b]));
}

#[test]
fn parse_double_negation_canonicalizes() {
    // !!a is semantically `a`; canonicalization collapses it.
    assert_eq!(parse_label_expr("!!a").unwrap(), term("a"));
}

#[test]
fn parse_underscores_and_digits() {
    assert_eq!(parse_label_expr("my_label_2").unwrap(), term("my_label_2"));
}

#[test]
fn parse_rejects_numeric_label() {
    assert!(parse_label_expr("123").is_err());
}

#[test]
fn parse_rejects_hyphenated_label() {
    assert!(parse_label_expr("my-label").is_err());
}

#[test]
fn parse_complex_expression() {
    // (docker|integration)&!slow → must match docker OR integration when slow absent.
    let docker = Label::__new("docker");
    let integration = Label::__new("integration");
    let slow = Label::__new("slow");
    let unit = Label::__new("unit");
    let f = parse_label_expr("(docker|integration)&!slow").unwrap();
    assert!(matches_expr(&f, &[docker]));
    assert!(matches_expr(&f, &[integration]));
    assert!(!matches_expr(&f, &[docker, slow]));
    assert!(!matches_expr(&f, &[unit]));
}

#[test]
fn parse_associative_or_function() {
    // a|b|c — any one of {a,b,c} satisfies.
    let a = Label::__new("a");
    let b = Label::__new("b");
    let c = Label::__new("c");
    let d = Label::__new("d");
    let f = parse_label_expr("a|b|c").unwrap();
    assert!(matches_expr(&f, &[a]));
    assert!(matches_expr(&f, &[b]));
    assert!(matches_expr(&f, &[c]));
    assert!(!matches_expr(&f, &[d]));
}

#[test]
fn parse_associative_and_function() {
    // a&b&c — all three must be present.
    let a = Label::__new("a");
    let b = Label::__new("b");
    let c = Label::__new("c");
    let f = parse_label_expr("a&b&c").unwrap();
    assert!(matches_expr(&f, &[a, b, c]));
    assert!(!matches_expr(&f, &[a, b]));
    assert!(!matches_expr(&f, &[a]));
}

#[test]
fn parse_invalid_empty() {
    assert!(parse_label_expr("").is_err());
}

#[test]
fn parse_invalid_whitespace_only() {
    assert!(parse_label_expr("   ").is_err());
    assert!(parse_label_expr(" \t ").is_err());
}

#[test]
fn parse_invalid_trailing_operator() {
    assert!(parse_label_expr("a|").is_err());
}

#[test]
fn parse_invalid_leading_operator() {
    assert!(parse_label_expr("&a").is_err());
}

#[test]
fn parse_invalid_unmatched_paren() {
    assert!(parse_label_expr("(a").is_err());
}

#[test]
fn parse_invalid_space_separated() {
    assert!(parse_label_expr("a b").is_err());
}

// LabelExpr::matches =====

fn matches(expr: &str, labels: &[Label]) -> bool {
    matches_expr(&parse_label_expr(expr).unwrap(), labels)
}

#[test]
fn eval_label_present() {
    let docker = Label::__new("docker");
    let slow = Label::__new("slow");
    assert!(matches("docker", &[docker, slow]));
}

#[test]
fn eval_label_absent() {
    let unit = Label::__new("unit");
    assert!(!matches("docker", &[unit]));
}

#[test]
fn eval_not() {
    let docker = Label::__new("docker");
    let slow = Label::__new("slow");
    assert!(matches("!slow", &[docker]));
    assert!(!matches("!slow", &[slow]));
}

#[test]
fn eval_and() {
    let docker = Label::__new("docker");
    let slow = Label::__new("slow");
    assert!(matches("docker&slow", &[docker, slow]));
    assert!(!matches("docker&slow", &[docker]));
}

#[test]
fn eval_or() {
    let unit = Label::__new("unit");
    let slow = Label::__new("slow");
    assert!(matches("docker|unit", &[unit]));
    assert!(!matches("docker|unit", &[slow]));
}

#[test]
fn eval_complex() {
    let docker = Label::__new("docker");
    let integration = Label::__new("integration");
    let slow = Label::__new("slow");
    let unit = Label::__new("unit");
    assert!(matches("(docker|integration)&!slow", &[docker]));
    assert!(matches("(docker|integration)&!slow", &[integration]));
    assert!(!matches("(docker|integration)&!slow", &[docker, slow]));
    assert!(!matches("(docker|integration)&!slow", &[unit]));
}

#[test]
fn eval_empty_labels() {
    assert!(!matches("docker", &[]));
    assert!(matches("!docker", &[]));
}

// validate_label_name =====

#[test]
fn validate_accepts_simple_names() {
    validate_label_name("foo");
    validate_label_name("_bar");
    validate_label_name("A1_b2");
    validate_label_name("_");
    validate_label_name("_123");
    validate_label_name("a");
}

#[test]
#[should_panic(expected = "invalid label name")]
fn validate_rejects_empty() {
    validate_label_name("");
}

#[test]
#[should_panic(expected = "invalid label name")]
fn validate_rejects_leading_digit() {
    validate_label_name("1foo");
}

#[test]
#[should_panic(expected = "invalid label name")]
fn validate_rejects_hyphen() {
    validate_label_name("has-dash");
}

#[test]
#[should_panic(expected = "invalid label name")]
fn validate_rejects_space() {
    validate_label_name("has space");
}

#[test]
#[should_panic(expected = "invalid label name")]
fn validate_rejects_exclamation() {
    validate_label_name("foo!");
}

#[test]
#[should_panic(expected = "invalid label name")]
fn new_rejects_invalid_name_at_runtime() {
    Label::__new("bad-name");
}

// LabelFilter =====

#[test]
fn label_filter_parse_and_matches() {
    let docker = Label::__new("docker");
    let slow = Label::__new("slow");
    let filter = LabelFilter::parse("docker & !slow").unwrap();
    assert!(filter.matches(&[docker]));
    assert!(!filter.matches(&[docker, slow]));
    assert!(!filter.matches(&[slow]));
}

#[test]
fn label_filter_parse_error() {
    assert!(LabelFilter::parse("").is_err());
    assert!(LabelFilter::parse("a &").is_err());
}

#[test]
fn label_filter_from_label() {
    let docker = Label::__new("docker");
    let other = Label::__new("other");
    let filter = LabelFilter::from(docker);
    assert!(filter.matches(&[docker]));
    assert!(!filter.matches(&[other]));
    assert!(!filter.matches(&[]));
}

// LabelFilter: Display -----

#[test]
fn label_filter_display_bare_label() {
    let f = LabelFilter::parse("foo").unwrap();
    assert_eq!(f.to_string(), "foo");
}

#[test]
fn label_filter_display_not() {
    let f = LabelFilter::parse("!foo").unwrap();
    assert_eq!(f.to_string(), "!foo");
}

#[test]
fn label_filter_display_and() {
    let f = LabelFilter::parse("a & b").unwrap();
    assert_eq!(f.to_string(), "a & b");
}

#[test]
fn label_filter_display_or() {
    let f = LabelFilter::parse("a | b").unwrap();
    assert_eq!(f.to_string(), "a | b");
}

#[test]
fn label_filter_display_precedence_and_over_or() {
    // a & b | c — no parens needed (AND binds tighter)
    let f = LabelFilter::parse("a & b | c").unwrap();
    assert_eq!(f.to_string(), "a & b | c");
}

#[test]
fn label_filter_display_or_inside_and_canonicalizes() {
    // (a | b) & c is semantically equivalent to (a & c) | (b & c). The canonical
    // form is the latter (sum-of-products from the BDD), and `sort_children`
    // orders the cubes by Display string.
    let f = LabelFilter::parse("(a | b) & c").unwrap();
    assert_eq!(f.to_string(), "a & c | b & c");
}

#[test]
fn label_filter_display_de_morgan() {
    // !(a | b) is semantically equivalent to !a & !b; the canonical form
    // applies De Morgan's law, then sorts children lexicographically (where
    // `!` sorts before letters in ASCII).
    let f = LabelFilter::parse("!(a | b)").unwrap();
    assert_eq!(f.to_string(), "!a & !b");
}

#[test]
fn label_filter_display_double_not_collapses() {
    let f = LabelFilter::parse("!!a").unwrap();
    assert_eq!(f.to_string(), "a");
}

#[test]
fn label_filter_display_complex_canonicalizes() {
    // (docker | integration) & !slow ≡ (!slow & docker) | (!slow & integration).
    // Within each AND, `!slow` sorts before the letter; across the OR, the cubes
    // sort by their Display string (`!slow & docker` < `!slow & integration`).
    let f = LabelFilter::parse("(docker | integration) & !slow").unwrap();
    assert_eq!(f.to_string(), "!slow & docker | !slow & integration");
}

#[test]
fn label_filter_display_round_trip() {
    let exprs = ["a", "!a", "a & b", "a | b", "(a | b) & !c", "!(a & b)", "a & b & c"];
    for expr in exprs {
        let f = LabelFilter::parse(expr).unwrap();
        let displayed = f.to_string();
        let reparsed = LabelFilter::parse(&displayed).unwrap();
        assert_eq!(
            f, reparsed,
            "round-trip failed for {expr:?} (displayed as {displayed:?})"
        );
    }
}

// LabelFilter: operators -----

#[test]
fn label_filter_not_label() {
    let a = Label::__new("a");
    let b = Label::__new("b");
    let filter = !a;
    assert!(!filter.matches(&[a]));
    assert!(filter.matches(&[b]));
}

#[test]
fn label_filter_label_and_label() {
    let a = Label::__new("a");
    let b = Label::__new("b");
    let filter = a & b;
    assert!(filter.matches(&[a, b]));
    assert!(!filter.matches(&[a]));
}

#[test]
fn label_filter_label_or_label() {
    let a = Label::__new("a");
    let b = Label::__new("b");
    let c = Label::__new("c");
    let filter = a | b;
    assert!(filter.matches(&[a]));
    assert!(filter.matches(&[b]));
    assert!(!filter.matches(&[c]));
}

#[test]
fn label_filter_complex_operator_composition() {
    let a = Label::__new("a");
    let b = Label::__new("b");
    let c = Label::__new("c");
    // (a | b) & !c
    let filter = (a | b) & !c;
    assert!(filter.matches(&[a]));
    assert!(filter.matches(&[b]));
    assert!(!filter.matches(&[c]));
    assert!(!filter.matches(&[a, c]));
    assert!(filter.matches(&[a, b]));
}

#[test]
fn label_filter_mixed_types() {
    let a = Label::__new("a");
    let b = Label::__new("b");
    // Label & LabelFilter
    let f: LabelFilter = b.into();
    let filter = a & f;
    assert!(filter.matches(&[a, b]));
    assert!(!filter.matches(&[a]));

    // LabelFilter | Label
    let f2: LabelFilter = a.into();
    let filter2 = f2 | b;
    assert!(filter2.matches(&[a]));
    assert!(filter2.matches(&[b]));
}

// LabelFilter: to_sql -----

#[test]
fn label_filter_to_sql_label() {
    let f = LabelFilter::parse("docker").unwrap();
    assert_eq!(
        f.to_sql(),
        "EXISTS (SELECT 1 FROM labels WHERE running_id = r.id AND label = 'docker')"
    );
}

#[test]
fn label_filter_to_sql_not() {
    let f = LabelFilter::parse("!docker").unwrap();
    assert_eq!(
        f.to_sql(),
        "NOT (EXISTS (SELECT 1 FROM labels WHERE running_id = r.id AND label = 'docker'))"
    );
}

#[test]
fn label_filter_to_sql_and() {
    let f = LabelFilter::parse("a & b").unwrap();
    let sql = f.to_sql();
    assert!(sql.contains("AND"), "expected AND in SQL: {sql}");
    assert!(sql.contains("label = 'a'"));
    assert!(sql.contains("label = 'b'"));
}

#[test]
fn label_filter_to_sql_or() {
    let f = LabelFilter::parse("a | b").unwrap();
    let sql = f.to_sql();
    assert!(sql.contains("OR"), "expected OR in SQL: {sql}");
}

#[test]
fn label_filter_to_sql_complex() {
    let f = LabelFilter::parse("(docker | integration) & !slow").unwrap();
    let sql = f.to_sql();
    assert!(sql.contains("AND"));
    assert!(sql.contains("OR"));
    assert!(sql.contains("NOT"));
    assert!(sql.contains("label = 'docker'"));
    assert!(sql.contains("label = 'integration'"));
    assert!(sql.contains("label = 'slow'"));
}

// Case-insensitive filter parsing -----

#[test]
fn parse_lowercases_uppercase_label() {
    assert_eq!(parse_label_expr("DOCKER").unwrap(), term("docker"));
}

#[test]
fn parse_lowercases_mixed_case_label() {
    assert_eq!(parse_label_expr("Docker").unwrap(), term("docker"));
}

#[test]
fn parse_lowercases_inside_expression() {
    let docker = Label::__new("docker");
    let slow = Label::__new("slow");
    let f = parse_label_expr("Docker & SLOW").unwrap();
    assert!(matches_expr(&f, &[docker, slow]));
    assert!(!matches_expr(&f, &[docker]));
    assert!(!matches_expr(&f, &[slow]));
}

#[test]
fn filter_matches_case_insensitive_simple() {
    let docker = Label::__new("docker");
    let filter = LabelFilter::parse("Docker").unwrap();
    assert!(filter.matches(&[docker]));
}

#[test]
fn filter_matches_case_insensitive_complex() {
    let docker = Label::__new("docker");
    let slow = Label::__new("slow");
    let filter = LabelFilter::parse("DOCKER & !SLOW").unwrap();
    assert!(filter.matches(&[docker]));
    assert!(!filter.matches(&[docker, slow]));
    assert!(!filter.matches(&[slow]));
}

// Cross-validation: validate_label_name and PEG grammar agree -----

#[test]
fn grammar_and_validator_agree() {
    let valid = ["foo", "_bar", "A1_b2", "_", "_123", "a", "docker", "slow"];
    let invalid = ["", "1foo", "has-dash", "has space", "foo!"];

    for name in valid {
        // validate_label_name should not panic
        validate_label_name(name);
        // PEG grammar should parse as a bare label
        assert!(
            parse_label_expr(name).is_ok(),
            "PEG grammar rejected valid label name {:?}",
            name
        );
    }

    for name in invalid {
        // validate_label_name should panic
        let result = std::panic::catch_unwind(|| validate_label_name(name));
        assert!(
            result.is_err(),
            "validate_label_name accepted invalid label name {:?}",
            name
        );
        // PEG grammar should reject
        assert!(
            parse_label_expr(name).is_err(),
            "PEG grammar accepted invalid label name {:?}",
            name
        );
    }

    // Reserved names (`true`, `false`) are an asymmetric case: identifier-shape
    // valid, but rejected by the validator AND interpreted by the grammar as
    // `Const`, never as a `Terminal`. Both sides reference RESERVED_LABEL_NAMES.
    use crate::label::RESERVED_LABEL_NAMES;
    for name in RESERVED_LABEL_NAMES {
        let validated = std::panic::catch_unwind(|| validate_label_name(name));
        assert!(
            validated.is_err(),
            "validate_label_name accepted reserved name {:?}",
            name
        );
        let parsed = parse_label_expr(name).expect("reserved name parses as Const");
        let expected = match *name {
            "true" => boolean_expression::Expr::Const(true),
            "false" => boolean_expression::Expr::Const(false),
            other => panic!("unexpected reserved name {other:?}"),
        };
        assert_eq!(
            parsed, expected,
            "reserved name {name:?} should parse to Const, not Terminal",
        );
    }
}

// Canonicalization tests (azhukova/35) =====
//
// These are added before the implementation per TDD. They exercise the
// canonical-form invariants documented in the design plan: semantic equality,
// constant folding, sentinel-friendly Display, and grammar `true`/`false`
// literals. They are #[ignore]d in this commit; the implementation commit
// removes the ignore.

const PENDING: &str = "pending impl in azhukova/35";

// Semantic equality -----

#[test]
fn canon_eq_and_commutative() {
    assert_eq!(
        LabelFilter::parse("a & b").unwrap(),
        LabelFilter::parse("b & a").unwrap()
    );
}

#[test]
fn canon_eq_or_commutative() {
    assert_eq!(
        LabelFilter::parse("a | b").unwrap(),
        LabelFilter::parse("b | a").unwrap()
    );
}

#[test]
fn canon_eq_distributive() {
    assert_eq!(
        LabelFilter::parse("(a & b) | (a & c)").unwrap(),
        LabelFilter::parse("a & (b | c)").unwrap(),
    );
}

#[test]
fn canon_eq_double_negation() {
    assert_eq!(LabelFilter::parse("!!a").unwrap(), LabelFilter::parse("a").unwrap());
}

#[test]
fn canon_eq_tautology() {
    assert_eq!(
        LabelFilter::parse("a | !a").unwrap(),
        LabelFilter::parse("true").unwrap()
    );
}

#[test]
fn canon_eq_contradiction() {
    assert_eq!(
        LabelFilter::parse("a & !a").unwrap(),
        LabelFilter::parse("false").unwrap()
    );
}

#[test]
fn canon_eq_dedup() {
    assert_eq!(LabelFilter::parse("a | a").unwrap(), LabelFilter::parse("a").unwrap());
    assert_eq!(LabelFilter::parse("a & a").unwrap(), LabelFilter::parse("a").unwrap());
}

#[test]
fn canon_eq_unused_terminal_dropped() {
    // b & !b is a contradiction, so a | (b & !b) is just a.
    assert_eq!(
        LabelFilter::parse("a | (b & !b)").unwrap(),
        LabelFilter::parse("a").unwrap(),
    );
}

#[test]
fn canon_clone_preserves_equality_and_matches() {
    let a = Label::__new("a");
    let b = Label::__new("b");
    let f = LabelFilter::parse("a & b").unwrap();
    let cloned = f.clone();
    assert_eq!(f, cloned);
    assert_eq!(f.matches(&[a, b]), cloned.matches(&[a, b]));
    assert_eq!(f.matches(&[a]), cloned.matches(&[a]));
}

// Display canonical form -----

#[test]
fn canon_display_double_negation_collapses() {
    assert_eq!(LabelFilter::parse("!!a").unwrap().to_string(), "a");
}

#[test]
fn canon_display_tautology() {
    assert_eq!(LabelFilter::parse("a | !a").unwrap().to_string(), "true");
}

#[test]
fn canon_display_contradiction() {
    assert_eq!(LabelFilter::parse("a & !a").unwrap().to_string(), "false");
}

#[test]
fn canon_display_negated_const() {
    assert_eq!(LabelFilter::parse("!true").unwrap().to_string(), "false");
    assert_eq!(LabelFilter::parse("!false").unwrap().to_string(), "true");
}

#[test]
fn canon_round_trip_corpus() {
    let corpus = [
        "a",
        "!a",
        "a & b",
        "a | b",
        "(a | b) & !c",
        "!(a & b)",
        "a & b & c",
        "(a & b) | (a & c)",
        "a | (b & c)",
        "!!!a",
    ];
    for src in corpus {
        let f = LabelFilter::parse(src).unwrap();
        let displayed = f.to_string();
        // Display idempotency: re-parsing the displayed form yields the same string.
        let reparsed = LabelFilter::parse(&displayed).unwrap();
        assert_eq!(reparsed.to_string(), displayed, "display drift on {src:?}");
        // Structural-equality round-trip: re-parsing matches the original filter.
        assert_eq!(reparsed, f, "structural round-trip failed on {src:?}");
    }
}

// From<Label> matches parse -----

#[test]
fn canon_from_label_matches_parse() {
    let a = Label::__new("a");
    assert_eq!(LabelFilter::from(a), LabelFilter::parse("a").unwrap());
}

// Grammar boundaries (true/false literals) -----

#[test]
fn canon_grammar_true_matches_everything() {
    let a = Label::__new("a");
    let f = LabelFilter::parse("true").unwrap();
    assert!(f.matches(&[]));
    assert!(f.matches(&[a]));
}

#[test]
fn canon_grammar_false_matches_nothing() {
    let a = Label::__new("a");
    let b = Label::__new("b");
    let f = LabelFilter::parse("false").unwrap();
    assert!(!f.matches(&[]));
    assert!(!f.matches(&[a, b]));
}

#[test]
fn canon_grammar_truelike_label_names() {
    // `true` and `false` are reserved, but anything that has them as a prefix
    // and continues with an identifier character is a regular label name.
    // Labels are stored canonical-lowercase (per #29), so the filter parser
    // lowercases mixed-case identifiers like `TrueLike` to `truelike`.
    let cases: &[(&str, Label)] = &[
        ("truely", Label::__new("truely")),
        ("truex", Label::__new("truex")),
        ("true_", Label::__new("true_")),
        ("falsey", Label::__new("falsey")),
        ("_true", Label::__new("_true")),
        ("TrueLike", Label::__new("truelike")),
        ("Falseish", Label::__new("falseish")),
    ];
    for (name, lbl) in cases {
        let f = LabelFilter::parse(name).unwrap();
        // Behaves like a single-label filter, not a constant.
        assert!(f.matches(&[*lbl]), "label-like name {name:?} should match itself");
        assert!(!f.matches(&[]), "label-like name {name:?} should not match empty");
    }
}

#[test]
fn canon_grammar_uppercase_true_false_collapse_to_const() {
    // After bool_lit became case-insensitive (matching the case-insensitive
    // label parsing from #29), `True`/`FALSE` parse as Const, not Terminal.
    assert!(LabelFilter::parse("True").unwrap().is_tautology());
    assert!(LabelFilter::parse("FALSE").unwrap().is_contradiction());
    assert!(LabelFilter::parse("tRuE").unwrap().is_tautology());
}

// Matching with extra labels -----

#[test]
fn canon_matches_extra_labels() {
    let a = Label::__new("a");
    let b = Label::__new("b");
    let c = Label::__new("c");
    assert!(LabelFilter::parse("a").unwrap().matches(&[a, b, c]));
    assert!(LabelFilter::parse("!a").unwrap().matches(&[b, c]));
}

// SQL for constants -----

#[test]
fn canon_sql_true() {
    let sql = LabelFilter::parse("true").unwrap().to_sql();
    assert_eq!(sql, "1=1");
    assert!(
        !sql.contains("label = 'true'"),
        "true literal must not leak as a label name in SQL"
    );
}

#[test]
fn canon_sql_false() {
    let sql = LabelFilter::parse("false").unwrap().to_sql();
    assert_eq!(sql, "1=0");
}

// validate_serial_filters with constants -----

#[test]
fn canon_serial_filter_false_validates() {
    // `serial = "false"` is well-formed and parses to a contradiction.
    assert!(LabelFilter::parse("false").is_ok());
}

// Avoid unused-const warning when the rest are run with --ignored.
#[test]
fn _canon_pending_marker_is_present() {
    assert_eq!(PENDING, "pending impl in azhukova/35");
}

// Property-based canonicity test (TP-canonicity from the design plan).
//
// Generates random filter STRINGS (not Exprs — that would beg the question on
// Display) over the alphabet {a,b,c,d}. For each pair (s1, s2):
//   semantic equality (truth tables match)  ⇔  structural equality of
//   canonical LabelFilter (==).
//
// The 16 possible label-set assignments are enumerated explicitly; each
// assignment is a subset of {a,b,c,d}.

mod canon_proptest {
    use super::*;
    use proptest::prelude::*;

    /// Generate a syntactically valid filter string over {a,b,c,d}.
    /// Bounded depth keeps truth-table evaluation cheap.
    fn filter_strategy() -> impl Strategy<Value = String> {
        let leaf = prop_oneof![
            Just("a".to_string()),
            Just("b".to_string()),
            Just("c".to_string()),
            Just("d".to_string()),
        ];
        leaf.prop_recursive(
            4,  // max nesting depth
            32, // target size
            4,  // items per inner collection
            |inner| {
                prop_oneof![
                    inner.clone().prop_map(|s| format!("!{s}")),
                    (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} & {b})")),
                    (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} | {b})")),
                    inner.prop_map(|s| format!("({s})")),
                ]
            },
        )
    }

    /// Compute the truth table of a parsed filter over all 16 subsets of {a,b,c,d}.
    fn truth_table(filter: &LabelFilter) -> u16 {
        const A: Label = Label::__new("a");
        const B: Label = Label::__new("b");
        const C: Label = Label::__new("c");
        const D: Label = Label::__new("d");
        let mut bits: u16 = 0;
        for mask in 0u16..16 {
            let mut labels: Vec<Label> = Vec::new();
            if mask & 1 != 0 {
                labels.push(A);
            }
            if mask & 2 != 0 {
                labels.push(B);
            }
            if mask & 4 != 0 {
                labels.push(C);
            }
            if mask & 8 != 0 {
                labels.push(D);
            }
            if filter.matches(&labels) {
                bits |= 1 << mask;
            }
        }
        bits
    }

    proptest! {
        #[test]
                fn semantic_equality_matches_structural_equality(
            s1 in filter_strategy(),
            s2 in filter_strategy(),
        ) {
            let f1 = LabelFilter::parse(&s1).unwrap();
            let f2 = LabelFilter::parse(&s2).unwrap();
            let same_truth_table = truth_table(&f1) == truth_table(&f2);
            let structurally_equal = f1 == f2;
            prop_assert_eq!(
                same_truth_table, structurally_equal,
                "semantic vs structural mismatch on s1={:?}, s2={:?}", s1, s2,
            );
        }

        #[test]
                fn display_round_trip_is_stable(s in filter_strategy()) {
            let f = LabelFilter::parse(&s).unwrap();
            let displayed = f.to_string();
            let reparsed = LabelFilter::parse(&displayed).unwrap();
            prop_assert_eq!(reparsed, f);
        }
    }
}
