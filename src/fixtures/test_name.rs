//! Per-test fixture providing the current test function name.

use std::fmt;
use std::ops::Deref;

/// The current test's function name. Implements `Deref<Target = str>` so it
/// can be used transparently as `&str` via `#[fixture(test_name)] name: &str`.
pub struct TestName(String);

impl Deref for TestName {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TestName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[skuld::fixture(scope = test, deref)]
pub fn test_name() -> Result<TestName, String> {
    Ok(TestName(crate::current_test().name.to_owned()))
}
