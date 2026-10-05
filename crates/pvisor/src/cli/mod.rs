//! Job lifecycle commands and discovery of independent executable extensions.
mod checkpoint;
mod commands;
pub mod extensions;
mod product;
mod run;
pub mod runtime;
#[cfg(unix)]
pub mod terminal;
mod trajectory;

pub use checkpoint::ResumeArgs;
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
    /// Review a stopped Job's current staged changes and execution evidence.
    Review(product::ReviewArgs),
    /// Manage immutable, Job-scoped checkpoints.
    Checkpoint(checkpoint::CheckpointArgs),
    /// Suspend a Job when its executor supports complete execution checkpoints.
    Suspend(checkpoint::SuspendArgs),
    /// Continue a suspended Job when its executor supports full state restoration.
    Resume(checkpoint::ResumeArgs),
    /// Request graceful termination of a live Job.
    Kill(runtime::KillArgs),
    /// Branch a Job from staged files or a VM execution checkpoint.
    Fork(run::ForkArgs),
    /// Open a read-only shell or run a command against a Job filesystem view.
    Inspect(runtime::InspectArgs),
    #[cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
    /// Manage deployments, cluster tasks and shared node resources.
    Service(crate::service::ServiceArgs),
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

fn root_command() -> anyhow::Result<clap::Command> {
    let mut command = Cli::command();
    for (_, manifest) in extensions::discover()? {
        command = command.subcommand(clap::Command::new(manifest.name).about(manifest.description));
    }
    for (order, name) in [
        "run",
        "status",
        "kill",
        "inspect",
        "review",
        "apply",
        "drop",
        "checkpoint",
        "suspend",
        "resume",
        "fork",
        "service",
        "replay",
        "tui",
        "help",
    ]
    .into_iter()
    .enumerate()
    {
        if command.find_subcommand(name).is_some() {
            command = command.mut_subcommand(name, |sub| sub.display_order(order));
        }
    }
    command.build();
    let groups = grouped_commands(&command);
    Ok(command
        .before_help(groups.trim_end().to_owned())
        .after_help("Use pvisor -- COMMAND for default execution. Use pvisor help COMMAND for command details.")
        .help_template("{about}\n\n{usage-heading} {usage}\n\n{before-help}Options:\n{options}{after-help}\n"))
}

/// Display descriptions from the registered commands, including installed
/// companions, while keeping the command syntax and parser unchanged.
fn grouped_commands(command: &clap::Command) -> String {
    const GROUPS: &[(&str, &[&str])] = &[
        (
            "Jobs",
            &[
                "run",
                "status",
                "kill",
                "suspend",
                "resume",
                "fork",
                "checkpoint",
            ],
        ),
        ("Filesystems", &["inspect", "review", "apply", "drop"]),
        ("Extensions", &["service", "replay", "tui"]),
        ("Help", &["help"]),
    ];
    let width = command
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set())
        .map(|sub| sub.get_name().len())
        .max()
        .unwrap_or(0);
    let mut output = String::new();
    for (heading, names) in GROUPS {
        let members: Vec<_> = names
            .iter()
            .filter_map(|name| command.find_subcommand(name))
            .filter(|sub| !sub.is_hide_set())
            .collect();
        if members.is_empty() {
            continue;
        }
        output.push_str(&format!("{heading}:\n"));
        for sub in members {
            let name = sub.get_name();
            let about = sub.get_about().map(ToString::to_string).unwrap_or_default();
            output.push_str(&format!("  {name:width$}  {about}\n"));
        }
        output.push('\n');
    }
    output
}

fn normalize_default_run(mut args: Vec<OsString>) -> Vec<OsString> {
    if let Some(first) = args.get(1).and_then(|arg| arg.to_str())
        && !extensions::BUILTINS.contains(&first)
        && ![
            "cache",
            "memory-pool",
            "cluster",
            "worker",
            "snapshot",
            "tui",
            "replay",
            "env",
            "ir",
            "trace",
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
    crate::diagnostics::init_inherited();
    terminal::init_child_context();
    match crate::sandbox::run_internal_if_requested() {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(error) => {
            eprintln!("pVisor local sandbox setup failed: {error:#}");
            std::process::exit(crate::sandbox::SANDBOX_SETUP_EXIT_CODE);
        }
    }
    // The trusted sandbox launcher is part of the parent's Run, not another
    // CLI invocation. Its cleared workload environment must not create a
    // duplicate startup record or override the parent's logging preference.
    crate::util::startup_mark("process.entry");
    if crate::run_krun_internal_if_requested()? {
        return Ok(());
    }
    let args: Vec<OsString> = std::env::args_os().collect();
    if let Some(name) = args.get(1).and_then(|arg| arg.to_str()) {
        reject_retired_command(name);
        #[cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
        if name == "service"
            && let Some(tool) = args.get(2).and_then(|arg| arg.to_str())
            && extensions::is_service_tool(tool)
        {
            return extensions::dispatch(tool, &args[3..]);
        }
        #[cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
        if name == "help"
            && args.get(2).is_some_and(|arg| arg == "service")
            && let Some(tool) = args.get(3).and_then(|arg| arg.to_str())
            && extensions::is_service_tool(tool)
        {
            let mut tool_args = args[4..].to_vec();
            tool_args.push("--help".into());
            return extensions::dispatch(tool, &tool_args);
        }
        if !extensions::BUILTINS.contains(&name)
            && let Some((path, _)) = extensions::find(name)?
        {
            return extensions::execute(path, &args[2..]);
        }
        if name == "help"
            && let Some(target) = args.get(2).and_then(|arg| arg.to_str())
            && !extensions::BUILTINS.contains(&target)
        {
            reject_retired_command(target);
            return extensions::dispatch(target, &["--help".into()]);
        }
    }
    let args = normalize_default_run(args);
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
    crate::util::startup_mark("cli.parsed");
    match parsed.command {
        #[cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
        Command::Service(args) => {
            tokio::runtime::Runtime::new()?.block_on(crate::service::run(args))?
        }
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
            let runtime = tokio::runtime::Runtime::new()?;
            crate::util::startup_mark("cli.runtime_ready");
            finish(runtime.block_on(run::run(*run))?);
        }
        Command::Fork(args) => finish(tokio::runtime::Runtime::new()?.block_on(run::fork(args))?),
        Command::Apply(args) => runtime::apply(args)?,
        Command::Drop(args) => runtime::drop_overlay(args)?,
        Command::Status(args) => runtime::status(args)?,
        Command::Review(args) => product::review(args)?,
        Command::Checkpoint(args) => {
            tokio::runtime::Runtime::new()?.block_on(checkpoint::run(args))?
        }
        Command::Suspend(args) => {
            tokio::runtime::Runtime::new()?.block_on(checkpoint::suspend(args))?
        }
        Command::Resume(resume) => {
            if resume.tui && !terminal::is_child() {
                anyhow::ensure!(
                    terminal::available(),
                    "--tui requires an interactive terminal"
                );
                return extensions::dispatch("tui", &args[1..]);
            }
            finish(tokio::runtime::Runtime::new()?.block_on(checkpoint::resume(resume))?);
        }
        Command::Kill(args) => runtime::kill(args)?,
        Command::Inspect(args) => finish(runtime::inspect(args)?),
        Command::External(args) => extensions::dispatch(
            args[0]
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("extension name must be UTF-8"))?,
            &args[1..],
        )?,
    }
    Ok(())
}

fn reject_retired_command(name: &str) {
    let message = if extensions::is_service_tool(name) {
        format!("`pvisor {name}` was removed; use `pvisor service {name}`")
    } else if name == "extensions" {
        "`pvisor extensions` was removed; use `pvisor --help` to see available commands".into()
    } else if name == "snapshot" {
        "`pvisor snapshot` was removed; use Job-scoped `checkpoint`, `suspend`, `resume` and `fork` with a supported execution profile".into()
    } else {
        return;
    };
    Cli::command()
        .error(clap::error::ErrorKind::InvalidSubcommand, message)
        .exit();
}

fn finish(code: i32) {
    if code != 0 {
        std::process::exit(code);
    }
}
