use super::*;
use crate::LabelFilter;

// Fixture serial-filter merging — canonicalization invariants (azhukova/35).
//
// `merge_serial_filters` produces possibly-redundant strings; canonicalization
// happens later when the merged string is parsed into a LabelFilter. These
// tests assert that canonicalization actually collapses the redundancies.

#[test]
fn merge_dedup_same_label() {
    let merged = merge_serial_filters("a", "a");
    let canon = LabelFilter::parse(&merged).unwrap().to_string();
    assert_eq!(canon, "a");
}

#[test]
fn merge_dedup_commutative() {
    let merged = merge_serial_filters("a & b", "b & a");
    let canon = LabelFilter::parse(&merged).unwrap();
    assert_eq!(canon, LabelFilter::parse("a & b").unwrap());
}

#[test]
fn merge_tautology_canonicalizes_to_const_true() {
    // a | !a ≡ true. After canonicalization, displayed as the literal "true".
    let merged = merge_serial_filters("a", "!a");
    let canon = LabelFilter::parse(&merged).unwrap().to_string();
    assert_eq!(canon, "true");
}

// Invariant: `merge_serial_filters` itself only emits "*" when an input is "*".
// (Sentinel collapse for tautological filters happens later, in the storage
// layer, not in the raw merge.) Guards against a future regression where a
// well-meaning refactor moves the collapse into the merge function and breaks
// downstream callers that depend on the raw merge output.
#[test]
fn merge_never_emits_star_from_non_star_inputs() {
    for (a, b) in [("a", "b"), ("a", "!a"), ("a & b", "c | d"), ("", "a"), ("a", "")] {
        let merged = merge_serial_filters(a, b);
        if a != "*" && b != "*" {
            assert_ne!(
                merged, "*",
                "merge_serial_filters({a:?}, {b:?}) unexpectedly returned the global-serial sentinel"
            );
        }
    }
}

// Fixture-graph walking and label collection =====

fn labeled_def(name: &'static str, deps: &'static [&'static str], labels: &'static [Label]) -> FixtureDef {
    FixtureDef {
        name,
        scope: FixtureScope::Variable,
        requires: &[],
        deps,
        labels,
        setup: || Ok(Box::new(())),
        cast: |_, _| None,
        type_name: "()",
        serial: "",
    }
}

fn registry_of(defs: &[FixtureDef]) -> HashMap<&str, &FixtureDef> {
    defs.iter().map(|d| (d.name, d)).collect()
}

fn visited_names(registry: &HashMap<&str, &FixtureDef>, roots: &[&str]) -> Vec<&'static str> {
    let mut seen = Vec::new();
    walk_fixture_deps_in(registry, roots, |def| seen.push(def.name));
    seen
}

#[test]
fn walk_visits_each_fixture_once_in_first_visit_order() {
    let defs = [
        labeled_def("top", &["left", "right"], &[]),
        labeled_def("left", &["base"], &[]),
        labeled_def("right", &["base"], &[]),
        labeled_def("base", &[], &[]),
    ];
    assert_eq!(
        visited_names(&registry_of(&defs), &["top", "right"]),
        ["top", "left", "base", "right"]
    );
}

#[test]
fn walk_terminates_on_a_cycle() {
    let defs = [labeled_def("a", &["b"], &[]), labeled_def("b", &["a"], &[])];
    assert_eq!(visited_names(&registry_of(&defs), &["a"]), ["a", "b"]);
}

#[test]
fn walk_skips_an_unregistered_root() {
    let defs = [labeled_def("a", &[], &[])];
    assert_eq!(visited_names(&registry_of(&defs), &["missing", "a"]), ["a"]);
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "depends on unregistered fixture")]
fn walk_rejects_an_unregistered_dependency() {
    let defs = [labeled_def("a", &["missing"], &[])];
    visited_names(&registry_of(&defs), &["a"]);
}

#[test]
fn labels_collected_during_a_walk_are_deduplicated() {
    let a = Label::__new("la");
    let b = Label::__new("lb");
    let labels_a: &'static [Label] = Box::leak(Box::new([a, b, a]));
    let labels_b: &'static [Label] = Box::leak(Box::new([b]));
    let defs = [
        labeled_def("top", &["x", "y"], labels_a),
        labeled_def("x", &[], labels_b),
        labeled_def("y", &["x"], &[]),
    ];
    let registry = registry_of(&defs);
    let mut labels = Vec::new();
    walk_fixture_deps_in(&registry, &["top"], |def| push_unique(&mut labels, def.labels));
    assert_eq!(labels, [a, b]);
}

#[test]
fn fixture_labels_of_a_def_do_not_need_the_registry() {
    let a = Label::__new("la");
    let own: &'static [Label] = Box::leak(Box::new([a]));
    let def = labeled_def("not_registered_anywhere", &[], own);
    assert_eq!(fixture_labels_of(&def), [a]);
}
