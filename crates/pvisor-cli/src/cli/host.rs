//! Typed Job requests. CLI syntax is never reconstructed in the service.
//! These DTOs are an internal exact-build/schema protocol, not a stable public
//! Job API. The shared core envelopes and host framing are separate contracts.
use super::{checkpoint, product, run, runtime};
use anyhow::Context;
use clap::Args;
use pvisor_core::host_protocol::{
    AGENTCTL_HOST_VERSION, AgentCtlHostError, AgentCtlHostErrorCode, AgentCtlHostRequest,
    AgentCtlTarget, HostVmCommand,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "args",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(crate) enum JobCommand {
    Run(Box<run::RunArgs>),
    Status(runtime::StatusArgs),
    Kill(runtime::KillArgs),
    Suspend(checkpoint::SuspendArgs),
    Resume(checkpoint::ResumeArgs),
    Fork(run::ForkArgs),
    Checkpoint(checkpoint::CheckpointArgs),
    Inspect(runtime::InspectArgs),
    Review(product::ReviewArgs),
    Apply(runtime::ApplyArgs),
    Drop(runtime::SelectArgs),
    Vm(VmRequest),
}

impl JobCommand {
    pub(super) fn inherits_terminal_input(&self) -> anyhow::Result<bool> {
        match self {
            Self::Run(args) => args.inherits_terminal_input(),
            Self::Inspect(_) => Ok(true),
            Self::Fork(args) if !args.restores_execution() => Ok(true),
            Self::Fork(_) | Self::Resume(_) => {
                let Some(record) = self.selected_record(&std::env::current_dir()?)? else {
                    return Ok(false);
                };
                let Some(job) = pvisor::job_execution::Job::read(&record)? else {
                    return Ok(false);
                };
                let pvisor_core::RunInvocation::Process(process) = &job.spec.invocation;
                Ok(process.stdin == pvisor_core::StdioMode::Inherit)
            }
            _ => Ok(false),
        }
    }
}

#[derive(Debug, Default, Args)]
#[command(next_help_heading = "Live VM")]
pub(super) struct VmOptions {
    /// Host-only live VM endpoint (requires explicit Job and Attempt identities).
    #[arg(long, requires_all = ["vm_job_id", "vm_attempt_id"])]
    pub vm_socket: Option<PathBuf>,
    /// Explicit Job identity served by the live VM endpoint.
    #[arg(long, requires = "vm_socket")]
    pub vm_job_id: Option<String>,
    /// Explicit live Attempt identity; never inferred from the Job.
    #[arg(long, requires = "vm_socket")]
    pub vm_attempt_id: Option<String>,
}

#[derive(Debug, Default, Args)]
#[command(next_help_heading = "Live VM")]
pub(super) struct SuspendVmOptions {
    #[command(flatten)]
    pub identity: VmOptions,
    /// Pause vCPUs rather than creating an execution checkpoint (suspend only).
    #[arg(long, requires = "vm_socket", conflicts_with = "vm_offload")]
    pub vm_pause: bool,
    /// Quiesce and offload live VM RAM (suspend only).
    #[arg(long, requires = "vm_socket")]
    pub vm_offload: bool,
    /// Write offloaded RAM to this host file (requires --vm-offload).
    #[arg(long, requires = "vm_offload")]
    pub vm_ram_file: Option<PathBuf>,
}

#[derive(Debug, Default, Args)]
#[command(next_help_heading = "Live VM")]
pub(super) struct ResumeVmOptions {
    #[command(flatten)]
    pub identity: VmOptions,
    /// Reload/unpause the same live Attempt, not snapshot restoration (resume only).
    #[arg(long, requires = "vm_socket")]
    pub vm_load: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VmRequest {
    pub socket: PathBuf,
    pub job_id: String,
    pub attempt_id: String,
    pub command: HostVmCommand,
}

impl VmOptions {
    fn request(
        &self,
        command: HostVmCommand,
        selector: Option<&Path>,
    ) -> anyhow::Result<Option<JobCommand>> {
        let Some(socket) = &self.vm_socket else {
            return Ok(None);
        };
        let job_id = self
            .vm_job_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("--vm-job-id is required"))?;
        let attempt_id = self
            .vm_attempt_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("--vm-attempt-id is required"))?;
        anyhow::ensure!(
            selector.is_none_or(|selector| selector == std::path::Path::new(&job_id)),
            "live VM command selector must match the explicit --vm-job-id"
        );
        anyhow::ensure!(
            !job_id.trim().is_empty() && !attempt_id.trim().is_empty(),
            "VM identities must not be empty"
        );
        Ok(Some(JobCommand::Vm(VmRequest {
            socket: socket.clone(),
            job_id: job_id.clone(),
            attempt_id: attempt_id.clone(),
            command,
        })))
    }
}

pub(super) fn vm_request(command: &super::Command) -> anyhow::Result<Option<JobCommand>> {
    match command {
        super::Command::Status(args) => args
            .vm
            .request(HostVmCommand::Status, args.args.selector.as_deref()),
        super::Command::Suspend(args) => {
            if args.vm.identity.vm_socket.is_none() {
                return Ok(None);
            }
            let command = if args.vm.vm_pause {
                HostVmCommand::Pause
            } else if args.vm.vm_offload {
                HostVmCommand::Offload {
                    file: args.vm.vm_ram_file.clone(),
                }
            } else {
                anyhow::bail!("live VM suspend requires --vm-pause or --vm-offload");
            };
            args.vm
                .identity
                .request(command, Some(args.args.job_selector()))
        }
        super::Command::Resume(args) => {
            if args.vm.identity.vm_socket.is_none() {
                return Ok(None);
            }
            anyhow::ensure!(args.vm.vm_load, "live VM resume requires --vm-load");
            args.vm
                .identity
                .request(HostVmCommand::Resume, Some(args.args.job_selector()))
        }
        _ => Ok(None),
    }
}

impl JobCommand {
    fn selection(&self) -> Option<(Option<&Path>, &Path)> {
        match self {
            Self::Run(_) | Self::Vm(_) => None,
            Self::Status(a) => Some((a.selector.as_deref(), &a.output_dir)),
            Self::Inspect(a) => Some((a.selector.as_deref(), &a.output_dir)),
            Self::Review(a) => Some((a.selector.as_deref(), &a.output_dir)),
            Self::Kill(a) => Some((Some(&a.selector), &a.output_dir)),
            Self::Apply(a) => Some((Some(&a.selector), &a.output_dir)),
            Self::Drop(a) => Some((Some(&a.selector), &a.output_dir)),
            Self::Fork(a) => {
                let (selector, output) = a.selection();
                Some((Some(selector), output))
            }
            Self::Suspend(a) => Some((Some(&a.selection().job), &a.selection().output_dir)),
            Self::Resume(a) => Some((Some(&a.selection().job), &a.selection().output_dir)),
            Self::Checkpoint(a) => Some((Some(&a.selection().job), &a.selection().output_dir)),
        }
    }
    pub(super) fn selected_record(&self, cwd: &Path) -> anyhow::Result<Option<pvisor::RunRecord>> {
        let Some((selector, output)) = self.selection() else {
            return Ok(None);
        };
        let output = if output.is_absolute() {
            output.to_owned()
        } else {
            cwd.join(output)
        };
        let selector =
            selector.context("service Job selector must be resolved before submission")?;
        anyhow::ensure!(
            selector.is_absolute(),
            "service Job selector must be an absolute durable path"
        );
        Ok(Some(pvisor::resolve_run(Some(selector), &output)?))
    }
    /// Resolve aliases in the originating cwd once. All subsequent reads and
    /// mutations address this durable Job root, never a retargetable `last`.
    pub(super) fn pin_target(&mut self) -> anyhow::Result<Option<AgentCtlTarget>> {
        if let Self::Vm(a) = self {
            let target = AgentCtlTarget {
                job_id: a.job_id.clone(),
                attempt_id: Some(a.attempt_id.clone()),
                generation: None,
            };
            target.validate()?;
            return Ok(Some(target));
        }
        let Some((selector, output)) = self.selection() else {
            return Ok(None);
        };
        let record = pvisor::resolve_run(selector, output)?;
        let root = pvisor::job_execution::Job::read(&record)?
            .map(|job| job.root)
            .unwrap_or_else(|| record.stage_dir());
        let root = std::fs::canonicalize(&root)
            .with_context(|| format!("resolve durable Job root {}", root.display()))?;
        match self {
            Self::Status(a) => a.selector = Some(root),
            Self::Inspect(a) => a.selector = Some(root),
            Self::Review(a) => a.selector = Some(root),
            Self::Kill(a) => a.selector = root,
            Self::Apply(a) => a.selector = root,
            Self::Drop(a) => a.selector = root,
            Self::Fork(a) => a.pin_selection(root),
            Self::Suspend(a) => a.selection_mut().job = root,
            Self::Resume(a) => a.selection_mut().job = root,
            Self::Checkpoint(a) => a.selection_mut().job = root,
            Self::Run(_) | Self::Vm(_) => unreachable!(),
        }
        Ok(Some(AgentCtlTarget {
            job_id: record.run_id,
            attempt_id: record.attempt_id,
            generation: record.overlay.map(|overlay| overlay.generation.to_string()),
        }))
    }
    pub(super) fn validate_target(
        &self,
        target: Option<&AgentCtlTarget>,
        cwd: &Path,
    ) -> anyhow::Result<()> {
        if let Self::Vm(a) = self {
            pvisor::host_transport::validate_host_target(target, &a.job_id, &a.attempt_id)?;
            return Ok(());
        }
        match (self.selected_record(cwd)?, target) {
            (Some(record), Some(target)) => check_target(target, &record),
            (Some(_), None) => Err(AgentCtlHostError::new(
                AgentCtlHostErrorCode::InvalidRequest,
                "resolved Job command requires a target fence",
            )
            .into()),
            (None, None) => Ok(()),
            (None, Some(_)) => Err(AgentCtlHostError::new(
                AgentCtlHostErrorCode::Conflict,
                "new Job command cannot address an existing target",
            )
            .into()),
        }
    }
}

pub(super) fn service_context() -> pvisor::job_service::ServiceContext<'static> {
    pvisor::job_service::ServiceContext {
        expected_target: None,
        check_cancelled: Some(&super::host_service::check_cancelled),
        check_record: Some(&super::host_service::check_record),
    }
}

pub(super) fn lock_selected_job(
    record: &pvisor::RunRecord,
) -> anyhow::Result<Option<(pvisor::job_execution::Job, impl Send)>> {
    pvisor::job_service::lock_selected_job(&service_context(), record)
}
pub(super) fn check_selected_record(
    selected: &pvisor::RunRecord,
    current: &pvisor::RunRecord,
) -> anyhow::Result<()> {
    pvisor::job_service::check_selected_record(selected, current)
}
pub(super) fn check_target(
    target: &AgentCtlTarget,
    record: &pvisor::RunRecord,
) -> anyhow::Result<()> {
    pvisor::job_service::check_target(target, record)
}

pub(crate) fn execute(rt: &tokio::runtime::Runtime, command: JobCommand) -> anyhow::Result<i32> {
    super::host_service::check_cancelled()?;
    match command {
        JobCommand::Run(args) => return rt.block_on(run::run(*args)),
        JobCommand::Fork(args) => return rt.block_on(run::fork(args)),
        JobCommand::Resume(args) => return rt.block_on(checkpoint::resume(args)),
        JobCommand::Inspect(args) => return runtime::inspect(args),
        JobCommand::Status(args) => runtime::status(args)?,
        JobCommand::Kill(args) => runtime::kill(args)?,
        JobCommand::Apply(args) => runtime::apply(args)?,
        JobCommand::Drop(args) => runtime::drop_overlay(args)?,
        JobCommand::Review(args) => product::review(args)?,
        JobCommand::Suspend(args) => rt.block_on(checkpoint::suspend(args))?,
        JobCommand::Checkpoint(args) => rt.block_on(checkpoint::run(args))?,
        JobCommand::Vm(args) => rt.block_on(async {
            let response = pvisor::host_vm_exchange(
                &args.socket,
                &AgentCtlHostRequest {
                    version: AGENTCTL_HOST_VERSION,
                    request_id: uuid::Uuid::new_v4().to_string(),
                    target: Some(AgentCtlTarget {
                        job_id: args.job_id,
                        attempt_id: Some(args.attempt_id),
                        generation: None,
                    }),
                    command: args.command,
                },
            )
            .await?;
            let result = response.result?;
            println!("{}", serde_json::to_string(&result)?);
            Ok::<_, anyhow::Error>(())
        })?,
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[test]
    fn nested_run_args_roundtrip_as_typed_data() {
        let parsed = super::super::Cli::try_parse_from([
            "pvisor",
            "run",
            "--executor",
            "vm",
            "--memory",
            "256MiB",
            "--timeout",
            "2m",
            "--mount",
            "/tmp:read",
            "--access",
            "secret:deny",
            "--vm-ram-dedup=false",
            "--",
            "/bin/echo",
            "literal ; not a shell request",
        ])
        .unwrap();
        let super::super::Command::Run(args) = parsed.command else {
            panic!("expected run");
        };
        let request = JobCommand::Run(args);
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["operation"], "run");
        assert_eq!(value["args"]["command"][1], "literal ; not a shell request");
        let roundtrip: JobCommand = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(roundtrip).unwrap(), value);
    }

    #[test]
    fn terminal_handoff_is_limited_to_execution_with_inherited_input() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("spec.json");
        for stdin in [
            pvisor_core::StdioMode::Inherit,
            pvisor_core::StdioMode::Null,
            pvisor_core::StdioMode::Capture,
        ] {
            let mut spec =
                pvisor_core::RunSpec::process("terminal-classification", "true", "/bin/true");
            let pvisor_core::RunInvocation::Process(process) = &mut spec.invocation;
            process.stdin = stdin;
            std::fs::write(&path, serde_json::to_vec(&spec).unwrap()).unwrap();
            let parsed = super::super::Cli::try_parse_from([
                "pvisor",
                "run",
                "--spec",
                path.to_str().unwrap(),
                "--result-file",
                "result.json",
            ])
            .unwrap();
            let super::super::Command::Run(args) = parsed.command else {
                panic!("expected run");
            };
            assert_eq!(
                JobCommand::Run(args).inherits_terminal_input().unwrap(),
                stdin == pvisor_core::StdioMode::Inherit
            );
        }
        let parsed = super::super::Cli::try_parse_from(["pvisor", "status", "--json"]).unwrap();
        let super::super::Command::Status(args) = parsed.command else {
            panic!("expected status");
        };
        assert!(
            !JobCommand::Status(args.args)
                .inherits_terminal_input()
                .unwrap()
        );
    }

    fn parse_vm(command: &str, selector: Option<&str>, options: &[&str]) -> super::super::Cli {
        let mut args = vec!["pvisor", command];
        args.extend(selector);
        args.extend([
            "--vm-socket",
            "/private/control.sock",
            "--vm-job-id",
            "job-one",
            "--vm-attempt-id",
            "attempt-one",
        ]);
        args.extend_from_slice(options);
        super::super::Cli::try_parse_from(args).unwrap()
    }

    #[test]
    fn vm_flags_are_rejected_outside_their_command() {
        let vm_flags = [
            ("--vm-socket", Some("/private/control.sock")),
            ("--vm-job-id", Some("job-one")),
            ("--vm-attempt-id", Some("attempt-one")),
            ("--vm-pause", None),
            ("--vm-offload", None),
            ("--vm-ram-file", Some("/private/ram")),
            ("--vm-load", None),
        ];
        for (flag, value) in vm_flags {
            for command in [
                "run",
                "kill",
                "inspect",
                "apply",
                "drop",
                "review",
                "fork",
                "checkpoint",
                "feature",
            ] {
                let mut args = vec!["pvisor", command, flag];
                args.extend(value);
                let error = super::super::Cli::try_parse_from(args).unwrap_err();
                assert_eq!(
                    error.kind(),
                    clap::error::ErrorKind::UnknownArgument,
                    "{command} {flag}"
                );
            }
            let mut args = vec!["pvisor", flag];
            args.extend(value);
            args.push("status");
            assert!(
                super::super::Cli::try_parse_from(args.clone()).is_err(),
                "{flag}"
            );
            let mut command = super::super::Cli::command();
            command.build();
            let args = super::super::normalize_default_run(
                args.into_iter().map(Into::into).collect(),
                &command,
            );
            assert!(
                super::super::Cli::try_parse_from(args).is_err(),
                "normalized {flag}"
            );
        }
        for (command, flags) in [
            (
                "status",
                vec!["--vm-pause", "--vm-offload", "--vm-ram-file", "--vm-load"],
            ),
            ("suspend", vec!["--vm-load"]),
            (
                "resume",
                vec!["--vm-pause", "--vm-offload", "--vm-ram-file"],
            ),
        ] {
            for flag in flags {
                let error = super::super::Cli::try_parse_from(["pvisor", command, "job-one", flag])
                    .unwrap_err();
                assert_eq!(
                    error.kind(),
                    clap::error::ErrorKind::UnknownArgument,
                    "{command} {flag}"
                );
            }
        }
    }

    #[test]
    fn vm_dependencies_and_selector_fences_are_preserved() {
        for (command, options) in [
            ("status", vec![]),
            ("suspend", vec!["--vm-pause"]),
            ("suspend", vec!["--vm-offload"]),
            ("resume", vec!["--vm-load"]),
        ] {
            for missing in ["--vm-socket", "--vm-job-id", "--vm-attempt-id"] {
                let mut args = vec!["pvisor", command, "job-one"];
                for (flag, value) in [
                    ("--vm-socket", "/private/control.sock"),
                    ("--vm-job-id", "job-one"),
                    ("--vm-attempt-id", "attempt-one"),
                ] {
                    if flag != missing {
                        args.extend([flag, value]);
                    }
                }
                args.extend_from_slice(&options);
                assert!(
                    super::super::Cli::try_parse_from(args).is_err(),
                    "{command} missing {missing}"
                );
            }
            let parsed = parse_vm(command, Some("another-job"), &options);
            assert!(
                vm_request(&parsed.command)
                    .unwrap_err()
                    .to_string()
                    .contains("selector must match")
            );
            let parsed = parse_vm(command, Some("job-one"), &options);
            let mut request = vm_request(&parsed.command).unwrap().unwrap();
            let target = request.pin_target().unwrap().unwrap();
            assert_eq!(target.job_id, "job-one");
            assert_eq!(target.attempt_id.as_deref(), Some("attempt-one"));
            request
                .validate_target(Some(&target), Path::new("/"))
                .unwrap();
            assert!(request.validate_target(None, Path::new("/")).is_err());
            let mut wrong = target.clone();
            wrong.attempt_id = Some("another-attempt".into());
            assert!(
                request
                    .validate_target(Some(&wrong), Path::new("/"))
                    .is_err()
            );
        }
        for identity in ["--vm-job-id", "--vm-attempt-id"] {
            let mut args = vec!["pvisor", "status", "--vm-socket", "/private/control.sock"];
            for (flag, value) in [
                ("--vm-job-id", "job-one"),
                ("--vm-attempt-id", "attempt-one"),
            ] {
                args.extend([flag, if flag == identity { "   " } else { value }]);
            }
            let parsed = super::super::Cli::try_parse_from(args).unwrap();
            assert!(
                vm_request(&parsed.command)
                    .unwrap_err()
                    .to_string()
                    .contains("must not be empty")
            );
        }
        for (command, options) in [("suspend", vec![]), ("resume", vec![])] {
            let parsed = parse_vm(command, Some("job-one"), &options);
            assert!(vm_request(&parsed.command).is_err());
        }
        for options in [
            vec!["--vm-pause", "--vm-offload"],
            vec!["--vm-ram-file", "/private/ram"],
        ] {
            let mut args = vec![
                "pvisor",
                "suspend",
                "job-one",
                "--vm-socket",
                "/private/control.sock",
                "--vm-job-id",
                "job-one",
                "--vm-attempt-id",
                "attempt-one",
            ];
            args.extend(options);
            assert!(super::super::Cli::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn vm_help_is_local_documented_and_non_global() {
        use clap::CommandFactory;
        let mut root = super::super::Cli::command();
        root.build();
        assert!(!root.render_long_help().to_string().contains("--vm-"));
        for command in ["status", "suspend", "resume"] {
            let subcommand = root.find_subcommand_mut(command).unwrap();
            let help = subcommand.render_long_help().to_string();
            assert!(help.contains("Live VM:"), "{help}");
            for arg in subcommand
                .get_arguments()
                .filter(|arg| arg.get_id().as_str().starts_with("vm_"))
            {
                assert!(!arg.is_global_set(), "{command}: {}", arg.get_id());
                assert!(arg.get_help().is_some(), "{command}: {}", arg.get_id());
                assert_eq!(arg.get_help_heading(), Some("Live VM"));
            }
            for flag in ["--vm-socket", "--vm-job-id", "--vm-attempt-id"] {
                assert!(help.contains(flag), "{help}");
            }
            for flag in ["--vm-pause", "--vm-offload", "--vm-ram-file"] {
                assert_eq!(help.contains(flag), command == "suspend", "{help}");
            }
            assert_eq!(help.contains("--vm-load"), command == "resume", "{help}");
        }
        for command in [
            "run",
            "kill",
            "inspect",
            "apply",
            "drop",
            "review",
            "fork",
            "checkpoint",
            "feature",
        ] {
            let help = root
                .find_subcommand_mut(command)
                .unwrap()
                .render_long_help()
                .to_string();
            for flag in [
                "--vm-socket",
                "--vm-job-id",
                "--vm-attempt-id",
                "--vm-pause",
                "--vm-offload",
                "--vm-ram-file",
                "--vm-load",
            ] {
                assert!(!help.contains(flag), "{command}: {flag}");
            }
        }
    }

    #[test]
    fn vm_wrappers_leave_host_dtos_unchanged() {
        let parsed = parse_vm("status", None, &[]);
        assert!(matches!(
            vm_request(&parsed.command).unwrap(),
            Some(JobCommand::Vm(_))
        ));
        let super::super::Command::Status(args) = parsed.command else {
            unreachable!()
        };
        let value = serde_json::to_value(JobCommand::Status(args.args)).unwrap();
        assert!(!value.to_string().contains("vm_"));
        let _: JobCommand = serde_json::from_value(value).unwrap();
        let parsed = parse_vm(
            "suspend",
            Some("job-one"),
            &["--vm-offload", "--vm-ram-file", "/private/ram"],
        );
        let JobCommand::Vm(request) = vm_request(&parsed.command).unwrap().unwrap() else {
            unreachable!()
        };
        assert!(
            matches!(request.command, HostVmCommand::Offload { file: Some(ref file) } if file == Path::new("/private/ram"))
        );
        let super::super::Command::Suspend(args) = parsed.command else {
            unreachable!()
        };
        let value = serde_json::to_value(JobCommand::Suspend(args.args)).unwrap();
        assert!(!value.to_string().contains("vm_"));
        let _: JobCommand = serde_json::from_value(value).unwrap();
        let parsed = parse_vm("resume", Some("job-one"), &["--vm-load"]);
        let super::super::Command::Resume(args) = parsed.command else {
            unreachable!()
        };
        let value = serde_json::to_value(JobCommand::Resume(args.args)).unwrap();
        assert!(!value.to_string().contains("vm_"));
        let _: JobCommand = serde_json::from_value(value).unwrap();
    }

    #[test]
    fn vm_controls_are_normal_command_options() {
        for (command, option, action) in [
            ("status", None, "Status"),
            ("suspend", Some("--vm-pause"), "Pause"),
            ("suspend", Some("--vm-offload"), "Offload { file: None }"),
            ("resume", Some("--vm-load"), "Resume"),
        ] {
            let mut args = vec![
                "pvisor",
                command,
                "job-one",
                "--vm-socket",
                "/private/control.sock",
                "--vm-job-id",
                "job-one",
                "--vm-attempt-id",
                "attempt-one",
            ];
            if let Some(option) = option {
                args.push(option);
            }
            let parsed = super::super::Cli::try_parse_from(args).unwrap();
            let JobCommand::Vm(request) = vm_request(&parsed.command).unwrap().unwrap() else {
                panic!("expected VM control");
            };
            assert_eq!(format!("{:?}", request.command), action);
            assert_eq!(request.job_id, "job-one");
            assert_eq!(request.attempt_id, "attempt-one");
        }
        assert!(
            super::super::Cli::try_parse_from([
                "pvisor",
                "status",
                "--vm-socket",
                "/private/control.sock"
            ])
            .is_err()
        );
    }
}
