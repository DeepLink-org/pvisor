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
                let Some(job) = crate::runtime::job_execution::Job::read(&record)? else {
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
pub(super) struct VmOptions {
    /// Host-only live VM endpoint (requires explicit Job and Attempt identities).
    #[arg(long, global = true, requires_all = ["vm_job_id", "vm_attempt_id"])]
    pub vm_socket: Option<PathBuf>,
    #[arg(long, global = true, requires = "vm_socket")]
    pub vm_job_id: Option<String>,
    #[arg(long, global = true, requires = "vm_socket")]
    pub vm_attempt_id: Option<String>,
    /// Pause vCPUs rather than creating an execution checkpoint (suspend only).
    #[arg(
        long,
        global = true,
        requires = "vm_socket",
        conflicts_with = "vm_offload"
    )]
    pub vm_pause: bool,
    /// Quiesce and offload live VM RAM (suspend only).
    #[arg(long, global = true, requires = "vm_socket")]
    pub vm_offload: bool,
    #[arg(long, global = true, requires = "vm_offload")]
    pub vm_ram_file: Option<PathBuf>,
    /// Reload/unpause the same live Attempt, not snapshot restoration (resume only).
    #[arg(long, global = true, requires = "vm_socket")]
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
    pub fn request(self, command: &super::Command) -> anyhow::Result<Option<JobCommand>> {
        let Some(socket) = self.vm_socket else {
            return Ok(None);
        };
        let vm_command = match command {
            super::Command::Status(_) if !self.vm_pause && !self.vm_offload && !self.vm_load => {
                HostVmCommand::Status
            }
            super::Command::Suspend(_) if self.vm_pause && !self.vm_load => HostVmCommand::Pause,
            super::Command::Suspend(_) if self.vm_offload && !self.vm_load => {
                HostVmCommand::Offload {
                    file: self.vm_ram_file,
                }
            }
            super::Command::Resume(_) if self.vm_load && !self.vm_pause && !self.vm_offload => {
                HostVmCommand::Resume
            }
            _ => anyhow::bail!(
                "VM options require status, suspend --vm-pause/--vm-offload, or resume --vm-load"
            ),
        };
        let job_id = self
            .vm_job_id
            .ok_or_else(|| anyhow::anyhow!("--vm-job-id is required"))?;
        let attempt_id = self
            .vm_attempt_id
            .ok_or_else(|| anyhow::anyhow!("--vm-attempt-id is required"))?;
        let selector = match command {
            super::Command::Status(args) => args.selector.as_deref(),
            super::Command::Suspend(args) => Some(args.job_selector()),
            super::Command::Resume(args) => Some(args.job_selector()),
            _ => None,
        };
        anyhow::ensure!(
            selector.is_none_or(|selector| selector == std::path::Path::new(&job_id)),
            "live VM command selector must match the explicit --vm-job-id"
        );
        anyhow::ensure!(
            !job_id.trim().is_empty() && !attempt_id.trim().is_empty(),
            "VM identities must not be empty"
        );
        Ok(Some(JobCommand::Vm(VmRequest {
            socket,
            job_id,
            attempt_id,
            command: vm_command,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

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
    fn durable_request_ids_share_core_bounds_and_reject_controls() {
        validate_request_id(&"x".repeat(256)).unwrap();
        for id in [
            "x".repeat(257),
            "bad\nkey".into(),
            "bad\u{0085}key".into(),
            " ".into(),
        ] {
            assert_eq!(
                validate_request_id(&id)
                    .unwrap_err()
                    .downcast_ref::<AgentCtlHostError>()
                    .unwrap()
                    .code,
                AgentCtlHostErrorCode::InvalidRequest
            );
        }
    }

    #[test]
    fn durable_target_rejects_stale_job_attempt_and_generation() {
        let record: crate::RunRecord = serde_json::from_value(serde_json::json!({
            "schema_version": 1, "run_id": "job-fence", "session_id": "session-fence",
            "agent": "sh", "pid": 0, "command": ["/bin/sh"], "state": "completed",
            "started_at_unix_ms": 1, "finished_at_unix_ms": 2, "storage": "/tmp/job-fence",
            "network": {}, "gateway_listen": null,
            "overlay": {"id": "job-fence", "generation": 7, "target": "/tmp/workspace",
                "upper": {"upper_dir": "/tmp/job-fence/upper", "work_dir": "/tmp/job-fence/work"},
                "merged_dir": "/tmp/job-fence/merged", "stage_dir": "/tmp/job-fence",
                "auto_apply": false, "state": "staged"}
        }))
        .unwrap();
        let target = AgentCtlTarget {
            job_id: record.run_id.clone(),
            attempt_id: record.attempt_id.clone(),
            generation: Some("7".into()),
        };
        check_target(&target, &record).unwrap();
        for field in 0..3 {
            let mut stale = target.clone();
            match field {
                0 => stale.job_id = "other-job".into(),
                1 => stale.attempt_id = Some("stale-attempt".into()),
                _ => stale.generation = Some("6".into()),
            }
            assert_eq!(
                check_target(&stale, &record)
                    .unwrap_err()
                    .downcast_ref::<AgentCtlHostError>()
                    .unwrap()
                    .code,
                AgentCtlHostErrorCode::Conflict
            );
        }
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
        assert!(!JobCommand::Status(args).inherits_terminal_input().unwrap());
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
            let JobCommand::Vm(request) = parsed.vm.request(&parsed.command).unwrap().unwrap()
            else {
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
    pub(super) fn selected_record(&self, cwd: &Path) -> anyhow::Result<Option<crate::RunRecord>> {
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
        Ok(Some(crate::runtime::resolve_run(Some(selector), &output)?))
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
        let record = crate::runtime::resolve_run(selector, output)?;
        let root = crate::runtime::job_execution::Job::read(&record)?
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
            crate::runtime::host_transport::validate_host_target(target, &a.job_id, &a.attempt_id)?;
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

pub(super) fn validate_request_id(id: &str) -> anyhow::Result<()> {
    pvisor_core::host_protocol::AgentCtlHostRequest {
        version: pvisor_core::host_protocol::AGENTCTL_HOST_VERSION,
        request_id: id.to_owned(),
        target: None,
        command: (),
    }
    .validate()?;
    if id.trim().is_empty() {
        return Err(AgentCtlHostError::new(
            AgentCtlHostErrorCode::InvalidRequest,
            "durable request id must not be blank",
        )
        .into());
    }
    Ok(())
}

pub(super) fn lock_selected_job(
    record: &crate::RunRecord,
) -> anyhow::Result<Option<(crate::runtime::job_execution::Job, impl Send)>> {
    let Some(template) = crate::runtime::job_execution::Job::read(record)? else {
        return Ok(None);
    };
    let lease = template.lock()?;
    let current = template.current()?;
    current.validate_record_target(record)?;
    let current_record = crate::RunRecord::read(&current.active_stage)?;
    check_selected_record(record, &current_record)?;
    super::host_service::check_record(&current_record)?;
    Ok(Some((current, lease)))
}

pub(super) fn check_selected_record(
    selected: &crate::RunRecord,
    current: &crate::RunRecord,
) -> anyhow::Result<()> {
    check_target(
        &AgentCtlTarget {
            job_id: selected.run_id.clone(),
            attempt_id: selected.attempt_id.clone(),
            generation: selected.overlay.as_ref().map(|o| o.generation.to_string()),
        },
        current,
    )
}

pub(super) fn check_target(
    target: &AgentCtlTarget,
    record: &crate::RunRecord,
) -> anyhow::Result<()> {
    target.validate()?;
    let generation = record
        .overlay
        .as_ref()
        .map(|overlay| overlay.generation.to_string());
    if target.job_id != record.run_id
        || target.attempt_id != record.attempt_id
        || target.generation != generation
    {
        return Err(AgentCtlHostError::new(
            AgentCtlHostErrorCode::Conflict,
            "stale Job, Attempt, or workspace generation",
        )
        .into());
    }
    Ok(())
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
            let response = crate::runtime::instance_control::exchange(
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
