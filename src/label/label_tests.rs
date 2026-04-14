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

fn label(name: &str) -> LabelExpr {
    LabelExpr::Label(name.into())
}

fn not(inner: LabelExpr) -> LabelExpr {
    LabelExpr::Not(Box::new(inner))
}

fn and(left: LabelExpr, right: LabelExpr) -> LabelExpr {
    LabelExpr::And(Box::new(left), Box::new(right))
}

fn or(left: LabelExpr, right: LabelExpr) -> LabelExpr {
    LabelExpr::Or(Box::new(left), Box::new(right))
}

#[test]
fn parse_bare_label() {
    assert_eq!(parse_label_expr("unit").unwrap(), label("unit"));
}

#[test]
fn parse_or() {
    assert_eq!(parse_label_expr("a|b").unwrap(), or(label("a"), label("b")));
}

#[test]
fn parse_and() {
    assert_eq!(parse_label_expr("a&b").unwrap(), and(label("a"), label("b")));
}

#[test]
fn parse_not() {
    assert_eq!(parse_label_expr("!a").unwrap(), not(label("a")));
}

#[test]
fn parse_precedence_not_over_and() {
    // !a&b → (!a) & b
    assert_eq!(parse_label_expr("!a&b").unwrap(), and(not(label("a")), label("b")));
}

#[test]
fn parse_precedence_and_over_or() {
    // a|b&c → a | (b&c)
    assert_eq!(
        parse_label_expr("a|b&c").unwrap(),
        or(label("a"), and(label("b"), label("c")))
    );
}

#[test]
fn parse_grouping_overrides_precedence() {
    // (a|b)&c
    assert_eq!(
        parse_label_expr("(a|b)&c").unwrap(),
        and(or(label("a"), label("b")), label("c"))
    );
}

#[test]
fn parse_whitespace_ignored() {
    assert_eq!(parse_label_expr("  a  |  b  ").unwrap(), or(label("a"), label("b")));
}

#[test]
fn parse_double_negation() {
    assert_eq!(parse_label_expr("!!a").unwrap(), not(not(label("a"))));
}

#[test]
fn parse_underscores_and_digits() {
    assert_eq!(parse_label_expr("my_label_2").unwrap(), label("my_label_2"));
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
    // (docker|integration)&!slow
    assert_eq!(
        parse_label_expr("(docker|integration)&!slow").unwrap(),
        and(or(label("docker"), label("integration")), not(label("slow")))
    );
}

#[test]
fn parse_left_associative_or() {
    // a|b|c → Or(Or(a,b),c)
    assert_eq!(
        parse_label_expr("a|b|c").unwrap(),
        or(or(label("a"), label("b")), label("c"))
    );
}

#[test]
fn parse_left_associative_and() {
    // a&b&c → And(And(a,b),c)
    assert_eq!(
        parse_label_expr("a&b&c").unwrap(),
        and(and(label("a"), label("b")), label("c"))
    );
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
    parse_label_expr(expr).unwrap().matches(labels)
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
fn label_filter_display_or_inside_and() {
    // (a | b) & c — parens needed
    let f = LabelFilter::parse("(a | b) & c").unwrap();
    assert_eq!(f.to_string(), "(a | b) & c");
}

#[test]
fn label_filter_display_not_complex() {
    let f = LabelFilter::parse("!(a | b)").unwrap();
    assert_eq!(f.to_string(), "!(a | b)");
}

#[test]
fn label_filter_display_double_not() {
    let f = LabelFilter::parse("!!a").unwrap();
    assert_eq!(f.to_string(), "!!a");
}

#[test]
fn label_filter_display_complex() {
    let f = LabelFilter::parse("(docker | integration) & !slow").unwrap();
    assert_eq!(f.to_string(), "(docker | integration) & !slow");
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
    assert_eq!(parse_label_expr("DOCKER").unwrap(), label("docker"));
}

#[test]
fn parse_lowercases_mixed_case_label() {
    assert_eq!(parse_label_expr("Docker").unwrap(), label("docker"));
}

#[test]
fn parse_lowercases_inside_expression() {
    assert_eq!(
        parse_label_expr("Docker & SLOW").unwrap(),
        and(label("docker"), label("slow")),
    );
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
}
