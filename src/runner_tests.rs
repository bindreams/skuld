//! Tests for the serial coordination integration in [`crate::runner`].
//!
//! The core coordination logic (can_start, register, concurrent access) is
//! tested in [`crate::coordination::coordination_tests`]. These tests verify
//! that the runner correctly wires up coordination for test execution.

// No additional runner-level tests needed at this point — the coordination
// module has comprehensive tests. Integration tests in
// tests/integration_support/serial_tests.rs cover end-to-end behavior.
