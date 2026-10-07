//! Job lifecycle commands and dispatch to first-party companions.
mod cache;
mod checkpoint;
mod commands;
mod features;
#[cfg(unix)]
pub(crate) mod host;
#[cfg(unix)]
mod host_cancel;
#[cfg(unix)]
mod host_fds;
#[cfg(any(target_os = "macos", test))]
mod host_image;
#[cfg(unix)]
mod host_process;
#[cfg(unix)]
mod host_service;
mod values;
use crate::companions;
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
    #[command(flatten)]
    vm: host::VmOptions,
    /// Explicitly enable a runtime experiment (repeatable, comma-separated).
    #[arg(
        long = "feature",
        global = true,
        value_name = "NAME",
        value_delimiter = ','
    )]
    features: Vec<pvisor::features::Feature>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List registered runtime experiments and default/current CLI enable state.
    Feature(crate::cli::features::FeatureArgs),
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
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

fn root_command() -> anyhow::Result<clap::Command> {
    let mut command = Cli::command();
    for (_, manifest) in companions::discover()? {
        command = command.subcommand(clap::Command::new(manifest.name).about(manifest.description));
    }
    command.build();
    let groups = grouped_commands(&command);
    Ok(command
        .before_help(groups.trim_end().to_owned())
        .after_help("Use pvisor -- COMMAND for default execution. Use pvisor help COMMAND for command details.")
        .help_template("{about}\n\n{usage-heading} {usage}\n\n{before-help}Options:\n{options}{after-help}\n"))
}

/// Group registered command descriptions, including installed companions,
/// for help display without modifying the parser.
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
        ("Extensions", &["replay", "tui"]),
        ("Help", &["feature", "help"]),
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

// Only consume leading root feature options. Never scan guest/extension arguments.
fn command_offset(args: &[OsString]) -> usize {
    let mut index = 1;
    while let Some(arg) = args.get(index).and_then(|arg| arg.to_str()) {
        if arg == "--feature" {
            if args.get(index + 1).is_none() {
                break;
            }
            index += 2;
        } else if arg.starts_with("--feature=") {
            index += 1;
        } else {
            break;
        }
    }
    index
}

fn normalize_default_run(mut args: Vec<OsString>, command: &clap::Command) -> Vec<OsString> {
    let offset = command_offset(&args);
    if let Some(first) = args.get(offset).and_then(|arg| arg.to_str())
        && command.find_subcommand(first).is_none()
        && !companions::is_root_command(first)
        && !["ctrl", "service"].contains(&first)
        && !["--help", "-h", "--version", "-V"].contains(&first)
    {
        args.insert(offset, "run".into());
    }
    // clap propagates globals by replacement across parser levels, not append.
    // Put leading enables on the command level so pre/post-command repetitions
    // accumulate together; leave help and companion routing untouched.
    if offset > 1
        && args
            .get(offset)
            .and_then(|arg| arg.to_str())
            .is_some_and(|name| name != "help" && command.find_subcommand(name).is_some())
    {
        let enables: Vec<_> = args.drain(1..offset).collect();
        args.splice(2..2, enables);
    }
    args
}

pub fn main() -> anyhow::Result<()> {
    if host_service::internal_if_requested()? {
        return Ok(());
    }
    pvisor::diagnostics::init_inherited();
    terminal::init_child_context();
    match pvisor::sandbox::run_internal_if_requested() {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(error) => {
            eprintln!("pVisor local sandbox setup failed: {error:#}");
            std::process::exit(pvisor::sandbox::SANDBOX_SETUP_EXIT_CODE);
        }
    }
    // The trusted sandbox launcher is part of the parent's Run, not another
    // CLI invocation. Its cleared workload environment must not create a
    // duplicate startup record or override the parent's logging preference.
    pvisor::startup_mark("process.entry");
    if pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    let args: Vec<OsString> = std::env::args_os().collect();
    anyhow::ensure!(
        !args.get(1).is_some_and(|arg| arg == "ctrl")
            && !(args.get(1).is_some_and(|arg| arg == "help")
                && args.get(2).is_some_and(|arg| arg == "ctrl")),
        "`pvisor ctrl` has been retired; use status, suspend --vm-pause/--vm-offload, or resume --vm-load with --vm-socket, --vm-job-id and --vm-attempt-id"
    );
    let mut core_command = Cli::command();
    core_command.build();
    let offset = command_offset(&args);
    anyhow::ensure!(
        !args.get(offset).is_some_and(|arg| arg == "service")
            && !(args.get(offset).is_some_and(|arg| arg == "help")
                && args.get(offset + 1).is_some_and(|arg| arg == "service")),
        "`pvisor service` has been retired; invoke pvisor-daemon or pvisor-cache directly; the daemon manages shared services and the memory pool"
    );
    let mut routing_args = vec![args[0].clone()];
    routing_args.extend_from_slice(&args[offset..]);
    if offset > 1 {
        // Validate root enables before bypassing clap for companion dispatch.
        let mut index = 1;
        while index < offset {
            let value = if args[index] == "--feature" {
                index += 1;
                args[index]
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("feature name must be UTF-8"))?
            } else {
                args[index]
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("feature name must be UTF-8"))?
                    .strip_prefix("--feature=")
                    .expect("leading feature option")
            };
            for name in value.split(',') {
                name.parse::<pvisor::features::Feature>()
                    .map_err(anyhow::Error::msg)?;
            }
            index += 1;
        }
    }
    if let Some(name) = routing_args.get(1).and_then(|arg| arg.to_str()) {
        let args = &routing_args;
        anyhow::ensure!(
            offset == 1
                || name == "help"
                || args[2..]
                    .iter()
                    .take_while(|arg| *arg != "--")
                    .any(|arg| arg == "--help" || arg == "-h")
                || !companions::is_root_command(name),
            "--feature enables apply to run, not extensions; use pvisor help COMMAND for help"
        );
        if companions::is_root_command(name)
            && let Some((path, _)) = companions::find(name)?
        {
            return companions::execute(path, &args[2..]);
        }
        if name == "help"
            && let Some(target) = args.get(2).and_then(|arg| arg.to_str())
            && companions::is_root_command(target)
        {
            return companions::dispatch(target, &["--help".into()]);
        }
    }
    let args = normalize_default_run(args, &core_command);
    if args.len() == 1 {
        root_command()?.print_long_help()?;
        println!();
        return Ok(());
    }
    let command = if args
        .get(command_offset(&args))
        .is_some_and(|arg| arg == "--help" || arg == "-h" || arg == "help")
    {
        root_command()?
    } else {
        core_command
    };
    let mut parsed = Cli::from_arg_matches(&command.get_matches_from(args.clone()))?;
    pvisor::startup_mark("cli.parsed");
    if let Command::Feature(query) = &parsed.command {
        return query.print(&parsed.features);
    }
    if let Command::Run(run) = &mut parsed.command {
        run.features = parsed.features.clone();
    } else {
        anyhow::ensure!(
            parsed.features.is_empty(),
            "--feature enables apply only to run or feature queries"
        );
    }
    if let Some(command) = parsed.vm.request(&parsed.command)? {
        finish(host_service::call(command)?);
        return Ok(());
    }
    match parsed.command {
        Command::Feature(_) => unreachable!("feature queries return without contacting Host"),
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
                    return companions::dispatch("tui", &args[1..]);
                }
            }
            finish(host_service::call(host::JobCommand::Run(run))?);
        }
        Command::Fork(args) => finish(host_service::call(host::JobCommand::Fork(args))?),
        Command::Apply(args) => finish(host_service::call(host::JobCommand::Apply(args))?),
        Command::Drop(args) => finish(host_service::call(host::JobCommand::Drop(args))?),
        Command::Status(args) => finish(host_service::call(host::JobCommand::Status(args))?),
        Command::Review(args) => finish(host_service::call(host::JobCommand::Review(args))?),
        Command::Checkpoint(args) => {
            finish(host_service::call(host::JobCommand::Checkpoint(args))?)
        }
        Command::Suspend(args) => finish(host_service::call(host::JobCommand::Suspend(args))?),
        Command::Resume(resume) => {
            if resume.tui && !terminal::is_child() {
                anyhow::ensure!(
                    terminal::available(),
                    "--tui requires an interactive terminal"
                );
                return companions::dispatch("tui", &args[1..]);
            }
            finish(host_service::call(host::JobCommand::Resume(resume))?);
        }
        Command::Kill(args) => finish(host_service::call(host::JobCommand::Kill(args))?),
        Command::Inspect(args) => finish(host_service::call(host::JobCommand::Inspect(args))?),
        Command::External(args) => companions::dispatch(
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

#[cfg(test)]
mod feature_parser_tests {
    use super::*;
    fn parse(args: &[&str]) -> Cli {
        let mut command = Cli::command();
        command.build();
        let args = normalize_default_run(args.iter().map(OsString::from).collect(), &command);
        Cli::from_arg_matches(&command.try_get_matches_from(args).unwrap()).unwrap()
    }
    #[test]
    fn root_enables_work_before_after_and_with_implicit_run() {
        for args in [
            vec![
                "pvisor",
                "--feature",
                "workload-aware-memory-offloading",
                "run",
                "--executor",
                "vm",
                "--",
                "true",
            ],
            vec![
                "pvisor",
                "run",
                "--feature=workload-aware-memory-offloading",
                "--executor",
                "vm",
                "--",
                "true",
            ],
            vec![
                "pvisor",
                "--feature",
                "workload-aware-memory-offloading",
                "--executor",
                "vm",
                "--",
                "true",
            ],
            vec![
                "pvisor",
                "--feature",
                "workload-aware-memory-offloading",
                "--",
                "true",
            ],
        ] {
            let parsed = parse(&args);
            assert!(matches!(parsed.command, Command::Run(_)));
            assert_eq!(
                parsed.features,
                [pvisor::features::Feature::WorkloadAwareMemoryOffloading]
            );
        }
    }
    #[test]
    fn old_feature_name_is_rejected_without_an_alias() {
        let mut command = Cli::command();
        command.build();
        for args in [
            vec!["pvisor", "--feature", "vm-vcpu-observe", "feature"],
            vec!["pvisor", "run", "--feature=vm-vcpu-observe", "--", "true"],
            vec!["pvisor", "--feature", "vm-vcpu-observe", "--", "true"],
        ] {
            let normalized =
                normalize_default_run(args.iter().map(OsString::from).collect(), &command);
            let error = command
                .clone()
                .try_get_matches_from(normalized)
                .unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
            assert!(
                error
                    .to_string()
                    .contains("unknown feature 'vm-vcpu-observe'")
            );
        }
    }

    #[test]
    fn repeated_comma_values_and_guest_boundary() {
        let mut parsed = parse(&[
            "pvisor",
            "--feature",
            "workload-aware-memory-offloading,workload-aware-memory-offloading",
            "run",
            "--feature",
            "workload-aware-memory-offloading",
            "--",
            "echo",
            "--feature",
            "guest-only",
        ]);
        assert_eq!(parsed.features.len(), 3);
        let Command::Run(run) = &mut parsed.command else {
            panic!("run")
        };
        run.features = parsed.features;
        let value = serde_json::to_value(host::JobCommand::Run(run.clone())).unwrap();
        assert_eq!(
            value["args"]["features"][0],
            "workload-aware-memory-offloading"
        );
        assert_eq!(
            value["args"]["command"],
            serde_json::json!(["echo", "--feature", "guest-only"])
        );
        let roundtrip: host::JobCommand = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(roundtrip).unwrap(), value);
    }
    #[test]
    fn query_synonyms_json_help_and_extensions_remain_commands() {
        for args in [
            vec!["pvisor", "feature"],
            vec!["pvisor", "feature", "list", "--json"],
            vec![
                "pvisor",
                "--feature",
                "workload-aware-memory-offloading",
                "feature",
                "--json",
            ],
        ] {
            assert!(matches!(parse(&args).command, Command::Feature(_)));
        }
        let mut command = Cli::command();
        command.build();
        for args in [
            vec![
                "pvisor",
                "--feature",
                "workload-aware-memory-offloading",
                "help",
                "run",
            ],
            vec![
                "pvisor",
                "run",
                "--feature",
                "workload-aware-memory-offloading",
                "--help",
            ],
        ] {
            let normalized =
                normalize_default_run(args.iter().map(OsString::from).collect(), &command);
            assert_eq!(
                command
                    .clone()
                    .try_get_matches_from(normalized)
                    .unwrap_err()
                    .kind(),
                clap::error::ErrorKind::DisplayHelp
            );
        }
        assert!(matches!(
            Cli::try_parse_from(["pvisor", "custom-extension", "--feature", "extension-only"])
                .unwrap()
                .command,
            Command::External(_)
        ));
        assert_eq!(
            normalize_default_run(
                vec!["pvisor".into(), "replay".into(), "--help".into()],
                &command
            )[1],
            "replay"
        );
        assert!(
            command
                .try_get_matches_from(["pvisor", "run", "--feature", "unknown", "--", "true"])
                .unwrap_err()
                .to_string()
                .contains("unknown feature 'unknown'")
        );
    }
}
