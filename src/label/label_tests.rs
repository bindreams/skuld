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
fn parse_hyphens_and_underscores() {
    assert_eq!(parse_label_expr("my-label_2").unwrap(), label("my-label_2"));
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
