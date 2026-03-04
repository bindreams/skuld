//! Per-test fixture providing serializable metadata about the current test.

use crate::metadata::TestMetadata;

#[skuld::fixture(scope = test)]
fn metadata() -> Result<TestMetadata, String> {
    Ok(TestMetadata::from_current())
}
