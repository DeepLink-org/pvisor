//! Job lifecycle commands and dispatch to first-party companions.
mod apply;
mod cache;
mod checkpoint;
mod daemon;
mod diff;
mod docker;
mod features;
mod handoff;
#[cfg(unix)]
pub(crate) mod host;
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
mod stage;
mod stage_source;
#[cfg(unix)]
pub mod terminal;
mod trajectory;

pub use checkpoint::ResumeArgs;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
pub use run::RunArgs;
use std::ffi::OsString;

#[derive(Debug, Parser)]
#[command(
    name = "pvisor",
    version,
    about = "Run containers and agents in independent environments"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Enable a runtime experiment (repeatable, comma-separated).
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
    #[command(flatten)]
    Container(docker::DockerCommand),
    /// List runtime experiments.
    Feature(crate::cli::features::FeatureArgs),
    /// Build, publish and query image, rootfs and workspace caches.
    Cache(cache::CacheArgs),
    #[command(about = run::RUN_COMMAND_ABOUT, long_about = run::RUN_COMMAND_LONG_ABOUT)]
    Spawn(Box<run::RunArgs>),
    /// Run a command with all host-root filesystem changes staged for review (Linux only).
    #[command(long_about = stage::STAGE_LONG_ABOUT)]
    Stage(stage::StageArgs),

    /// Apply selected changes, preview them, or pick paths in a terminal.
    #[command(
        long_about = "Apply selected staged changes. SOURCE is required: a Job id, stage directory, or workspace selector (including last). Upper directories and internal stage paths are not apply sources.\n\nExplicit --path, --include or --all executes without prompting. --all permits --exclude, but not --path or --include. Exclusions alone require --interactive. With no selection, a picker opens only when both stdin and stdout are terminals; otherwise selection is required.\n\nUse --dry-run for a read-only plan, optionally with --diff for bounded text diffs. --json requires an explicit positive selection and never opens a picker. --interactive conflicts with --dry-run and --json.\n\nOrdinary apply is project-scoped. --rootfs explicitly requests privileged system changes; it is not an atomic system upgrade. Exit codes: 0 success, 1 execution/service error, 2 arguments, 3 blocked, 130 quit."
    )]
    Apply(runtime::ApplyArgs),
    /// Discard staged changes.
    Drop(runtime::SelectArgs),
    /// Show Job status and execution evidence.
    Status(runtime::StatusCommandArgs),
    /// Review staged changes and execution evidence.
    Review(product::ReviewArgs),
    /// Export selected project results without applying them (Linux only).
    #[command(long_about = handoff::EXPORT_LONG_ABOUT)]
    Export(handoff::ExportArgs),
    /// Manage Job checkpoints.
    Checkpoint(checkpoint::CheckpointArgs),
    /// Suspend a Job or pause a live VM.
    Suspend(checkpoint::SuspendCommandArgs),
    /// Resume a suspended Job or live VM.
    Resume(checkpoint::ResumeCommandArgs),
    /// Stop a running Job.
    Kill(runtime::KillArgs),
    /// Branch a Job from staged files or a checkpoint.
    Fork(run::ForkArgs),
    /// Inspect a Job's filesystem.
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
        .after_help("Examples:\n  pvisor run --rm alpine:3.20 echo hello\n  pvisor spawn --safe -- claude\n  pvisor stage -it -- claude\n  pvisor review last\n  pvisor apply last --path src\n\nUse pvisor -- COMMAND for default Job execution (spawn).\nUse pvisor help COMMAND for command details.")
        .help_template("{about}\n\n{usage-heading} {usage}\n\n{before-help}Options:\n{options}{after-help}\n"))
}

/// Group registered command descriptions, including installed companions,
/// for help display without modifying the parser.
fn grouped_commands(command: &clap::Command) -> String {
    const GROUPS: &[(&str, &[&str])] = &[
        (
            "Commands",
            &[
                "run", "build", "pull", "images", "rmi", "exec", "ps", "start", "stop", "rm",
                "logs", "network",
            ],
        ),
        (
            "Agent Commands",
            &[
                "spawn",
                "stage",
                "status",
                "kill",
                "review",
                "export",
                "apply",
                "drop",
                "inspect",
                "checkpoint",
                "suspend",
                "resume",
                "fork",
            ],
        ),
        (
            "Tools",
            &["daemon", "cache", "replay", "tui", "feature", "help"],
        ),
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
            let about = match name {
                "spawn" => "Run an agent or command as a Job".to_owned(),
                _ => sub.get_about().map(ToString::to_string).unwrap_or_default(),
            };
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

fn normalize_default_spawn(mut args: Vec<OsString>, command: &clap::Command) -> Vec<OsString> {
    let offset = command_offset(&args);
    if let Some(first) = args.get(offset).and_then(|arg| arg.to_str())
        && command.find_subcommand(first).is_none()
        && !companions::is_root_command(first)
        && !["ctrl", "service", "help"].contains(&first)
        && !["--help", "-h", "--version", "-V"].contains(&first)
    {
        args.insert(offset, "spawn".into());
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
    let startup_entry = (std::env::var("PVISOR_STARTUP_PROFILE").as_deref() == Ok("1"))
        .then(std::time::Instant::now);
    if pvisor::docker::internal_if_requested()? {
        return Ok(());
    }
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

    if pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    let args: Vec<OsString> = std::env::args_os().collect();
    anyhow::ensure!(
        !args.get(1).is_some_and(|arg| arg == "ctrl")
            && !(args.get(1).is_some_and(|arg| arg == "help")
                && args.get(2).is_some_and(|arg| arg == "ctrl")),
        "`pvisor ctrl` has been retired; use status JOB, suspend JOB --vm-pause/--vm-offload, or resume JOB"
    );
    let core_command = Cli::command();
    let offset = command_offset(&args);
    anyhow::ensure!(
        !args.get(offset).is_some_and(|arg| arg == "service")
            && !(args.get(offset).is_some_and(|arg| arg == "help")
                && args.get(offset + 1).is_some_and(|arg| arg == "service")),
        "`pvisor service` has been retired; invoke pvisor-daemon directly; the daemon manages shared services and the memory pool"
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
            "--feature enables apply to spawn, not extensions; use pvisor help COMMAND for help"
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
    let args = normalize_default_spawn(args, &core_command);
    if args.len() == 1 {
        root_command()?.print_help()?;
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
    let matches = match command.try_get_matches_from(args.clone()) {
        Ok(matches) => matches,
        Err(error) => {
            let offset = command_offset(&args);
            if error.use_stderr()
                && args.get(offset).is_some_and(|arg| arg == "apply")
                && args[offset + 1..].iter().any(|arg| arg == "--json")
            {
                apply::render_error(
                    true,
                    args[offset + 1..].iter().any(|arg| arg == "--dry-run"),
                    &error.to_string(),
                );
                finish(2);
                return Ok(());
            }
            if error.use_stderr() && args.get(offset).is_some_and(|arg| arg == "run") {
                error.print()?;
                finish(125);
                return Ok(());
            }
            error.exit();
        }
    };
    let mut parsed = Cli::from_arg_matches(&matches)?;
    parsed.command = match parsed.command {
        Command::Stage(args) => Command::Spawn(Box::new(args.into_run()?)),
        command => command,
    };
    if let Command::Feature(query) = &parsed.command {
        return query.print(&parsed.features);
    }
    if let Command::Container(command) = parsed.command {
        anyhow::ensure!(
            parsed.features.is_empty(),
            "--feature enables apply only to spawn, stage or feature queries"
        );
        let failure_code = command.failure_exit_code();
        match docker::run(command) {
            Ok(code) => finish(code),
            Err(error) => {
                eprintln!("pvisor: {error:#}");
                finish(failure_code);
            }
        }
        return Ok(());
    }
    // Informational invocations return without emitting execution diagnostics.
    if let Some(entry) = startup_entry {
        let duration = entry.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let record = serde_json::json!({
            "schema": 1, "pid": std::process::id(), "role": "frontend",
            "clock": "host_monotonic_process_relative", "epoch": "cli-entry",
            "stage": "cli.entry_to_parsed", "start_ns": 0, "end_ns": duration,
            "duration_ns": duration, "outcome": "ok",
        });
        pvisor::diagnostics::diagnostic(format_args!("pvisor-startup-profile {record}"));
    }
    pvisor::startup_mark("process.entry");
    pvisor::startup_mark("cli.parsed");
    if let Command::Spawn(run) = &mut parsed.command {
        run.features = parsed.features.clone();
    } else {
        anyhow::ensure!(
            parsed.features.is_empty(),
            "--feature enables apply only to spawn, stage or feature queries"
        );
    }
    if let Some(command) = host::vm_request(&parsed.command)? {
        finish(host_service::call(command)?);
        return Ok(());
    }
    match parsed.command {
        Command::Container(_) => {
            unreachable!("container commands dispatch before Job startup")
        }
        Command::Feature(_) => unreachable!("feature queries return without contacting Host"),
        Command::Stage(_) => {
            unreachable!("stage converts to spawn before feature and owner dispatch")
        }
        Command::Cache(args) => cache::run(args)?,
        Command::Spawn(run) => {
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
        Command::Apply(args) => finish(apply::run(args)),
        Command::Drop(args) => finish(host_service::call(host::JobCommand::Drop(args))?),
        Command::Status(args) => finish(host_service::call(host::JobCommand::Status(args.args))?),
        Command::Review(args) => finish(host_service::call(host::JobCommand::Review(args))?),
        Command::Export(args) => handoff::export(args)?,
        Command::Checkpoint(args) => {
            finish(host_service::call(host::JobCommand::Checkpoint(args))?)
        }
        Command::Suspend(args) => finish(host_service::call(host::JobCommand::Suspend(args.args))?),
        Command::Resume(resume) => {
            let resume = resume.args;
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
        let command = Cli::command();
        let args = normalize_default_spawn(args.iter().map(OsString::from).collect(), &command);
        Cli::from_arg_matches(&command.try_get_matches_from(args).unwrap()).unwrap()
    }
    #[test]
    fn unbuilt_parser_preserves_informational_and_job_commands() {
        for (args, expected) in [
            (
                vec!["pvisor", "--help"],
                clap::error::ErrorKind::DisplayHelp,
            ),
            (
                vec!["pvisor", "help", "status"],
                clap::error::ErrorKind::DisplayHelp,
            ),
            (
                vec!["pvisor", "help", "review"],
                clap::error::ErrorKind::DisplayHelp,
            ),
            (
                vec!["pvisor", "--version"],
                clap::error::ErrorKind::DisplayVersion,
            ),
        ] {
            let command = Cli::command();
            let args =
                normalize_default_spawn(args.into_iter().map(OsString::from).collect(), &command);
            assert_eq!(
                command.try_get_matches_from(args).unwrap_err().kind(),
                expected
            );
        }
        assert!(matches!(
            parse(&["pvisor", "status", "last", "--json"]).command,
            Command::Status(_)
        ));
        assert!(matches!(
            parse(&["pvisor", "review", "last", "--json"]).command,
            Command::Review(_)
        ));
    }
    #[test]
    fn container_commands_precede_default_spawn_and_keep_workload_flags_literal() {
        for args in [
            vec!["pvisor", "run", "alpine", "echo", "--feature", "guest-only"],
            vec!["pvisor", "pull", "alpine"],
            vec![
                "pvisor",
                "exec",
                "container",
                "echo",
                "--feature",
                "guest-only",
            ],
            vec!["pvisor", "images"],
            vec!["pvisor", "rmi", "alpine"],
            vec!["pvisor", "ps"],
            vec!["pvisor", "start", "container"],
            vec!["pvisor", "stop", "container"],
            vec!["pvisor", "rm", "container"],
            vec!["pvisor", "logs", "container"],
        ] {
            let parsed = parse(&args);
            assert!(matches!(parsed.command, Command::Container(_)), "{args:?}");
            assert!(parsed.features.is_empty(), "{args:?}");
        }
        for args in [
            vec!["pvisor", "/bin/echo", "--rm"],
            vec!["pvisor", "--", "run"],
            vec!["pvisor", "spawn", "--", "pull"],
        ] {
            assert!(
                matches!(parse(&args).command, Command::Spawn(_)),
                "{args:?}"
            );
        }
    }
    #[test]
    fn stage_is_an_agent_command_immediately_after_spawn() {
        let mut command = Cli::command();
        command.build();
        let groups = grouped_commands(&command);
        let agents = groups.split("Agent Commands:\n").nth(1).unwrap();
        let names: Vec<_> = agents
            .lines()
            .take_while(|line| !line.is_empty())
            .map(|line| line.split_whitespace().next().unwrap())
            .collect();
        assert_eq!(&names[..3], &["spawn", "stage", "status"]);
        let containers = groups.split("Agent Commands:\n").next().unwrap();
        assert!(
            !containers
                .lines()
                .any(|line| line.split_whitespace().next() == Some("stage"))
        );
        let help = root_command().unwrap().render_help().to_string();
        assert!(help.contains("pvisor stage -it -- claude"));
    }

    #[test]
    fn stage_is_recognized_without_changing_default_spawn() {
        for args in [
            vec!["pvisor", "stage"],
            vec!["pvisor", "stage", "-it", "sh", "-c", "true"],
        ] {
            assert!(
                matches!(parse(&args).command, Command::Stage(_)),
                "{args:?}"
            );
        }
        for args in [
            vec!["pvisor", "--", "stage", "--rm"],
            vec!["pvisor", "spawn", "--", "stage"],
            vec!["pvisor", "echo", "stage"],
        ] {
            assert!(
                matches!(parse(&args).command, Command::Spawn(_)),
                "{args:?}"
            );
        }
    }

    #[test]
    fn stage_features_stop_at_the_workload_boundary() {
        for args in [
            vec![
                "pvisor",
                "--feature",
                "workload-aware-memory-offloading",
                "stage",
                "echo",
                "--feature",
                "guest-only",
            ],
            vec![
                "pvisor",
                "stage",
                "--feature=workload-aware-memory-offloading",
                "echo",
                "--feature",
                "guest-only",
            ],
            vec![
                "pvisor",
                "--feature",
                "workload-aware-memory-offloading",
                "stage",
                "--",
                "echo",
                "--feature",
                "guest-only",
            ],
        ] {
            let parsed = parse(&args);
            assert_eq!(
                parsed.features,
                [pvisor::features::Feature::WorkloadAwareMemoryOffloading]
            );
            let Command::Stage(stage) = parsed.command else {
                panic!("stage")
            };
            assert_eq!(stage.command, ["echo", "--feature", "guest-only"]);
        }
        let parsed = parse(&["pvisor", "stage", "echo", "--feature", "guest-only"]);
        assert!(parsed.features.is_empty());
        let Command::Stage(stage) = parsed.command else {
            panic!("stage")
        };
        assert_eq!(stage.command, ["echo", "--feature", "guest-only"]);
        let parsed = parse(&[
            "pvisor",
            "--feature",
            "workload-aware-memory-offloading",
            "stage",
            "--feature",
            "workload-aware-memory-offloading",
            "--",
            "true",
        ]);
        assert_eq!(parsed.features.len(), 2);
        assert!(matches!(parsed.command, Command::Stage(_)));
    }

    #[test]
    fn root_enables_work_before_after_and_with_implicit_spawn() {
        for args in [
            vec![
                "pvisor",
                "--feature",
                "workload-aware-memory-offloading",
                "spawn",
                "--executor",
                "vm",
                "--",
                "true",
            ],
            vec![
                "pvisor",
                "spawn",
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
            assert!(matches!(parsed.command, Command::Spawn(_)));
            assert_eq!(
                parsed.features,
                [pvisor::features::Feature::WorkloadAwareMemoryOffloading]
            );
        }
    }
    #[test]
    fn old_feature_name_is_rejected_without_an_alias() {
        let command = Cli::command();
        for args in [
            vec!["pvisor", "--feature", "vm-vcpu-observe", "feature"],
            vec!["pvisor", "spawn", "--feature=vm-vcpu-observe", "--", "true"],
            vec!["pvisor", "--feature", "vm-vcpu-observe", "--", "true"],
        ] {
            let normalized =
                normalize_default_spawn(args.iter().map(OsString::from).collect(), &command);
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
            "spawn",
            "--feature",
            "workload-aware-memory-offloading",
            "--",
            "echo",
            "--feature",
            "guest-only",
        ]);
        assert_eq!(parsed.features.len(), 3);
        let Command::Spawn(run) = &mut parsed.command else {
            panic!("spawn")
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
        let command = Cli::command();
        for args in [
            vec![
                "pvisor",
                "--feature",
                "workload-aware-memory-offloading",
                "help",
                "spawn",
            ],
            vec![
                "pvisor",
                "spawn",
                "--feature",
                "workload-aware-memory-offloading",
                "--help",
            ],
        ] {
            let normalized =
                normalize_default_spawn(args.iter().map(OsString::from).collect(), &command);
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
            normalize_default_spawn(
                vec!["pvisor".into(), "replay".into(), "--help".into()],
                &command
            )[1],
            "replay"
        );
        assert!(
            command
                .try_get_matches_from(["pvisor", "spawn", "--feature", "unknown", "--", "true"])
                .unwrap_err()
                .to_string()
                .contains("unknown feature 'unknown'")
        );
    }
}
