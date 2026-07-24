pub mod discovery;
pub mod emit;
pub mod graph;
pub mod metadata;

// TODO(Task 8): replace this placeholder with the real
// `build_nextest_run_command` implementation.
pub mod command {
    pub fn build_nextest_run_command(
        _tool_config_path: &std::path::Path,
        _passthrough: &[String],
    ) -> std::process::Command {
        unimplemented!("replaced in Task 8")
    }
}
