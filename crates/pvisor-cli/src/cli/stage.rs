//! Docker-style arguments for staged host-root Jobs, without an image selector.
use super::run;
use clap::Args;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub(super) const STAGE_LONG_ABOUT: &str = "Create a persistent coding agent stagebox. By default, run in a Linux VM using host / as a live, read-only lower filesystem, without an image. All rootfs changes are retained for review; workload filesystem writes do not change host files. --stage PATH selects storage; omission also retains the stage. Exit, including nonzero exit, stops the instance without deleting it. Use ps -a to list it, fork ID to create a persistent filesystem-CoW branch, and rm ID to remove it. drop discards pending changes without deleting the instance.\n\nExplicit --executor host or --config uses shared Job options instead of the default host-root VM preset. Host staging remains explicit (--safe or --stage); ordinary Host writes are direct. The default VM has no host fallback and requires Linux. Filesystem fork requires a stopped source and native filesystem CoW; unsupported filesystems fail without a byte-copy fallback. The live host lower is not frozen by fork.\n\nReview project results, then use pvisor export JOB --output NEWDIR --path src (or --all) for non-root project artifact handoff. Inspect manifest.json and its deletions before accepting results; export does not apply changes or include the experimental environment automatically. It is not a complete patch or dependency closure.\n\nNetworked Agent: --network bridge (the compatible default) enables the existing controlled IPv4 TCP/DNS egress, not a model-API-only allowlist. Host files, HOME, project .env and readable credentials remain visible; review credential sources and explicit -e/--env-file inputs. Sensitive reads can leave in model requests. Choose --cpus and --memory for guest resources; staging does not bound external API cost.\n\nOffline script: --network none disables guest networking, but host files and readable credentials remain visible. Review the same reads, environment inputs and resource limits (--cpus/--memory); offline networking is not a confidentiality boundary. The host lower is live, not a point-in-time snapshot.\n\nAPI calls, database writes and sent messages are external side effects that review/drop cannot undo. Staging does not automatically change the Agent's own approval settings; this help does not claim validation of a real Agent workflow.\n\nAdvanced only: rootfs apply is an explicit, separate operation requiring a root-owned stage and effective root. It is not an atomic upgrade or a multi-file transaction.";

#[derive(Clone, Serialize, Deserialize, Args, Debug)]
pub(super) struct StageArgs {
    /// Shared Job policies and executor options; stage always retains its storage.
    #[command(flatten)]
    pub(super) job: run::RunArgs,
    /// Keep stdin open for the workload.
    #[arg(short, long)]
    pub(super) interactive: bool,
    /// Allocate a pseudo-terminal.
    #[arg(short = 't', long)]
    pub(super) tty: bool,
    /// Set KEY=VALUE, or inherit/unset KEY using Docker environment semantics.
    #[arg(short = 'e', long = "env", action = clap::ArgAction::Append)]
    pub(super) env: Vec<String>,
    /// Read environment entries from a file; explicit --env entries take precedence.
    #[arg(long = "env-file", value_name = "PATH")]
    pub(super) env_files: Vec<PathBuf>,
    /// Working directory inside the staged root filesystem.
    #[arg(short = 'w', long = "workdir", value_name = "PATH")]
    pub(super) cwd: Option<PathBuf>,
    /// Native vCPU count (1 through 8); fractional quotas are unsupported.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=8))]
    pub(super) cpus: u8,
    /// Connect to the bridge, or disable network access.
    #[arg(long, default_value = "bridge", value_parser = ["bridge", "none"])]
    pub(super) network: String,
}

impl StageArgs {
    pub(super) fn into_run(self) -> anyhow::Result<run::RunArgs> {
        run::RunArgs::for_stage(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Cli {
        #[command(flatten)]
        args: StageArgs,
    }

    fn parse(args: &[&str]) -> StageArgs {
        Cli::try_parse_from(std::iter::once("stage").chain(args.iter().copied()))
            .unwrap()
            .args
    }

    #[test]
    fn defaults_need_neither_image_nor_command() {
        let args = parse(&[]);
        assert!(!args.interactive);
        assert!(!args.tty);
        assert!(args.env.is_empty());
        assert!(args.env_files.is_empty());
        assert!(args.cwd.is_none());
        assert_eq!(args.cpus, 2);
        assert_eq!(args.network, "bridge");
        let job = serde_json::to_value(&args.job).unwrap();
        assert!(job["run"]["name"].is_null());
        assert!(job["stage"].is_null());
        assert_eq!(job["command"], serde_json::json!([]));
    }

    #[test]
    fn docker_style_options_repeat_and_preserve_literal_workload_flags() {
        let args = parse(&[
            "-it",
            "-e",
            "A=first",
            "--env",
            "A=last",
            "--env-file",
            "first.env",
            "--env-file",
            "last.env",
            "-w",
            "/work",
            "--name",
            "agent",
            "--cpus",
            "8",
            "-m",
            "1g",
            "--network",
            "none",
            "--stage",
            "/stages/agent",
            "sh",
            "-c",
            "echo hello",
            "--env",
            "WORKLOAD=literal",
            "--rm",
        ]);
        assert!(args.interactive && args.tty);
        assert_eq!(args.env, ["A=first", "A=last"]);
        assert_eq!(
            args.env_files,
            [PathBuf::from("first.env"), PathBuf::from("last.env")]
        );
        assert_eq!(args.cwd, Some(PathBuf::from("/work")));
        let job = serde_json::to_value(&args.job).unwrap();
        assert_eq!(job["run"]["name"], "agent");
        assert_eq!(args.cpus, 8);
        assert_eq!(job["run"]["memory"], 1024 * 1024 * 1024);
        assert_eq!(args.network, "none");
        assert_eq!(job["stage"], "/stages/agent");
        assert_eq!(
            args.job.command,
            [
                "sh",
                "-c",
                "echo hello",
                "--env",
                "WORKLOAD=literal",
                "--rm"
            ]
        );
        let value = serde_json::to_value(args.clone()).unwrap();
        let restored: StageArgs = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), value);
    }

    #[test]
    fn explicit_separator_preserves_workload_flags() {
        let args = parse(&["--", "echo", "--cpus", "99", "--network", "host"]);
        assert_eq!(
            args.job.command,
            ["echo", "--cpus", "99", "--network", "host"]
        );
        assert_eq!(args.cpus, 2);
        assert_eq!(args.network, "bridge");
    }

    #[test]
    fn resources_network_and_stage_breaking_options_are_rejected() {
        for args in [
            vec!["--cpus", "0"],
            vec!["--cpus", "9"],
            vec!["--cpus", "1.5"],
            vec!["-m", "bogus"],
            vec!["--network", "host"],
            vec!["--network", "custom"],
            vec!["-v", "/host:/guest"],
            vec!["--volume", "/host:/guest"],
            vec!["-d"],
            vec!["--detach"],
            vec!["--rm"],
        ] {
            assert!(
                Cli::try_parse_from(std::iter::once("stage").chain(args.iter().copied())).is_err(),
                "{args:?}"
            );
        }
        assert_eq!(parse(&["--cpus", "1"]).cpus, 1);
        for size in ["16m", "512M", "1GiB", "536870912"] {
            assert!(Cli::try_parse_from(["stage", "--memory", size]).is_ok());
        }
        assert!(parse(&["-m", "15m"]).into_run().is_err());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn conversion_rejects_unsupported_hosts_without_fallback() {
        let error = parse(&[]).into_run().unwrap_err().to_string();
        assert!(error.contains("requires Linux"));
        assert!(error.contains("no fallback"));
    }
}
