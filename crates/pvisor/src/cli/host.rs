//! Typed Job requests. CLI syntax is never reconstructed in the service.
use super::{checkpoint, product, run, runtime};
use clap::Args;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "operation", content = "args", rename_all = "snake_case")]
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
pub(crate) struct VmRequest {
    pub socket: PathBuf,
    pub job_id: String,
    pub attempt_id: String,
    pub action: VmAction,
    pub file: Option<PathBuf>,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum VmAction {
    Status,
    Pause,
    Offload,
    Load,
}

impl VmOptions {
    pub fn request(self, command: &super::Command) -> anyhow::Result<Option<JobCommand>> {
        let Some(socket) = self.vm_socket else {
            return Ok(None);
        };
        let action = match command {
            super::Command::Status(_) if !self.vm_pause && !self.vm_offload && !self.vm_load => {
                VmAction::Status
            }
            super::Command::Suspend(_) if self.vm_pause && !self.vm_load => VmAction::Pause,
            super::Command::Suspend(_) if self.vm_offload && !self.vm_load => VmAction::Offload,
            super::Command::Resume(_) if self.vm_load && !self.vm_pause && !self.vm_offload => {
                VmAction::Load
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
            action,
            file: self.vm_ram_file,
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
    fn vm_controls_are_normal_command_options() {
        for (command, option, action) in [
            ("status", None, "Status"),
            ("suspend", Some("--vm-pause"), "Pause"),
            ("suspend", Some("--vm-offload"), "Offload"),
            ("resume", Some("--vm-load"), "Load"),
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
            assert_eq!(format!("{:?}", request.action), action);
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

pub(crate) fn execute(command: JobCommand) -> anyhow::Result<i32> {
    let rt = tokio::runtime::Runtime::new()?;
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
            use crate::runtime::instance_control::{
                INSTANCE_CONTROL_VERSION, InstanceControlCommand as C, InstanceControlRequest,
                exchange,
            };
            let response = exchange(
                &args.socket,
                &InstanceControlRequest {
                    version: INSTANCE_CONTROL_VERSION,
                    run_id: args.job_id.into(),
                    attempt_id: args.attempt_id.into(),
                    command: match args.action {
                        VmAction::Status => C::Status,
                        VmAction::Pause => C::Pause,
                        VmAction::Offload => C::Offload,
                        VmAction::Load => C::Load,
                    },
                    file: args.file,
                },
            )
            .await?;
            println!("{}", serde_json::to_string(&response)?);
            anyhow::ensure!(
                response.ok,
                "VM control rejected: {}",
                response.error.as_deref().unwrap_or("unspecified error")
            );
            Ok::<_, anyhow::Error>(())
        })?,
    }
    Ok(0)
}
