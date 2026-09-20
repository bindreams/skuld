//! Conflict graph over dumped test metadata, mirroring
//! `coordination::can_start` exactly via `skuld::LabelFilter::matches_names`.
//! Connected components of size > 1 become one `max-threads = 1` nextest
//! group each; isolated tests are left ungrouped.

use crate::metadata::TestMetadata;
use petgraph::unionfind::UnionFind;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestGroup {
    pub name: String,
    pub members: Vec<(String, String)>,
}

fn conflicts(a: &TestMetadata, b: &TestMetadata) -> bool {
    if a.serial_filter == "*" || b.serial_filter == "*" {
        return true;
    }
    if !a.serial_filter.is_empty() {
        // Callers (metadata::collect_metadata) already validate every
        // serial_filter parses before it reaches here — a failure at this
        // point is an internal invariant violation, not reachable data.
        let filter = skuld::LabelFilter::parse(&a.serial_filter)
            .expect("serial_filter must already be validated by collect_metadata");
        let b_labels: Vec<&str> = b.labels.iter().map(String::as_str).collect();
        if filter.matches_names(&b_labels) {
            return true;
        }
    }
    if !b.serial_filter.is_empty() {
        let filter = skuld::LabelFilter::parse(&b.serial_filter)
            .expect("serial_filter must already be validated by collect_metadata");
        let a_labels: Vec<&str> = a.labels.iter().map(String::as_str).collect();
        if filter.matches_names(&a_labels) {
            return true;
        }
    }
    false
}

/// Derive a group's name from its own canonicalized membership — stable
/// (unlike `DefaultHasher`, which std explicitly does not guarantee stable
/// across builds), independent of which OTHER groups exist (unlike a
/// positional index), and, using blake3's first 64 output bits rather than
/// a 32-bit CRC, with a collision space wide enough that an accidental
/// collision at realistic test-suite scale is not a practical concern (the
/// same standard used for e.g. git object naming), not merely "unlikely."
///
/// Prefixed with `@tool:skuld:`: nextest requires every test-group defined
/// by a tool-provided config (`--tool-config-file skuld:<path>`) to carry
/// that namespace prefix, and rejects the config outright otherwise.
fn group_name(members: &[(String, String)]) -> String {
    let mut buf = Vec::new();
    for (binary_id, name) in members {
        buf.extend_from_slice(binary_id.as_bytes());
        buf.push(0u8);
        buf.extend_from_slice(name.as_bytes());
        buf.push(0u8);
    }
    format!("@tool:skuld:skuld_group_{}", &blake3::hash(&buf).to_hex()[..16])
}

pub fn build_groups(tests: &[TestMetadata]) -> Vec<TestGroup> {
    let mut uf = UnionFind::new(tests.len());
    for i in 0..tests.len() {
        for j in (i + 1)..tests.len() {
            if conflicts(&tests[i], &tests[j]) {
                uf.union(i, j);
            }
        }
    }

    let mut components: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..tests.len() {
        components.entry(uf.find(i)).or_default().push(i);
    }

    let mut by_name: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for indices in components.into_values().filter(|indices| indices.len() > 1) {
        let mut members: Vec<(String, String)> = indices
            .into_iter()
            .map(|i| (tests[i].binary_id.clone(), tests[i].name.clone()))
            .collect();
        members.sort();
        let name = group_name(&members);
        if let Some(existing) = by_name.insert(name.clone(), members.clone()) {
            // Defense-in-depth, not a live concern at this hash width: if
            // this ever fires, it points to a canonicalization bug (two
            // genuinely different member sets encoding to the same bytes),
            // not a real blake3 collision.
            assert_eq!(
                existing, members,
                "group name {name:?} bound to two different member sets"
            );
        }
    }

    by_name
        .into_iter()
        .map(|(name, members)| TestGroup { name, members })
        .collect()
}

#[cfg(test)]
mod graph_tests;
