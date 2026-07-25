//! Renders `graph::TestGroup`s into an nextest tool-config-file, consumed
//! via `cargo nextest run --tool-config-file skuld:<path>`.

use crate::graph::TestGroup;
use std::collections::BTreeMap;

const MIN_NEXTEST_VERSION: &str = "0.9.85";

#[derive(serde::Serialize)]
struct ToolConfigFile {
    #[serde(rename = "nextest-version")]
    nextest_version: NextestVersionReq,
    #[serde(rename = "test-groups", skip_serializing_if = "BTreeMap::is_empty")]
    test_groups: BTreeMap<String, TestGroupDef>,
    #[serde(skip_serializing_if = "ProfileSection::is_empty")]
    profile: ProfileSection,
}

#[derive(serde::Serialize)]
struct NextestVersionReq {
    required: String,
}

#[derive(serde::Serialize)]
struct TestGroupDef {
    #[serde(rename = "max-threads")]
    max_threads: u32,
}

#[derive(serde::Serialize, Default)]
struct ProfileSection {
    default: DefaultProfile,
}

impl ProfileSection {
    fn is_empty(&self) -> bool {
        self.default.overrides.is_empty()
    }
}

#[derive(serde::Serialize, Default)]
struct DefaultProfile {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    overrides: Vec<OverrideEntry>,
}

#[derive(serde::Serialize)]
struct OverrideEntry {
    filter: String,
    #[serde(rename = "test-group")]
    test_group: String,
}

/// Escape a test/binary name for nextest's `=` equality matcher, per
/// nextest's documented filterset escape sequences (`\n \r \t \\ \/ \) \,`).
/// Every other character, including spaces and `(`, passes through
/// unescaped — nextest's filterset grammar deliberately avoids
/// quote-delimited literals, and only `)`/`,`/the other five documented
/// sequences are called out as needing escaping. `)` is safety-critical
/// here specifically: our own clauses wrap each member in `(...)`, so an
/// unescaped `)` in a name would prematurely close that wrapping paren.
/// Verified end-to-end (not just by string assertion) against a real
/// `cargo nextest run --tool-config-file` invocation — see
/// `cargo-skuld-nextest/tests/gen_and_run.rs`.
fn escape_nextest_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            '/' => out.push_str("\\/"),
            ')' => out.push_str("\\)"),
            ',' => out.push_str("\\,"),
            other => out.push(other),
        }
    }
    out
}

fn filter_for_group(group: &TestGroup) -> String {
    group
        .members
        .iter()
        .map(|(binary_id, name)| {
            format!(
                "(binary_id(={}) and test(={}))",
                escape_nextest_name(binary_id),
                escape_nextest_name(name)
            )
        })
        .collect::<Vec<_>>()
        .join(" or ")
}

pub fn render_tool_config(groups: &[TestGroup]) -> String {
    let test_groups = groups
        .iter()
        .map(|g| (g.name.clone(), TestGroupDef { max_threads: 1 }))
        .collect();
    let overrides = groups
        .iter()
        .map(|g| OverrideEntry {
            filter: filter_for_group(g),
            test_group: g.name.clone(),
        })
        .collect();
    let config = ToolConfigFile {
        nextest_version: NextestVersionReq {
            required: MIN_NEXTEST_VERSION.to_string(),
        },
        test_groups,
        profile: ProfileSection {
            default: DefaultProfile { overrides },
        },
    };
    toml::to_string(&config)
        .expect("ToolConfigFile serialization cannot fail (no maps with non-string keys, no floats)")
}

#[cfg(test)]
mod emit_tests;
