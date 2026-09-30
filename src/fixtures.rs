//! Built-in fixtures provided by skuld.

pub mod cwd;
pub mod env;
pub mod metadata;
pub mod temp_dir;
#[cfg(test)]
mod temp_dir_tests;
pub mod test_name;
#[cfg(all(test, windows))]
mod adv_probe_tests;
