//! Serializable metadata about tests and fixtures for debugging and observability.

use std::fmt;

use serde::Serialize;

use crate::fixture::{collect_fixture_serial, fixture_registry, merge_serial_filters, FixtureDef, FixtureScope};
use crate::{Ignore, Requirement, ShouldPanic, TestDef};

// RequirementInfo =================================================================================

/// Serializable snapshot of a [`Requirement`]: its name and current evaluation result.
#[derive(Clone, Debug, Serialize)]
pub struct RequirementInfo {
    pub name: String,
    pub met: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl RequirementInfo {
    /// Evaluate a requirement and capture the result.
    pub fn from_requirement(req: &Requirement) -> Self {
        match req.eval() {
            Ok(()) => Self {
                name: req.name.to_owned(),
                met: true,
                reason: None,
            },
            Err(e) => Self {
                name: req.name.to_owned(),
                met: false,
                reason: Some(e),
            },
        }
    }
}

// FixtureMetadata =================================================================================

/// Serializable metadata about a registered fixture.
#[derive(Clone, Debug, Serialize)]
pub struct FixtureMetadata {
    pub name: String,
    pub scope: String,
    pub serial: String,
    pub deps: Vec<String>,
    pub type_name: String,
    pub requires: Vec<RequirementInfo>,
}

impl FixtureMetadata {
    /// Build from a [`FixtureDef`].
    pub fn from_def(def: &FixtureDef) -> Self {
        Self {
            name: def.name.to_owned(),
            scope: match def.scope {
                FixtureScope::Variable => "variable",
                FixtureScope::Test => "test",
                FixtureScope::Process => "process",
            }
            .to_owned(),
            serial: def.serial.to_owned(),
            deps: def.deps.iter().map(|s| s.to_string()).collect(),
            type_name: def.type_name.to_owned(),
            requires: def.requires.iter().map(RequirementInfo::from_requirement).collect(),
        }
    }
}

impl fmt::Display for FixtureMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let yaml = serde_yml::to_string(self).map_err(|_| fmt::Error)?;
        f.write_str(&yaml)
    }
}

// TestMetadata ====================================================================================

/// Serializable metadata about a test: its name, fixtures, labels, serial
/// status, and requirements. Exposed as the built-in `metadata` fixture.
#[derive(Clone, Debug, Serialize)]
pub struct TestMetadata {
    pub name: String,
    pub module: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub serial: String,
    pub labels: Vec<String>,
    pub ignore: String,
    pub should_panic: String,
    pub fixtures: Vec<FixtureMetadata>,
    pub requires: Vec<RequirementInfo>,
}

impl TestMetadata {
    /// Build from the currently executing test (uses thread-local context +
    /// [`test_registry`](crate::test_registry) for O(1) lookup).
    ///
    /// # Panics
    ///
    /// If called outside a test body or if no matching `TestDef` is found
    /// (e.g. for dynamic tests added via `TestRunner::add`).
    pub fn from_current() -> Self {
        let ct = crate::current_test();
        let test_def = crate::test_registry()
            .get(&(ct.name, ct.module_path))
            .unwrap_or_else(|| {
                panic!(
                    "TestMetadata::from_current: no TestDef for ({:?}, {:?})",
                    ct.name, ct.module_path
                )
            });
        Self::from_def(test_def)
    }

    /// Build from a [`TestDef`].
    pub fn from_def(def: &TestDef) -> Self {
        let registry = fixture_registry();
        let fixture_metas: Vec<FixtureMetadata> = def
            .fixture_names
            .iter()
            .filter_map(|&name| registry.get(name).map(|d| FixtureMetadata::from_def(d)))
            .collect();

        let fixture_serial = collect_fixture_serial(def.fixture_names);
        let effective_serial = merge_serial_filters(def.serial, &fixture_serial);

        let requires: Vec<RequirementInfo> = def.requires.iter().map(RequirementInfo::from_requirement).collect();

        let ignore = match def.ignore {
            Ignore::No => "no".to_owned(),
            Ignore::Yes => "yes".to_owned(),
            Ignore::WithReason(r) => format!("yes: {r}"),
        };

        let should_panic = match def.should_panic {
            ShouldPanic::No => "no".to_owned(),
            ShouldPanic::Yes => "yes".to_owned(),
            ShouldPanic::WithMessage(m) => format!("yes: {m}"),
        };

        Self {
            name: def.name.to_owned(),
            module: def.module.to_owned(),
            display_name: def.display_name.map(str::to_owned),
            serial: effective_serial,
            labels: def.labels.iter().map(|l| l.name().to_owned()).collect(),
            ignore,
            should_panic,
            fixtures: fixture_metas,
            requires,
        }
    }
}

impl fmt::Display for TestMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let yaml = serde_yml::to_string(self).map_err(|_| fmt::Error)?;
        f.write_str(&yaml)
    }
}
