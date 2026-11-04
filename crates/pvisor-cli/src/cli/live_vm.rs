//! Foreground live-VM controls, independent of durable Job lifecycle requests.
use anyhow::Context;
use clap::Args;
use pvisor::host_transport::{InstanceCommand, LiveControlRecord};
use pvisor_core::host_protocol::{
    AGENTCTL_HOST_VERSION, AgentCtlHostRequest, AgentCtlTarget, HostVmCommand,
};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Args)]
#[command(next_help_heading = "Live VM")]
pub(super) struct VmOptions {
    /// Host-only live VM endpoint (requires explicit Job and Attempt identities).
    #[arg(long, requires_all = ["vm_job_id", "vm_attempt_id"])]
    pub vm_socket: Option<PathBuf>,
    /// Explicit Job identity served by the live VM endpoint.
    #[arg(long, requires = "vm_socket")]
    pub vm_job_id: Option<String>,
    /// Explicit Attempt identity for the debug socket override.
    #[arg(long, requires = "vm_socket")]
    pub vm_attempt_id: Option<String>,
}

#[derive(Debug, Default, Args)]
#[command(next_help_heading = "Live VM")]
pub(super) struct SuspendVmOptions {
    #[command(flatten)]
    pub identity: VmOptions,
    /// Pause vCPUs rather than creating an execution checkpoint (suspend only).
    #[arg(long, conflicts_with = "vm_offload")]
    pub vm_pause: bool,
    /// Quiesce and offload live VM RAM (suspend only).
    #[arg(long)]
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
    #[arg(long)]
    pub vm_load: bool,
}

#[derive(Debug)]
enum Endpoint {
    Recorded(LiveControlRecord),
    Debug(PathBuf),
}

/// Not a bootstrap DTO: live controls never launch or restore a Job owner.
#[derive(Debug)]
pub(super) struct VmRequest {
    endpoint: Endpoint,
    target: AgentCtlTarget,
    pub(super) command: HostVmCommand,
}

impl VmRequest {
    pub(super) fn target(&self) -> anyhow::Result<AgentCtlTarget> {
        self.target.validate()?;
        Ok(self.target.clone())
    }

    pub(super) fn validate_target(&self, target: Option<&AgentCtlTarget>) -> anyhow::Result<()> {
        pvisor::host_transport::validate_host_target(
            target,
            &self.target.job_id,
            self.target
                .attempt_id
                .as_deref()
                .context("VM target has no Attempt")?,
        )?;
        Ok(())
    }

    pub(super) fn execute(&self, rt: &tokio::runtime::Runtime) -> anyhow::Result<i32> {
        super::host_service::check_cancelled()?;
        match &self.endpoint {
            Endpoint::Recorded(endpoint) => print_result(
                &endpoint.exchange_checked(
                    &self.target.job_id,
                    self.target
                        .attempt_id
                        .as_deref()
                        .context("VM target has no Attempt")?,
                    match &self.command {
                        HostVmCommand::Status => InstanceCommand::Status {},
                        HostVmCommand::Pause => InstanceCommand::Pause {},
                        HostVmCommand::Resume => InstanceCommand::Resume {},
                        HostVmCommand::Offload { file } => {
                            InstanceCommand::Offload { file: file.clone() }
                        }
                    },
                    Some(&super::host_service::check_cancelled),
                )?,
            ),
            Endpoint::Debug(socket) => {
                let response = rt.block_on(pvisor::host_vm_exchange(
                    socket,
                    &AgentCtlHostRequest {
                        version: AGENTCTL_HOST_VERSION,
                        request_id: uuid::Uuid::new_v4().to_string(),
                        target: Some(self.target.clone()),
                        command: self.command.clone(),
                    },
                ))?;
                print_result(&response.result?)
            }
        }
    }
}

fn print_result(result: &impl serde::Serialize) -> anyhow::Result<i32> {
    println!("{}", serde_json::to_string(result)?);
    Ok(0)
}

pub(super) fn resume_record(
    rt: &tokio::runtime::Runtime,
    record: &pvisor::RunRecord,
) -> anyhow::Result<i32> {
    request_for_record(HostVmCommand::Resume, record)?.execute(rt)
}

impl VmOptions {
    fn request(
        &self,
        command: HostVmCommand,
        selector: Option<&Path>,
    ) -> anyhow::Result<Option<VmRequest>> {
        let Some(socket) = &self.vm_socket else {
            return Ok(None);
        };
        let job_id = self.vm_job_id.as_ref().context("--vm-job-id is required")?;
        let attempt_id = self
            .vm_attempt_id
            .as_ref()
            .context("--vm-attempt-id is required")?;
        anyhow::ensure!(
            selector.is_none_or(|selector| selector == Path::new(job_id)),
            "live VM command selector must match the explicit --vm-job-id"
        );
        anyhow::ensure!(
            !job_id.trim().is_empty() && !attempt_id.trim().is_empty(),
            "VM identities must not be empty"
        );
        Ok(Some(VmRequest {
            endpoint: Endpoint::Debug(socket.clone()),
            target: AgentCtlTarget {
                job_id: job_id.clone(),
                attempt_id: Some(attempt_id.clone()),
                generation: None,
            },
            command,
        }))
    }
}

fn recorded_vm(
    command: HostVmCommand,
    selector: &Path,
    storage: &Path,
) -> anyhow::Result<Option<VmRequest>> {
    let record = pvisor::resolve_run(Some(selector), storage)?;
    request_for_record(command, &record).map(Some)
}

fn request_for_record(
    command: HostVmCommand,
    record: &pvisor::RunRecord,
) -> anyhow::Result<VmRequest> {
    let endpoint = record
        .live_control
        .as_ref()
        .filter(|endpoint| endpoint.is_available())
        .context("Job has no valid recorded owner")?;
    Ok(VmRequest {
        endpoint: Endpoint::Recorded(endpoint.clone()),
        target: AgentCtlTarget {
            job_id: record.run_id.clone(),
            attempt_id: Some(
                record
                    .attempt_id
                    .clone()
                    .context("Job has no Attempt identity")?,
            ),
            generation: None,
        },
        command,
    })
}

pub(super) fn vm_request(command: &super::Command) -> anyhow::Result<Option<VmRequest>> {
    match command {
        super::Command::Status(args) => args
            .vm
            .request(HostVmCommand::Status, args.args.selector.as_deref()),
        super::Command::Suspend(args) => {
            if args.vm.identity.vm_socket.is_none() && !args.vm.vm_pause && !args.vm.vm_offload {
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
            if args.vm.identity.vm_socket.is_none() {
                return recorded_vm(
                    command,
                    args.args.job_selector(),
                    &args.args.selection().output_dir,
                );
            }
            args.vm
                .identity
                .request(command, Some(args.args.job_selector()))
        }
        super::Command::Resume(args) => {
            if args.vm.identity.vm_socket.is_none() {
                if args.vm.vm_load {
                    return recorded_vm(
                        HostVmCommand::Resume,
                        args.args.job_selector(),
                        &args.args.selection().output_dir,
                    );
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use pvisor::host_transport::{read_host_frame_sync, write_host_frame_sync};
    use pvisor_core::host_protocol::{
        AgentCtlHostError, AgentCtlHostErrorCode, AgentCtlHostResponse,
    };
    use std::os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::UnixListener,
    };

    fn listener() -> (tempfile::TempDir, LiveControlRecord, UnixListener) {
        let root = pvisor::host_transport::host_authority_root().unwrap();
        let directory = tempfile::Builder::new()
            .prefix("vm-test-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(root)
            .unwrap();
        let path = directory.path().join("control.sock");
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        let endpoint = LiveControlRecord {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            protocol: pvisor::host_transport::INSTANCE_CONTROL_PROTOCOL.into(),
            binary: std::env::current_exe().unwrap(),
            binary_digest: String::new(),
            pid: std::process::id(),
        };
        (directory, endpoint, listener)
    }

    fn target() -> AgentCtlTarget {
        AgentCtlTarget {
            job_id: "job-one".into(),
            attempt_id: Some("attempt-one".into()),
            generation: None,
        }
    }

    #[test]
    fn recorded_controls_send_one_exactly_targeted_request_without_job_bootstrap() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for (command, expected) in [
            (
                HostVmCommand::Status,
                serde_json::json!({"operation": "status"}),
            ),
            (
                HostVmCommand::Pause,
                serde_json::json!({"operation": "pause"}),
            ),
            (
                HostVmCommand::Resume,
                serde_json::json!({"operation": "resume"}),
            ),
            (
                HostVmCommand::Offload {
                    file: Some("/private/ram".into()),
                },
                serde_json::json!({"operation": "offload", "file": "/private/ram"}),
            ),
        ] {
            let (_directory, endpoint, listener) = listener();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let request: AgentCtlHostRequest<InstanceCommand> =
                    read_host_frame_sync(&mut stream).unwrap();
                request.validate().unwrap();
                assert_eq!(request.target, Some(target()));
                assert_eq!(serde_json::to_value(&request.command).unwrap(), expected);
                write_host_frame_sync(
                    &mut stream,
                    &AgentCtlHostResponse {
                        version: AGENTCTL_HOST_VERSION,
                        request_id: request.request_id,
                        result: Ok(serde_json::json!({"status": "fixture", "value": null})),
                    },
                )
                .unwrap();
            });
            let request = VmRequest {
                endpoint: Endpoint::Recorded(endpoint),
                target: target(),
                command,
            };
            assert_eq!(request.execute(&rt).unwrap(), 0);
            server.join().unwrap();
        }
    }

    #[test]
    fn debug_controls_preserve_the_typed_wire_and_endpoint_errors() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (_directory, endpoint, listener) = listener();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let request: AgentCtlHostRequest<HostVmCommand> =
                read_host_frame_sync(&mut stream).unwrap();
            request.validate().unwrap();
            assert_eq!(request.target, Some(target()));
            assert!(matches!(request.command, HostVmCommand::Pause));
            write_host_frame_sync(
                &mut stream,
                &AgentCtlHostResponse::<serde_json::Value> {
                    version: AGENTCTL_HOST_VERSION,
                    request_id: request.request_id,
                    result: Err(AgentCtlHostError::new(
                        AgentCtlHostErrorCode::Unsupported,
                        "not a VM",
                    )),
                },
            )
            .unwrap();
        });
        let request = VmRequest {
            endpoint: Endpoint::Debug(endpoint.path),
            target: target(),
            command: HostVmCommand::Pause,
        };
        let error = request.execute(&rt).unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<AgentCtlHostError>()
                .unwrap_or_else(|| panic!("{error:#}"))
                .code,
            AgentCtlHostErrorCode::Unsupported
        );
        server.join().unwrap();
    }
}
