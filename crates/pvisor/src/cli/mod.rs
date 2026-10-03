//! Job lifecycle commands and discovery of independent executable extensions.
mod commands;
pub mod extensions;
mod product;
mod run;
pub mod runtime;
#[cfg(unix)]
pub mod terminal;
mod trajectory;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
pub use commands::cache_main;
pub use run::RunArgs;
use std::ffi::OsString;

#[derive(Debug, Parser)]
#[command(
    name = "pvisor",
    version,
    about = "Manage Agent Jobs through one execution kernel"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(about = run::RUN_COMMAND_ABOUT, long_about = run::RUN_COMMAND_LONG_ABOUT)]
    Run(Box<run::RunArgs>),
    /// Apply selected staged changes from a stopped Job.
    Apply(runtime::ApplyArgs),
    /// Discard staged changes from a stopped Job.
    Drop(runtime::SelectArgs),
    /// Show a Job's process, filesystem, and network status.
    Status(runtime::StatusArgs),
    /// Request graceful termination of a live Job.
    Kill(runtime::KillArgs),
    /// Start a new safe Job from a stopped Job or checkpoint.
    Fork(run::ForkArgs),
    /// Open a read-only shell or run a command against a Job filesystem view.
    Inspect(runtime::InspectArgs),
    /// List installed executable extensions and their descriptions.
    Extensions,
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

fn root_command() -> anyhow::Result<clap::Command> {
    let mut command = Cli::command().after_help("Use pvisor -- COMMAND for default execution. Independent tools are discovered from companion commands alongside pvisor.");
    for (_, manifest) in extensions::discover()? {
        command = command.subcommand(clap::Command::new(manifest.name).about(manifest.description));
    }
    Ok(command)
}

fn normalize_default_run(mut args: Vec<OsString>) -> Vec<OsString> {
    if let Some(first) = args.get(1).and_then(|arg| arg.to_str())
        && !extensions::BUILTINS.contains(&first)
        && ![
            "cache",
            "memory-pool",
            "tui",
            "replay",
            "env",
            "ir",
            "trace",
            "review",
            "checkpoint",
            "job",
            "--help",
            "-h",
            "--version",
            "-V",
        ]
        .contains(&first)
    {
        args.insert(1, "run".into());
    }
    args
}

pub fn main() -> anyhow::Result<()> {
    if crate::run_krun_internal_if_requested()? {
        return Ok(());
    }
    match crate::sandbox::run_internal_if_requested() {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(error) => {
            eprintln!("pVisor local sandbox setup failed: {error:#}");
            std::process::exit(crate::sandbox::SANDBOX_SETUP_EXIT_CODE);
        }
    }
    let args: Vec<OsString> = std::env::args_os().collect();
    if let Some(name) = args.get(1).and_then(|arg| arg.to_str()) {
        if !extensions::BUILTINS.contains(&name)
            && let Some((path, _)) = extensions::find(name)?
        {
            return extensions::execute(path, &args[2..]);
        }
        if name == "help"
            && let Some(target) = args.get(2).and_then(|arg| arg.to_str())
            && !extensions::BUILTINS.contains(&target)
        {
            return extensions::dispatch(target, &["--help".into()]);
        }
    }
    let args = normalize_default_run(args);
    terminal::init_child_context();
    if args.len() == 1 {
        root_command()?.print_long_help()?;
        println!();
        return Ok(());
    }
    let command = if args
        .get(1)
        .is_some_and(|arg| arg == "--help" || arg == "-h" || arg == "help")
    {
        root_command()?
    } else {
        Cli::command()
    };
    let parsed = Cli::from_arg_matches(&command.get_matches_from(args.clone()))?;
    match parsed.command {
        Command::Run(run) => {
            if !terminal::is_child() {
                let audit = run.audit_requested()?;
                if run.tui_requested() || audit {
                    anyhow::ensure!(
                        run.wants_tui(audit),
                        "--tui/--ask requires inherited stdio and a normal Job"
                    );
                    anyhow::ensure!(
                        terminal::available(),
                        "--tui/--ask requires an interactive terminal"
                    );
                    return extensions::dispatch("tui", &args[1..]);
                }
            }
            finish(tokio::runtime::Runtime::new()?.block_on(run::run(*run))?);
        }
        Command::Fork(args) => finish(tokio::runtime::Runtime::new()?.block_on(run::fork(args))?),
        Command::Apply(args) => runtime::apply(args)?,
        Command::Drop(args) => runtime::drop_overlay(args)?,
        Command::Status(args) => runtime::status(args)?,
        Command::Kill(args) => runtime::kill(args)?,
        Command::Inspect(args) => finish(runtime::inspect(args)?),
        Command::Extensions => println!(
            "{}",
            serde_json::to_string_pretty(
                &extensions::discover()?
                    .into_iter()
                    .map(|(path, manifest)| serde_json::json!({"path":path,"name":manifest.name,"description":manifest.description}))
                    .collect::<Vec<_>>()
            )?
        ),
        Command::External(args) => extensions::dispatch(
            args[0]
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("extension name must be UTF-8"))?,
            &args[1..],
        )?,
    }
    Ok(())
}

fn finish(code: i32) {
    if code != 0 {
        std::process::exit(code);
    }
}
