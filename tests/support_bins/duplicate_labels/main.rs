//! Minimal skuld-powered binary that declares the same label in two
//! sibling modules, to verify that startup label-registry validation
//! detects cross-module (same-binary, functionally cross-crate)
//! duplicates and panics with both source locations.
//!
//! Invoked as a subprocess by `tests/duplicate_labels_cli.rs`.
//!
//! The two `mod` declarations suffice to keep the modules in the linkage
//! graph; `inventory::submit!` places its statics in linker-managed
//! sections, so no explicit reference to `a::DUP` / `b::DUP` is needed.

#[path = "a.rs"]
mod a;
#[path = "b.rs"]
mod b;

fn main() {
    skuld::run_all();
}
