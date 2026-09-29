//! Standalone `pvisor` command-line frontend.

mod env;
mod product;
mod replay;
mod run;
pub mod runtime;
mod trajectory;
#[cfg(unix)]
mod tui;

use clap::{Parser, Subcommand};

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
    /// Start a Job by replaying an agent-native trajectory, then continue the Agent.
    Replay(Box<replay::ReplayArgs>),
}

pub fn main() -> anyhow::Result<()> {
    #[cfg(unix)]
    tui::init_child_context();
    let args = normalize_default_run(std::env::args_os().collect());
    let parsed = Cli::parse_from(args.clone());
    #[cfg(unix)]
    if let Command::Run(run) = &parsed.command
        && !tui::is_child()
    {
        let audit = run.audit_requested()?;
        if run.tui_requested() || audit {
            anyhow::ensure!(
                run.wants_tui(audit),
                "--tui/--ask requires inherited stdio and a normal Job"
            );
            anyhow::ensure!(
                tui::available(),
                "--tui/--ask requires an interactive terminal"
            );
            let code = tui::run(args, audit)?;
            if code != 0 {
                std::process::exit(code);
            }
            return Ok(());
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
        Command::Replay(args) => {
            let code = replay::run(*args);
            if code != 0 {
                std::process::exit(code);
            }
        }
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
        !reserved.contains(&value) && value != "--help" && value != "-h" && value != "--version"
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
            Cli::try_parse_from(args).expect("valid pvisor command");
        }
    }

    #[test]
    fn replay_modes_are_mutually_exclusive_cli_flags() {
        for mode in ["--prepare-only", "--replay-only"] {
            Cli::try_parse_from([
                "pvisor",
                "replay",
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

        let error = Cli::try_parse_from([
            "pvisor",
            "replay",
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
        let help = Cli::try_parse_from(["pvisor", "replay", "--help"])
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
            "run", "apply", "drop", "status", "kill", "fork", "inspect", "env", "replay",
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
