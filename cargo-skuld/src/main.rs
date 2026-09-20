use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "cargo-skuld", bin_name = "cargo skuld", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Integrate Skuld's serial test coordination with cargo-nextest.
    Nextest {
        #[command(subcommand)]
        command: NextestCommand,
    },
}

#[derive(Subcommand)]
enum NextestCommand {
    /// Regenerate the tool-config-file and run `cargo nextest run` with it.
    Run {
        #[arg(long, default_value = "target/skuld/nextest.toml")]
        output: PathBuf,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        nextest_args: Vec<String>,
    },
    /// Write the tool-config-file to a tracked location.
    Gen {
        #[arg(long, default_value = ".config/skuld-nextest.toml")]
        output: PathBuf,
        #[arg(long)]
        check: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("skuld") {
        args.remove(1);
    }
    let cli = Cli::parse_from(args);
    let manifest_dir = std::env::current_dir()?;

    // `Command` has a single variant, so this destructuring is irrefutable;
    // it stays a `match` on the inner enum as subcommands are added.
    let Command::Nextest { command } = cli.command;
    match command {
        NextestCommand::Gen { output, check } => {
            let rendered = generate(&manifest_dir)?;
            if check {
                let existing = match std::fs::read_to_string(&output) {
                    Ok(s) => Some(s),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => anyhow::bail!("failed to read {}: {e}", output.display()),
                };
                anyhow::ensure!(
                    existing.as_deref() == Some(rendered.as_str()),
                    "{} is stale — run `cargo skuld nextest gen` to update it",
                    output.display()
                );
            } else {
                if let Some(parent) = output.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&output, rendered)?;
            }
        }
        NextestCommand::Run { output, nextest_args } => {
            let rendered = generate(&manifest_dir)?;
            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&output, rendered)?;
            // `cargo nextest run --tool-config-file` requires an absolute
            // path; `output` may be relative (e.g. the default
            // `target/skuld/nextest.toml`).
            let absolute_output = std::fs::canonicalize(&output)?;
            let mut cmd = cargo_skuld::command::build_nextest_run_command(&absolute_output, &nextest_args);
            let status = cmd.status()?;
            std::process::exit(status.code().unwrap_or(1));
        }
    }
    Ok(())
}

fn generate(manifest_dir: &std::path::Path) -> anyhow::Result<String> {
    let binaries = cargo_skuld::discovery::discover_binaries(manifest_dir)?;
    let metadata = cargo_skuld::metadata::collect_metadata(&binaries)?;
    let groups = cargo_skuld::graph::build_groups(&metadata);
    Ok(cargo_skuld::emit::render_tool_config(&groups))
}
