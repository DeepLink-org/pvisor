//! Standalone `pvisor` command-line frontend.

mod env;
pub mod extensions;
mod product;
mod replay;
mod run;
pub mod runtime;
#[cfg(unix)]
mod terminal;
mod trajectory;
#[cfg(unix)]
mod tui;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use std::ffi::OsString;

#[cfg(target_os = "linux")]
const ROOT_ABOUT: &str =
    "Manage Agent Jobs with independent filesystem, network, and staging policies";
#[cfg(target_os = "linux")]
const ROOT_LONG_ABOUT: &str = "pVisor manages Jobs: `run` starts one, and `status`, `kill`, `inspect`, `fork`, `apply`, and `drop` act on it. `env` supplies reusable environments; `replay` starts a Job from a trajectory.\n\nHost execution preserves the host filesystem view by default. Use `--filesystem sandbox` for synthetic-root/Landlock restrictions, `--stage` for independent workspace staging, and `--overlaynet` for network policy. On Linux, `--overlaynet-deny-all` uses a private network namespace without enabling filesystem restrictions.";

#[cfg(target_os = "macos")]
const ROOT_ABOUT: &str =
    "Manage Agent Jobs with independent filesystem, network, and staging policies";
#[cfg(target_os = "macos")]
const ROOT_LONG_ABOUT: &str = "pVisor manages Jobs: `run` starts one, and `status`, `kill`, `inspect`, `fork`, `apply`, and `drop` act on it. `env` supplies reusable environments; `replay` starts a Job from a trajectory.\n\nHost execution preserves the host filesystem view by default. Use `--filesystem sandbox` for Seatbelt filesystem restrictions, `--stage` for independent workspace staging (macFUSE may be required), and `--overlaynet` for network policy. Full-disk reads remain available and ambient unless filesystem sandboxing is requested. `--overlaynet-deny-all` blocks non-loopback IP and ambient host Unix sockets while retaining loopback proxy access and Job-local IPC.";

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const ROOT_ABOUT: &str = "Manage Agent Jobs with staged, reviewable workspaces";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const ROOT_LONG_ABOUT: &str = "pVisor manages Jobs: `run` starts one; `status`, `kill`, `inspect`, `fork`, `apply`, and `drop` act on it.";

#[derive(Debug, Parser)]
#[command(
    name = "pvisor",
    version,
    about = ROOT_ABOUT,
    long_about = ROOT_LONG_ABOUT
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(
        about = run::RUN_COMMAND_ABOUT,
        long_about = run::RUN_COMMAND_LONG_ABOUT
    )]
    Run(Box<run::RunArgs>),
    /// Serve or query the shared OCI file cache.
    #[cfg(unix)]
    Cache(crate::image::cache::CacheArgs),
    /// Apply selected staged changes from a stopped Job.
    Apply(runtime::ApplyArgs),
    /// Discard staged changes from a stopped Job.
    Drop(runtime::SelectArgs),
    /// Show a Job's process, filesystem, and network status.
    Status(runtime::StatusArgs),
    /// Request graceful termination of a live Job.
    Kill(runtime::KillArgs),
    /// Start a new safe Job from a stopped Job or a logical checkpoint.
    Fork(run::ForkArgs),
    /// Open a read-only shell or run a command against a Job filesystem view.
    Inspect(runtime::InspectArgs),
    /// Manage reusable execution environments for Jobs.
    Env(env::EnvArgs),
    /// List installed executable extensions and their manifests.
    Extensions,
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

pub fn main() -> anyhow::Result<()> {
    let args = normalize_default_run(std::env::args_os().collect());
    if let Some(name) = args.get(1).and_then(|arg| arg.to_str())
        && !extensions::BUILTINS.contains(&name)
        && extensions::find(name)?.is_some()
    {
        return extensions::dispatch(name, &args[2..]);
    }
    #[cfg(unix)]
    terminal::init_child_context();
    let mut command = Cli::command();
    if args
        .get(1)
        .is_some_and(|arg| arg == "--help" || arg == "-h" || arg == "help")
    {
        let installed = extensions::discover()?;
        let help = installed
            .iter()
            .map(|(_, manifest)| format!("  {}  {}", manifest.name, manifest.description))
            .collect::<Vec<_>>()
            .join("\n");
        command = command.after_help(format!("Installed extensions:\n{help}"));
    }
    let parsed = Cli::from_arg_matches(&command.get_matches_from(args.clone()))?;
    #[cfg(unix)]
    if let Command::Run(run) = &parsed.command
        && !terminal::is_child()
    {
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
    match parsed.command {
        #[cfg(unix)]
        Command::Cache(args) => crate::image::cache::run(args)?,
        Command::Run(args) => {
            let code = tokio::runtime::Runtime::new()?.block_on(run::run(*args))?;
            if code != 0 {
                std::process::exit(code);
            }
        }
        Command::Apply(args) => runtime::apply(args)?,
        Command::Drop(args) => runtime::drop_overlay(args)?,
        Command::Status(args) => runtime::status(args)?,
        Command::Kill(args) => runtime::kill(args)?,
        Command::Fork(args) => {
            let code = tokio::runtime::Runtime::new()?.block_on(run::fork(args))?;
            if code != 0 {
                std::process::exit(code);
            }
        }
        Command::Inspect(args) => {
            let code = runtime::inspect(args)?;
            if code != 0 {
                std::process::exit(code);
            }
        }
        Command::Env(args) => {
            let code = env::run(args)?;
            if code != 0 {
                std::process::exit(code);
            }
        }
        Command::Extensions => println!(
            "{}",
            serde_json::to_string_pretty(
                &extensions::discover()?
                    .into_iter()
                    .map(|(path, manifest)| serde_json::json!({"path":path,"manifest":manifest}))
                    .collect::<Vec<_>>()
            )?
        ),
        Command::External(args) => {
            let name = args[0]
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("extension name must be UTF-8"))?;
            extensions::dispatch(name, &args[1..])?;
        }
    }
    Ok(())
}

#[derive(Debug, Parser)]
#[command(
    name = "pvisor-replay",
    version,
    about = "Replay an agent-native trajectory"
)]
pub(crate) struct ReplayCli {
    #[command(flatten)]
    args: replay::ReplayArgs,
}

pub fn replay_main() -> anyhow::Result<()> {
    #[cfg(unix)]
    terminal::init_child_context();
    let code = replay::run(ReplayCli::parse().args);
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

#[cfg(unix)]
pub fn tui_main() -> anyhow::Result<()> {
    #[derive(Parser)]
    #[command(
        name = "pvisor-tui",
        version,
        about = "Run a Job in an interactive terminal"
    )]
    struct TuiCli {
        #[command(flatten)]
        run: run::RunArgs,
    }
    terminal::init_child_context();
    let mut args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|arg| arg == "run") {
        args.remove(1);
    }
    let mut parsed = TuiCli::parse_from(&args);
    parsed.run.enable_tui();
    let audit = parsed.run.audit_requested()?;
    anyhow::ensure!(
        parsed.run.wants_tui(audit),
        "TUI requires inherited stdio and a normal Job"
    );
    anyhow::ensure!(
        terminal::available(),
        "TUI requires an interactive terminal"
    );
    args[0] = extensions::core_executable()?.into_os_string();
    args.insert(1, "run".into());
    let code = tui::run(args, audit)?;
    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

fn normalize_default_run(mut args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    let first = args.get(1).and_then(|value| value.to_str());
    let reserved = [
        "ir",
        "trace",
        "run",
        "replay",
        "tui",
        "extensions",
        "env",
        "cache",
        "status",
        "kill",
        "inspect",
        "review",
        "checkpoint",
        "fork",
        "apply",
        "drop",
        "help",
    ];
    if first.is_some_and(|value| {
        !reserved.contains(&value)
            && extensions::find(value).is_ok_and(|found| found.is_none())
            && value != "--help"
            && value != "-h"
            && value != "--version"
    }) {
        args.insert(1, "run".into());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_cli_is_small_and_run_can_be_explicit() {
        for args in [
            vec!["pvisor", "status"],
            vec!["pvisor", "cache", "serve"],
            vec!["pvisor", "cache", "prepare", "alpine:latest"],
            vec!["pvisor", "cache", "list", "sha256:example"],
            vec!["pvisor", "inspect", "run-1", "--", "rg", "TODO"],
            vec!["pvisor", "status", "run-1", "--review"],
            vec!["pvisor", "kill", "run-1"],
            vec!["pvisor", "fork", "run-1", "--", "codex"],
            vec!["pvisor", "apply", "run-1"],
            vec!["pvisor", "apply", "run-1", "--target", "/tmp/restored"],
            vec!["pvisor", "apply", "run-1", "--path", "src"],
            vec![
                "pvisor",
                "apply",
                "run-1",
                "--include",
                "src/**",
                "--exclude",
                "src/generated/**",
            ],
            vec!["pvisor", "drop", "run-1"],
            vec!["pvisor", "env", "create", "demo", "--target", "/tmp"],
            vec!["pvisor", "env", "exec", "demo", "--", "/bin/true"],
            vec!["pvisor", "env", "shell", "demo"],
            vec!["pvisor", "env", "list"],
            vec!["pvisor", "env", "status", "demo"],
            vec!["pvisor", "env", "delete", "demo", "--force"],
            vec!["pvisor", "run", "--", "/usr/bin/true"],
            vec!["pvisor", "run", "/usr/bin/true"],
            vec![
                "pvisor",
                "replay",
                "--agent",
                "claude-code",
                "--trajectory",
                "/input/session.jsonl",
                "--after-step",
                "30",
            ],
            vec![
                "pvisor",
                "replay",
                "--agent",
                "claude-code",
                "--trajectory",
                "/input/session.jsonl",
                "--after-step",
                "30",
                "--boundary-user-prompt",
                "Review the fresh observation.",
                "--agent-entrypoint",
                "/usr/bin/claude",
                "--overlayfs-path",
                "/workspace",
            ],
        ] {
            if args.get(1) == Some(&"replay") {
                ReplayCli::try_parse_from(std::iter::once(args[0]).chain(args.into_iter().skip(2)))
                    .expect("valid replay command");
            } else {
                Cli::try_parse_from(args).expect("valid pvisor command");
            }
        }
    }

    #[test]
    fn replay_modes_are_mutually_exclusive_cli_flags() {
        for mode in ["--prepare-only", "--replay-only"] {
            ReplayCli::try_parse_from([
                "pvisor-replay",
                "--agent",
                "claude-code",
                "--trajectory",
                "/input/session.jsonl",
                "--after-step",
                "1",
                mode,
            ])
            .expect("individual replay mode flag must be accepted");
        }

        let error = ReplayCli::try_parse_from([
            "pvisor-replay",
            "--agent",
            "claude-code",
            "--trajectory",
            "/input/session.jsonl",
            "--after-step",
            "1",
            "--prepare-only",
            "--replay-only",
        ])
        .unwrap_err();
        assert!(error.to_string().contains("cannot be used with"));
    }

    #[test]
    fn replay_help_describes_phase_modes() {
        let help = ReplayCli::try_parse_from(["pvisor-replay", "--help"])
            .unwrap_err()
            .to_string();

        assert!(help.contains("--prepare-only"));
        assert!(help.contains("without executing tools or starting an Agent"));
        assert!(help.contains("--replay-only"));
        assert!(help.contains("stop before the next model request"));
        assert!(help.contains("--allow-stale-observations"));
        assert!(help.contains("--boundary-user-prompt"));
        assert!(help.contains("after the replayed boundary observation"));
        assert!(help.contains("including the replayed prefix and any live continuation"));
    }

    #[test]
    fn cache_is_not_rewritten_to_run() {
        let args = normalize_default_run(vec!["pvisor".into(), "cache".into(), "serve".into()]);
        assert_eq!(args[1], "cache");
        Cli::try_parse_from(args).unwrap();
    }

    #[test]
    fn unknown_first_token_becomes_default_run() {
        let args = normalize_default_run(vec!["pvisor".into(), "/bin/true".into()]);
        assert_eq!(args[1], "run");
    }

    #[test]
    fn root_help_names_the_effective_platform_boundary() {
        let help = Cli::try_parse_from(["pvisor", "--help"])
            .unwrap_err()
            .to_string();

        for command in [
            "run",
            "apply",
            "drop",
            "status",
            "kill",
            "fork",
            "inspect",
            "env",
            "extensions",
        ] {
            assert!(help.contains(&format!("\n  {command} ")));
        }
        for removed in ["review", "checkpoint", "trace", "job"] {
            assert!(!help.contains(&format!("\n  {removed} ")));
        }

        #[cfg(target_os = "linux")]
        {
            let normalized = help.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(normalized.contains("host filesystem view by default"));
            assert!(normalized.contains("--filesystem sandbox"));
            assert!(normalized.contains("namespace"));
            assert!(normalized.contains("Landlock"));
        }
        #[cfg(target_os = "macos")]
        {
            assert!(help.contains("macFUSE"));
            assert!(help.contains("Seatbelt"));
            assert!(help.contains("Full-disk reads remain available"));
        }
    }
}
