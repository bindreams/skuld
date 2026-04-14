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
