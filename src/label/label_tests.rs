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

// parse_label_filter =====

#[test]
fn parse_label_filter_none() {
    assert_eq!(parse_label_filter(None), None);
}

#[test]
fn parse_label_filter_empty() {
    assert_eq!(parse_label_filter(Some(String::new())), Some(vec![]));
}

#[test]
fn parse_label_filter_single() {
    assert_eq!(
        parse_label_filter(Some("docker".into())),
        Some(vec!["docker".to_string()])
    );
}

#[test]
fn parse_label_filter_multiple() {
    assert_eq!(
        parse_label_filter(Some("docker,slow".into())),
        Some(vec!["docker".to_string(), "slow".to_string()])
    );
}

#[test]
fn parse_label_filter_trims_whitespace() {
    assert_eq!(
        parse_label_filter(Some(" docker , slow ".into())),
        Some(vec!["docker".to_string(), "slow".to_string()])
    );
}

#[test]
fn parse_label_filter_filters_empty_segments() {
    assert_eq!(
        parse_label_filter(Some("docker,,slow,".into())),
        Some(vec!["docker".to_string(), "slow".to_string()])
    );
}

// label_matches =====

#[test]
fn label_matches_none_filter_passes_all() {
    let labels = [Label::__new("docker")];
    assert!(label_matches(&labels, None));
    assert!(label_matches(&[], None));
}

#[test]
fn label_matches_empty_filter_passes_none() {
    let labels = [Label::__new("docker")];
    let filter: Vec<String> = vec![];
    assert!(!label_matches(&labels, Some(&filter)));
    assert!(!label_matches(&[], Some(&filter)));
}

#[test]
fn label_matches_single_filter() {
    let docker = Label::__new("docker");
    let slow = Label::__new("slow");
    let filter = vec!["docker".to_string()];

    assert!(label_matches(&[docker], Some(&filter)));
    assert!(label_matches(&[docker, slow], Some(&filter)));
    assert!(!label_matches(&[slow], Some(&filter)));
    assert!(!label_matches(&[], Some(&filter)));
}

#[test]
fn label_matches_union_semantics() {
    let docker = Label::__new("docker");
    let slow = Label::__new("slow");
    let unit = Label::__new("unit");
    let filter = vec!["docker".to_string(), "slow".to_string()];

    assert!(label_matches(&[docker], Some(&filter)));
    assert!(label_matches(&[slow], Some(&filter)));
    assert!(label_matches(&[docker, slow], Some(&filter)));
    assert!(!label_matches(&[unit], Some(&filter)));
}
