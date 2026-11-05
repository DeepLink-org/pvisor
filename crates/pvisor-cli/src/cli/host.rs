//! Typed Job requests. CLI syntax is never reconstructed in the service.
//! These DTOs bootstrap a self-executed owner, not a stable public
//! Job API. The shared core envelopes and host framing are separate contracts.
use super::{checkpoint, product, run, runtime};
use anyhow::Context;
use pvisor_core::host_protocol::{AgentCtlHostError, AgentCtlHostErrorCode, AgentCtlTarget};
use serde::{Deserialize, Serialize};
use std::path::Path;

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

    Drop(runtime::SelectArgs),
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
                let Some(job) = pvisor::JobExecution::read(&record)? else {
                    return Ok(false);
                };
                let pvisor_core::RunInvocation::Process(process) = &job.spec.invocation;
                Ok(process.stdin == pvisor_core::StdioMode::Inherit)
            }
            _ => Ok(false),
        }
    }
}

impl JobCommand {
    pub(super) fn operation(&self) -> &'static str {
        match self {
            Self::Run(_) => "run",
            Self::Status(_) => "status",
            Self::Kill(_) => "kill",
            Self::Suspend(_) => "suspend",
            Self::Resume(_) => "resume",
            Self::Fork(_) => "fork",
            Self::Checkpoint(_) => "checkpoint",
            Self::Inspect(_) => "inspect",
            Self::Review(_) => "review",

            Self::Drop(_) => "drop",
        }
    }
    pub(super) fn preflight(&self, record: &pvisor::RunRecord) -> anyhow::Result<()> {
        match self {
            Self::Resume(_) | Self::Suspend(_) => checkpoint::check_execution(record)?,
            Self::Fork(args) if args.restores_execution() => checkpoint::check_execution(record)?,
            Self::Fork(_) => {
                pvisor::job_service::policy::workspace_config(record)?;
            }
            Self::Checkpoint(args) => args.preflight(record)?,
            _ => {}
        }
        Ok(())
    }
    /// Workspace mutation/restoration owns the stopped source directory.
    /// Execution fork instead pins immutable checkpoints under the runtime's
    /// execution-operation lease; the source Attempt may keep running.
    pub(super) fn lock_source(
        &self,
        record: &pvisor::RunRecord,
    ) -> anyhow::Result<Option<pvisor::JobDirectoryLock>> {
        if matches!(self, Self::Fork(args) if args.restores_execution()) {
            return Ok(None);
        }
        pvisor::JobDirectoryLock::acquire(record, self.operation()).map(Some)
    }
    fn selection(&self) -> Option<(Option<&Path>, &Path)> {
        match self {
            Self::Run(_) => None,
            Self::Status(a) => Some((a.selector.as_deref(), &a.output_dir)),
            Self::Inspect(a) => Some((a.selector.as_deref(), &a.output_dir)),
            Self::Review(a) => Some((a.selector.as_deref(), &a.output_dir)),
            Self::Kill(a) => Some((Some(&a.selector), &a.output_dir)),

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
        let Some((selector, output)) = self.selection() else {
            return Ok(None);
        };
        let record = pvisor::resolve_run(selector, output)?;
        let root = pvisor::JobExecution::read(&record)?
            .map(|job| job.root)
            .unwrap_or_else(|| record.stage_dir());
        let root = std::fs::canonicalize(&root)
            .with_context(|| format!("resolve durable Job root {}", root.display()))?;
        match self {
            Self::Status(a) => a.selector = Some(root),
            Self::Inspect(a) => a.selector = Some(root),
            Self::Review(a) => a.selector = Some(root),
            Self::Kill(a) => a.selector = root,

            Self::Drop(a) => a.selector = root,
            Self::Fork(a) => a.pin_selection(root),
            Self::Suspend(a) => a.selection_mut().job = root,
            Self::Resume(a) => a.selection_mut().job = root,
            Self::Checkpoint(a) => a.selection_mut().job = root,
            Self::Run(_) => unreachable!(),
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
) -> anyhow::Result<Option<(pvisor::JobExecution, impl Send)>> {
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

        JobCommand::Drop(args) => runtime::drop_overlay(args)?,
        JobCommand::Review(args) => product::review(args)?,
        JobCommand::Suspend(args) => rt.block_on(checkpoint::suspend(args))?,
        JobCommand::Checkpoint(args) => rt.block_on(checkpoint::run(args))?,
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::live_vm::vm_request;
    use clap::{CommandFactory, Parser};
    use pvisor_core::host_protocol::HostVmCommand;

    #[test]
    fn execution_fork_delegates_source_locking_but_workspace_fork_remains_exclusive() {
        let root = tempfile::tempdir().unwrap();
        let record: pvisor::RunRecord = serde_json::from_value(serde_json::json!({
            "schema_version":1, "run_id":"source", "session_id":"source",
            "agent":"probe", "pid":0, "command":["probe"], "state":"completed",
            "started_at_unix_ms":1, "finished_at_unix_ms":2,
            "storage":root.path(), "network":{}, "gateway_listen":null, "overlay":null
        }))
        .unwrap();
        record.write().unwrap();
        let owner = pvisor::JobDirectoryLock::acquire(&record, "test-owner").unwrap();
        for (state, checkpoint) in [
            ("workspace", None),
            ("execution", None),
            ("execution", Some("sealed-checkpoint")),
        ] {
            let mut argv = vec!["pvisor", "fork", "source", "--state", state];
            if let Some(checkpoint) = checkpoint {
                argv.extend(["--checkpoint", checkpoint]);
            }
            let parsed = super::super::Cli::try_parse_from(argv).unwrap();
            let super::super::Command::Fork(args) = parsed.command else {
                panic!("expected fork");
            };
            let command: JobCommand =
                serde_json::from_value(serde_json::to_value(JobCommand::Fork(args)).unwrap())
                    .unwrap();
            let result = command.lock_source(&record);
            if state == "execution" {
                assert!(result.unwrap().is_none());
            } else {
                assert!(
                    result.is_err(),
                    "workspace fork must retain source exclusion"
                );
            }
        }
        drop(owner);
    }

    #[test]
    fn nested_run_args_roundtrip_as_typed_data() {
        let parsed = super::super::Cli::try_parse_from([
            "pvisor",
            "spawn",
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
        let super::super::Command::Spawn(args) = parsed.command else {
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
    fn workspace_selectors_roundtrip_in_the_existing_typed_run_request() {
        let parsed = super::super::Cli::try_parse_from([
            "pvisor",
            "spawn",
            "--vm",
            "--rootfs-base-store",
            "/private/root-store",
            "--rootfs-base",
            "root-id",
            "--workspace-base-store",
            "/private/workspace-store",
            "--workspace-base",
            "workspace-id",
            "--",
            "true",
        ])
        .unwrap();
        let super::super::Command::Spawn(args) = parsed.command else {
            panic!("expected Run");
        };
        let value = serde_json::to_value(JobCommand::Run(args)).unwrap();
        assert_eq!(
            value["args"]["overlayfs"]["workspace_base_store"],
            "/private/workspace-store"
        );
        assert_eq!(value["args"]["overlayfs"]["workspace_base"], "workspace-id");
        let decoded: JobCommand = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), value);
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
                "spawn",
                "--spec",
                path.to_str().unwrap(),
                "--result-file",
                "result.json",
            ])
            .unwrap();
            let super::super::Command::Spawn(args) = parsed.command else {
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
                "spawn",
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
            let args = super::super::normalize_default_spawn(
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
            let request = vm_request(&parsed.command).unwrap().unwrap();
            let target = request.target().unwrap();
            assert_eq!(target.job_id, "job-one");
            assert_eq!(target.attempt_id.as_deref(), Some("attempt-one"));
            request.validate_target(Some(&target)).unwrap();
            assert!(request.validate_target(None).is_err());
            let mut wrong = target.clone();
            wrong.attempt_id = Some("another-attempt".into());
            assert!(request.validate_target(Some(&wrong)).is_err());
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
            "spawn",
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
        assert!(vm_request(&parsed.command).unwrap().is_some());
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
        let request = vm_request(&parsed.command).unwrap().unwrap();
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
            let request = vm_request(&parsed.command).unwrap().unwrap();
            assert_eq!(format!("{:?}", request.command), action);
            assert_eq!(request.target().unwrap().job_id, "job-one");
            assert_eq!(
                request.target().unwrap().attempt_id.as_deref(),
                Some("attempt-one")
            );
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

    #[test]
    fn live_vm_controls_cannot_enter_the_owner_bootstrap_protocol() {
        assert!(
            serde_json::from_value::<JobCommand>(serde_json::json!({
                "operation": "vm",
                "args": {
                    "job_id": "job-one",
                    "attempt_id": "attempt-one",
                    "socket": "/private/control.sock",
                    "endpoint": null,
                    "command": {"operation": "status"}
                }
            }))
            .is_err()
        );
        let root = super::super::Cli::command();
        assert!(root.find_subcommand("vm").is_none());
    }

    #[test]
    fn ordinary_lifecycle_commands_do_not_become_live_control_requests() {
        for command in ["status", "suspend", "resume"] {
            let parsed = super::super::Cli::try_parse_from(["pvisor", command, "job-one"]).unwrap();
            assert!(vm_request(&parsed.command).unwrap().is_none());
        }
    }
}
