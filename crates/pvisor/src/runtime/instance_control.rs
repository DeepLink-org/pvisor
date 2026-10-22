//! Host-only, attempt-scoped live VM control. One JSON line per connection.
//!
//! This is not snapshot restart: `resume` resumes the same live Attempt. Endpoint
//! discovery lives in the host Job record; no endpoint is projected into a guest.

use super::host_transport::{
    authorize_host_peer, host_authority_root, read_host_frame, validate_host_target,
    write_host_frame,
};
use super::run::{AttemptService, RunControlHandle, RunHandle};
#[cfg(test)]
use pvisor_core::AttemptId;
use pvisor_core::RunStatus;
#[cfg(test)]
use pvisor_core::host_protocol::AgentCtlTarget;
use pvisor_core::host_protocol::{
    AGENTCTL_HOST_VERSION, AgentCtlHostError, AgentCtlHostErrorCode, AgentCtlHostRequest,
    AgentCtlHostResponse, HostAttemptCommand, HostVmCommand, HostVmResult,
};
#[cfg(test)]
use pvisor_core::operation::OperationKind;
use std::collections::HashMap;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
#[cfg(test)]
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinSet;

/// Runtime-owned host path. Native executors must exclude it from all guest
/// filesystem lower projections (the same way they exclude live RAM backing).
pub const INSTANCE_CONTROL_DIRECTORY_METADATA: &str = "pvisor.instance_control.directory";
const MAX_CONNECTIONS: usize = 8;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const OPERATION_TIMEOUT: Duration = Duration::from_secs(310);

// RunHandle is constructed by Session outside this module. Keep only discovery
// paths here, never control authority or backing ownership; the server guard
// removes its entry even when the Tokio runtime aborts its task.
type Endpoints = HashMap<(String, String), PathBuf>;
fn endpoints() -> &'static Mutex<Endpoints> {
    static ENDPOINTS: OnceLock<Mutex<Endpoints>> = OnceLock::new();
    ENDPOINTS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn owns_record(record: &super::RunRecord) -> bool {
    let (Some(endpoint), Some(attempt)) = (&record.live_control, &record.attempt_id) else {
        return false;
    };
    endpoint.pid == std::process::id()
        && endpoints()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&(record.run_id.clone(), attempt.clone()))
            .is_some_and(|path| path == &endpoint.path)
}

pub(crate) fn socket(handle: &RunHandle) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        handle.status().attempt.executor.kind == pvisor_core::ExecutorKind::VirtualMachine,
        "live VM controls require a VM executor"
    );
    anyhow::ensure!(
        !handle.status().state.is_terminal(),
        "attempt control endpoint is no longer available"
    );
    endpoints()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(handle.run_id().to_string(), handle.attempt_id().to_string()))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("attempt control endpoint is unavailable"))
}

struct OwnedSocket {
    path: PathBuf,
    device: u64,
    inode: u64,
    prune: bool,
}

impl Drop for OwnedSocket {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
        }) {
            let _ = std::fs::remove_file(&self.path);
        }
        if self.prune
            && let Some(parent) = self.path.parent()
        {
            let _ = std::fs::remove_dir(parent);
            if let Some(job) = parent.parent() {
                let _ = std::fs::remove_dir(job);
            }
        }
    }
}

pub(crate) struct InstanceControlServer {
    listener: UnixListener,
    socket: OwnedSocket,
    identity: Option<(String, String)>,
    exclusion_directory: PathBuf,
}

impl InstanceControlServer {
    pub(crate) fn bind(path: Option<&Path>, job: &str, attempt: &str) -> anyhow::Result<Self> {
        // Directory-fd addressing keeps canonical UUID paths usable. Custom parents
        // must already exist; never create them, follow a parent symlink, or
        // unlink an existing entry to make binding succeed.
        let authority_root = host_authority_root()?;
        let prune = path.is_none();
        let path = match path {
            Some(path) => super::host_transport::instance_socket_override(path)?,
            None => super::host_transport::instance_control_path(job, attempt)?,
        };
        let exclusion_directory = authority_root;
        let native = super::host_transport::bind_instance_socket(&path)?;
        native.set_nonblocking(true)?;
        let listener = UnixListener::from_std(native)?;
        let metadata = std::fs::symlink_metadata(&path)?;
        let socket = OwnedSocket {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            prune,
        };
        // Install cleanup before the fallible permission operation.
        std::fs::set_permissions(&socket.path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            listener,
            socket,
            identity: None,
            exclusion_directory,
        })
    }

    pub(crate) fn record(&self) -> anyhow::Result<super::host_transport::LiveControlRecord> {
        use std::io::Read;
        let binary = std::env::current_exe()?;
        #[cfg(target_os = "linux")]
        let mut file = std::fs::File::open("/proc/self/exe")?;
        #[cfg(not(target_os = "linux"))]
        let mut file = std::fs::File::open(&binary)?;
        let mut hash = blake3::Hasher::new();
        let mut bytes = [0; 64 * 1024];
        loop {
            let count = file.read(&mut bytes)?;
            if count == 0 {
                break;
            }
            hash.update(&bytes[..count]);
        }
        Ok(super::host_transport::LiveControlRecord {
            path: self.socket.path.clone(),
            device: self.socket.device,
            inode: self.socket.inode,
            protocol: super::host_transport::INSTANCE_CONTROL_PROTOCOL.into(),
            binary,
            binary_digest: hash.finalize().to_hex().to_string(),
            pid: std::process::id(),
        })
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.exclusion_directory
    }

    pub(crate) fn start(mut self, handle: &RunHandle) -> tokio::task::JoinHandle<()> {
        let identity = (handle.run_id().to_string(), handle.attempt_id().to_string());
        endpoints()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(identity.clone(), self.socket.path.clone());
        self.identity = Some(identity);
        let controls = handle.controls();
        let mut status = handle.status.clone();
        let attempt_task = handle.join.abort_handle();
        let record = handle.record.clone();
        tokio::spawn(async move {
            let mut clients = JoinSet::new();
            let mut replies: Vec<(tokio::task::AbortHandle, Arc<AtomicBool>)> = Vec::new();
            let mounts = Arc::new(Mutex::new(HashMap::new()));
            // A panic can finish the attempt without publishing terminal status.
            // Our control authority holds a status sender, so channel closure
            // cannot detect that path. Observe task completion without joining
            // it, synthesizing lifecycle status, or delaying normal termination.
            let mut completion_check = tokio::time::interval(Duration::from_secs(1));
            completion_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                if status.borrow().state.is_terminal() {
                    break;
                }
                tokio::select! {
                    biased;
                    changed = status.changed() => {
                        if changed.is_err() { break; }
                    }
                    _ = completion_check.tick() => {
                        if attempt_task.is_finished() { break; }
                    }
                    _ = clients.join_next(), if !clients.is_empty() => { replies.retain(|(handle, _)| !handle.is_finished()); }
                    accepted = self.listener.accept() => {
                        let Ok((stream, _)) = accepted else { break; };
                        if clients.len() >= MAX_CONNECTIONS { continue; }
                        let controls = controls.clone();
                        let status = status.clone();
                        let record = record.clone();
                        let mounts = mounts.clone();
                        let retain = Arc::new(AtomicBool::new(false));
                        let pending = retain.clone();
                        let task = clients.spawn(async move {
                            serve(stream, controls, status, record, mounts, pending, OPERATION_TIMEOUT).await;
                        });
                        replies.push((task, retain));
                    }
                }
            }
            // A suspend reply requires durable Job completion after terminal
            // publication. Keep accepted capture/kill replies; abort idle clients.
            for (task, retain) in replies {
                if !retain.load(Ordering::Acquire) {
                    task.abort();
                }
            }
            drop(self);
            let _ = tokio::time::timeout(OPERATION_TIMEOUT + IO_TIMEOUT, async {
                while clients.join_next().await.is_some() {}
            })
            .await;
        })
    }
}

impl Drop for InstanceControlServer {
    fn drop(&mut self) {
        if let Some(identity) = &self.identity {
            let mut registry = endpoints().lock().unwrap_or_else(|e| e.into_inner());
            if registry.get(identity) == Some(&self.socket.path) {
                registry.remove(identity);
            }
        }
    }
}

fn host_operation(
    request: &AgentCtlHostRequest<HostVmCommand>,
    status: &RunStatus,
) -> Result<HostAttemptCommand, AgentCtlHostError> {
    request.validate()?;
    validate_host_target(
        request.target.as_ref(),
        status.run_id.as_str(),
        status.attempt.attempt_id.as_str(),
    )?;
    if status.attempt.executor.kind != pvisor_core::ExecutorKind::VirtualMachine {
        return Err(AgentCtlHostError::new(
            AgentCtlHostErrorCode::Unsupported,
            "live VM controls require a VM executor",
        ));
    }
    Ok(request.command.clone().into())
}

async fn serve(
    mut stream: UnixStream,
    controls: RunControlHandle,
    status: tokio::sync::watch::Receiver<RunStatus>,
    record: Option<super::RunRecord>,
    mounts: Arc<Mutex<HashMap<String, super::ReadOnlyOverlayMount>>>,
    retain_reply: Arc<AtomicBool>,
    operation_timeout: Duration,
) {
    if authorize_host_peer(&stream).is_err() {
        return;
    }
    // A malformed frame has no trustworthy correlation ID; close without a reply.
    let Ok(Ok(request)) = tokio::time::timeout(
        IO_TIMEOUT,
        read_host_frame::<AgentCtlHostRequest<super::host_transport::InstanceCommand>>(&mut stream),
    )
    .await
    else {
        return;
    };
    retain_reply.store(
        matches!(
            request.command,
            super::host_transport::InstanceCommand::Capture { .. }
                | super::host_transport::InstanceCommand::Terminate {}
        ),
        Ordering::Release,
    );
    let request_id = request.request_id.clone();
    let operation = dispatch(request, controls, status.borrow().clone(), record, mounts);
    tokio::pin!(operation);
    let result = match tokio::time::timeout(operation_timeout, &mut operation).await {
        Ok(result) => result,
        Err(_) => {
            respond(
                &mut stream,
                &request_id,
                Err(AgentCtlHostError::new(
                    AgentCtlHostErrorCode::Unavailable,
                    "control response timed out; accepted operation may still complete",
                )),
            )
            .await;
            drop(stream);
            let _ = operation.await;
            return;
        }
    };
    respond(&mut stream, &request_id, result).await;
}

async fn dispatch(
    request: AgentCtlHostRequest<super::host_transport::InstanceCommand>,
    controls: RunControlHandle,
    status: RunStatus,
    record: Option<super::RunRecord>,
    mounts: Arc<Mutex<HashMap<String, super::ReadOnlyOverlayMount>>>,
) -> Result<serde_json::Value, AgentCtlHostError> {
    use super::host_transport::InstanceCommand;
    request.validate()?;
    validate_host_target(
        request.target.as_ref(),
        status.run_id.as_str(),
        status.attempt.attempt_id.as_str(),
    )?;
    let error = |error: anyhow::Error| {
        error
            .downcast_ref::<AgentCtlHostError>()
            .cloned()
            .unwrap_or_else(|| {
                AgentCtlHostError::new(AgentCtlHostErrorCode::Internal, format!("{error:#}"))
            })
    };
    let record = || -> anyhow::Result<super::RunRecord> {
        let saved = record
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Job has no durable storage"))?;
        let current = super::RunRecord::read(&saved.stage_dir())?;
        anyhow::ensure!(
            current.run_id == status.run_id.as_str()
                && current.attempt_id.as_deref() == Some(status.attempt.attempt_id.as_str()),
            "stale Job/Attempt control record"
        );
        Ok(current)
    };
    let value = match request.command {
        InstanceCommand::JobStatus {} => {
            let record = record().map_err(error)?;
            let mut result = super::job_service::RuntimeJobService::status(
                &Default::default(),
                super::job_service::StatusRequest {
                    job: super::job_service::JobSelection {
                        selector: Some(record.stage_dir()),
                        storage: record.storage.clone(),
                    },
                },
            )
            .map_err(error)?;
            result.live_attempt = Some(controls.status());
            serde_json::to_value(result)
        }
        InstanceCommand::Capture {
            suspend,
            ram_storage,
            timeout_ms,
            request_id,
        } => {
            let record = record().map_err(error)?;
            let result = super::job_service::RuntimeJobService::capture_selected_execution(
                &Default::default(),
                &record,
                suspend,
                ram_storage,
                Some(request_id),
                Duration::from_millis(timeout_ms),
            )
            .await
            .map_err(error)?;
            serde_json::to_value(result)
        }
        InstanceCommand::Inspect { checkpoint } => {
            let mut record = record().map_err(error)?;
            if let Some(id) = checkpoint {
                let checkpoint =
                    super::checkpoint::resolve_checkpoint(&record, &id).map_err(error)?;
                record = super::checkpoint::workspace_view(&record, &checkpoint).map_err(error)?;
            }
            let overlay = record
                .overlay
                .as_ref()
                .ok_or_else(|| error(anyhow::anyhow!("Job has no OverlayFS workspace")))?;
            let lowers = if record.overlay_lowers.is_empty() {
                vec![overlay.target.clone()]
            } else {
                record.overlay_lowers.clone()
            };
            let mut mounts = mounts.lock().unwrap_or_else(|error| error.into_inner());
            if mounts.len() >= MAX_CONNECTIONS {
                return Err(AgentCtlHostError::new(
                    AgentCtlHostErrorCode::Unavailable,
                    "inspect session limit reached",
                ));
            }
            let id = uuid::Uuid::new_v4().to_string();
            let mount = super::mount_overlay_record_read_only(
                overlay,
                &lowers,
                &overlay.stage_dir.join("inspect").join(&id).join("merged"),
            )
            .map_err(|failure| error(failure.into()))?;
            let mountpoint = mount.mountpoint().to_owned();
            mounts.insert(id.clone(), mount);
            Ok(serde_json::json!({ "id": id, "mountpoint": mountpoint }))
        }
        InstanceCommand::UnmountInspect { id } => {
            let mount = mounts
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&id)
                .ok_or_else(|| error(anyhow::anyhow!("unknown inspect session {id}")))?;
            mount.unmount().map_err(error)?;
            Ok(serde_json::Value::Null)
        }
        InstanceCommand::Terminate {} => {
            let result = AttemptService::new(controls)
                .dispatch(HostAttemptCommand::Terminate)
                .await?;
            serde_json::to_value(result)
        }
        command => {
            let command = match command {
                InstanceCommand::Status {} => HostVmCommand::Status,
                InstanceCommand::Pause {} => HostVmCommand::Pause,
                InstanceCommand::Resume {} => HostVmCommand::Resume,
                InstanceCommand::Offload { file } => HostVmCommand::Offload { file },
                _ => unreachable!(),
            };
            let request = AgentCtlHostRequest {
                version: request.version,
                request_id: request.request_id,
                target: request.target,
                command,
            };
            let command = host_operation(&request, &status)?;
            let result = AttemptService::new(controls).dispatch(command).await?;
            serde_json::to_value(HostVmResult::from(result))
        }
    };
    value
        .map_err(|error| AgentCtlHostError::new(AgentCtlHostErrorCode::Internal, error.to_string()))
}

async fn respond(
    stream: &mut UnixStream,
    request_id: &str,
    result: Result<serde_json::Value, AgentCtlHostError>,
) {
    let response = AgentCtlHostResponse {
        version: AGENTCTL_HOST_VERSION,
        request_id: request_id.to_owned(),
        result,
    };
    let _ = tokio::time::timeout(IO_TIMEOUT, write_host_frame(stream, &response)).await;
}

/// Bounded host VM exchange with caller-selected correlation and explicit target.
/// A timeout may follow accepted effects; reconcile state before resubmitting.
pub async fn exchange(
    path: &Path,
    request: &AgentCtlHostRequest<HostVmCommand>,
) -> anyhow::Result<AgentCtlHostResponse<HostVmResult>> {
    request.validate()?;
    let target = request
        .target
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("explicit VM target is required"))?;
    let attempt_id = target
        .attempt_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("explicit VM attempt is required"))?;
    validate_host_target(Some(target), &target.job_id, attempt_id)?;
    let path = super::host_transport::instance_socket_override(path)?;
    let path = path.as_path();
    let metadata = std::fs::symlink_metadata(path)?;
    let endpoint = super::host_transport::LiveControlRecord {
        path: path.to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
        protocol: super::host_transport::INSTANCE_CONTROL_PROTOCOL.into(),
        binary: std::env::current_exe()?,
        binary_digest: String::new(),
        pid: std::process::id(),
    };
    let native = endpoint.connect()?;
    native.set_nonblocking(true)?;
    let mut stream = UnixStream::from_std(native)?;
    authorize_host_peer(&stream)?;
    tokio::time::timeout(IO_TIMEOUT, write_host_frame(&mut stream, request))
        .await
        .map_err(|_| anyhow::anyhow!("control request write timed out"))??;
    let reply: AgentCtlHostResponse<HostVmResult> =
        tokio::time::timeout(OPERATION_TIMEOUT + IO_TIMEOUT, read_host_frame(&mut stream))
            .await
            .map_err(|_| {
                anyhow::anyhow!("control response timed out; accepted operation may still complete")
            })??;
    reply.validate(&request.request_id)?;
    if let Ok(response) = &reply.result {
        anyhow::ensure!(
            response.status.run_id.as_str() == target.job_id
                && response.status.attempt.attempt_id.as_str() == attempt_id,
            "control response identity mismatch"
        );
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::process::ProcessExecutor;
    use crate::host_transport::allocate_host_directory;
    use crate::{PVisor, RunExecutor, Session};
    use pvisor_core::{
        ExecutorKind, ExecutorPlan, IsolationKind, RunInvocation, RunSpec, RunState,
    };
    use std::sync::Arc;

    async fn connect(path: &Path) -> anyhow::Result<UnixStream> {
        let metadata = std::fs::symlink_metadata(path)?;
        let record = super::super::host_transport::LiveControlRecord {
            path: path.to_owned(),
            device: metadata.dev(),
            inode: metadata.ino(),
            protocol: super::super::host_transport::INSTANCE_CONTROL_PROTOCOL.into(),
            binary: std::env::current_exe()?,
            binary_digest: String::new(),
            pid: std::process::id(),
        };
        let native = record.connect()?;
        native.set_nonblocking(true)?;
        Ok(UnixStream::from_std(native)?)
    }

    struct PublishedVm;
    #[async_trait::async_trait]
    impl RunExecutor for PublishedVm {
        fn descriptor(&self) -> ExecutorPlan {
            MockVm.descriptor()
        }
        fn supports(&self, _: &RunInvocation) -> bool {
            true
        }
        async fn execute(&self, session: &Session) -> crate::ExecutorOutput {
            let record =
                crate::RunRecord::read(&session.drivers.as_ref().unwrap().record().stage_dir())
                    .unwrap();
            let control = record
                .live_control
                .as_ref()
                .expect("published before executor entry");
            assert_eq!(
                control.path,
                super::super::host_transport::instance_control_path(
                    &record.run_id,
                    record.attempt_id.as_deref().unwrap()
                )
                .unwrap()
            );
            assert_eq!(
                control.protocol,
                super::super::host_transport::INSTANCE_CONTROL_PROTOCOL
            );
            assert_eq!(control.pid, std::process::id());
            assert!(!control.binary_digest.is_empty());
            assert!(control.connect().is_ok());
            ProcessExecutor::default().execute(session).await
        }
    }

    #[tokio::test]
    async fn durable_control_is_published_before_execution_and_cleared_at_terminal() {
        let temp = tempfile::tempdir().unwrap();
        let handle = PVisor::builder()
            .storage(temp.path())
            .executors(vec![Arc::new(PublishedVm)])
            .network(crate::NetworkDriverConfig {
                mode: crate::OverlayNetMode::Off,
                ..Default::default()
            })
            .build();
        let mut spec = RunSpec::process(
            format!("publish-{}", uuid::Uuid::new_v4()),
            "test",
            "/bin/sh",
        );
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "exit 0".into()];
        let handle = handle.run(spec).await.unwrap();
        let path = handle.control_socket().unwrap();
        let result = handle.wait().await.unwrap();
        assert_eq!(result.exit_code, Some(0), "{result:?}");
        assert!(
            crate::RunRecord::read(temp.path())
                .unwrap()
                .live_control
                .is_none()
        );
        removed(&path).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn protocol_admission_ignores_caller_binary_but_names_incompatible_owner() {
        let handle = mock().await;
        let path = handle.control_socket().unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        let mut endpoint = super::super::host_transport::LiveControlRecord {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            protocol: super::super::host_transport::INSTANCE_CONTROL_PROTOCOL.into(),
            binary: PathBuf::from("/previous/pvisor"),
            binary_digest: "different-binary-digest".into(),
            pid: std::process::id(),
        };
        let job = handle.run_id().to_string();
        let attempt = handle.attempt_id().to_string();
        let cwd = std::env::current_dir().unwrap();
        let copied = endpoint.clone();
        tokio::task::spawn_blocking(move || {
            copied.exchange(
                &job,
                &attempt,
                super::super::host_transport::InstanceCommand::Status {},
            )
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(std::env::current_dir().unwrap(), cwd);
        endpoint.protocol = "future-owner-2".into();
        let error = endpoint.connect().unwrap_err().to_string();
        assert!(
            error.contains("/previous/pvisor") && error.contains("future-owner-2"),
            "{error}"
        );
        handle.cancel();
        handle.wait().await.unwrap();
    }

    #[tokio::test]
    async fn recorded_socket_replacement_is_never_connected() {
        let server = InstanceControlServer::bind(
            None,
            "replacement-test",
            &uuid::Uuid::new_v4().to_string(),
        )
        .unwrap();
        let record = server.record().unwrap();
        assert!(record.connect().is_ok());
        std::fs::remove_file(&record.path).unwrap();
        let replacement = super::super::host_transport::bind_instance_socket(&record.path).unwrap();
        assert!(
            record
                .connect()
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
        drop(server);
        assert!(record.path.exists());
        drop(replacement);
        std::fs::remove_file(&record.path).unwrap();
    }

    struct MockVm;
    #[async_trait::async_trait]
    impl RunExecutor for MockVm {
        fn descriptor(&self) -> ExecutorPlan {
            let mut plan = ProcessExecutor::default().descriptor();
            plan.kind = ExecutorKind::VirtualMachine;
            plan.isolation = IsolationKind::VirtualMachine;
            plan
        }
        fn supports(&self, _: &RunInvocation) -> bool {
            true
        }
        async fn execute(&self, session: &Session) -> crate::ExecutorOutput {
            ProcessExecutor::default().execute(session).await
        }
    }

    async fn mock() -> RunHandle {
        mock_at(None).await
    }

    async fn mock_at(path: Option<&Path>) -> RunHandle {
        let mut spec = RunSpec::process("control-test", "test", "/bin/sleep");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["60".into()];
        spec.runtime.timeout_ms = Some(10_000);
        // The process-backed fixture does not consume a VM network transport.
        // Disable that driver rather than claiming attachment support it lacks.
        let mut builder = PVisor::builder().executors(vec![Arc::new(MockVm)]).network(
            crate::NetworkDriverConfig {
                mode: crate::OverlayNetMode::Off,
                ..Default::default()
            },
        );
        if let Some(path) = path {
            builder = builder.control_socket(path);
        }
        builder.build().run(spec).await.unwrap()
    }

    fn request(handle: &RunHandle, command: HostVmCommand) -> AgentCtlHostRequest<HostVmCommand> {
        AgentCtlHostRequest {
            version: AGENTCTL_HOST_VERSION,
            request_id: uuid::Uuid::new_v4().to_string(),
            target: Some(AgentCtlTarget {
                job_id: handle.run_id().to_string(),
                attempt_id: Some(handle.attempt_id().to_string()),
                generation: None,
            }),
            command,
        }
    }

    async fn exchange(
        path: &std::path::Path,
        request: &AgentCtlHostRequest<HostVmCommand>,
    ) -> HostVmResult {
        super::exchange(path, request)
            .await
            .unwrap()
            .result
            .unwrap()
    }

    async fn removed(path: &std::path::Path) {
        tokio::time::timeout(IO_TIMEOUT, async {
            while path.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!path.parent().unwrap().exists());
    }

    #[tokio::test]
    async fn mock_without_network_attachment_still_rejects_default_vm_network() {
        let result = PVisor::builder()
            .executors(vec![Arc::new(MockVm)])
            .build()
            .run(RunSpec::process(
                "mock-network-admission",
                "test",
                "/bin/true",
            ))
            .await;
        assert!(matches!(result,
            Err(super::super::run::PVisorError::InvalidSpec(message))
                if message.contains("does not support pVisor VM network attachments")
        ));
    }

    struct PanickingVm {
        trigger: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl RunExecutor for PanickingVm {
        fn descriptor(&self) -> ExecutorPlan {
            MockVm.descriptor()
        }
        fn supports(&self, _: &RunInvocation) -> bool {
            true
        }
        async fn execute(&self, session: &Session) -> crate::ExecutorOutput {
            session.transition(RunState::Running, None).await;
            self.trigger.notified().await;
            panic!("intentional mock attempt panic");
        }
    }

    #[tokio::test]
    async fn attempt_task_panic_removes_endpoint_without_terminal_status() {
        let trigger = Arc::new(tokio::sync::Notify::new());
        let handle = PVisor::builder()
            .executors(vec![Arc::new(PanickingVm {
                trigger: trigger.clone(),
            })])
            .network(crate::NetworkDriverConfig {
                mode: crate::OverlayNetMode::Off,
                ..Default::default()
            })
            .build()
            .run(RunSpec::process("panicking-control", "test", "/bin/true"))
            .await
            .unwrap();
        let path = handle.control_socket().unwrap();
        // Exercise cleanup after the server has already entered its accept
        // loop, not just an attempt that finished before the server was polled.
        exchange(&path, &request(&handle, HostVmCommand::Status)).await;
        trigger.notify_one();
        assert!(
            matches!(handle.wait().await, Err(super::super::run::PVisorError::Join(error)) if error.is_panic())
        );
        removed(&path).await;
    }

    #[tokio::test]
    async fn automatic_and_nested_custom_endpoints_exclude_the_whole_authority_root() {
        let root = host_authority_root().unwrap();
        let automatic =
            InstanceControlServer::bind(None, "bind-test", &uuid::Uuid::new_v4().to_string())
                .unwrap();
        assert_eq!(automatic.directory(), root);
        assert_eq!(
            automatic
                .socket
                .path
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .parent(),
            Some(root.join("jobs").as_path())
        );
        let sibling = allocate_host_directory("exec-").unwrap();
        let nested = InstanceControlServer::bind(
            Some(&sibling.path().join("custom.sock")),
            "bind-test",
            "attempt-test",
        )
        .unwrap();
        assert_eq!(nested.directory(), root);
        let path = automatic.socket.path.clone();
        drop(automatic);
        assert!(!path.exists());
        assert!(nested.socket.path.exists());
        assert!(root.is_dir());
    }

    #[tokio::test]
    async fn custom_path_permissions_create_only_and_inode_safe_cleanup() {
        let directory = allocate_host_directory("control-test-").unwrap();
        let path = directory.path().join("custom.sock");
        let server = InstanceControlServer::bind(Some(&path), "bind-test", "attempt-test").unwrap();
        assert_eq!(server.directory(), host_authority_root().unwrap());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(InstanceControlServer::bind(Some(&path), "bind-test", "attempt-test").is_err());
        drop(server);
        assert!(!path.exists());
        assert!(directory.path().is_dir());

        let server = InstanceControlServer::bind(Some(&path), "bind-test", "attempt-test").unwrap();
        std::fs::remove_file(&path).unwrap();
        let replacement = UnixListener::bind(&path).unwrap();
        let replacement_inode = std::fs::symlink_metadata(&path).unwrap().ino();
        drop(server);
        assert_eq!(
            std::fs::symlink_metadata(&path).unwrap().ino(),
            replacement_inode
        );
        drop(replacement);
        std::fs::remove_file(&path).unwrap();

        let server = InstanceControlServer::bind(Some(&path), "bind-test", "attempt-test").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("missing-target", &path).unwrap();
        drop(server);
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[tokio::test]
    async fn custom_path_rejects_missing_public_and_symlink_parents_and_existing_files() {
        let directory = allocate_host_directory("control-test-").unwrap();
        assert!(
            InstanceControlServer::bind(
                Some(&directory.path().join("missing/socket")),
                "bind-test",
                "attempt-test"
            )
            .is_err()
        );
        let public = directory.path().join("public");
        std::fs::create_dir(&public).unwrap();
        std::fs::set_permissions(&public, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            InstanceControlServer::bind(Some(&public.join("socket")), "bind-test", "attempt-test")
                .is_err()
        );
        let alias = directory.path().join("alias");
        std::os::unix::fs::symlink(directory.path(), &alias).unwrap();
        assert!(
            InstanceControlServer::bind(Some(&alias.join("socket")), "bind-test", "attempt-test")
                .is_err()
        );
        let path = directory.path().join("existing");
        std::fs::write(&path, b"keep-me").unwrap();
        assert!(InstanceControlServer::bind(Some(&path), "bind-test", "attempt-test").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep-me");
        let link = directory.path().join("existing-link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(InstanceControlServer::bind(Some(&link), "bind-test", "attempt-test").is_err());
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[tokio::test]
    async fn custom_sdk_path_client_exchange_and_terminal_cleanup() {
        let directory = allocate_host_directory("control-test-").unwrap();
        let path = directory
            .path()
            .canonicalize()
            .unwrap()
            .join("configured.sock");
        let handle = mock_at(Some(&path)).await;
        assert_eq!(handle.control_socket().unwrap(), path);
        let req = request(&handle, HostVmCommand::Status);
        super::exchange(&path, &req).await.unwrap().result.unwrap();
        let mut invalid = req;
        invalid.target.as_mut().unwrap().attempt_id = Some("other-attempt".into());
        assert!(
            super::exchange(&path, &invalid)
                .await
                .unwrap()
                .result
                .unwrap_err()
                .to_string()
                .contains("identity")
        );
        handle.cancel();
        handle.wait().await.unwrap();
        tokio::time::timeout(IO_TIMEOUT, async {
            while path.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(directory.path().is_dir());
    }

    #[tokio::test]
    async fn response_timeout_and_disconnect_keep_the_control_waiter_admitted() {
        let handle = mock().await;
        handle.controls().wait_ready().await.unwrap();
        let transition = handle.control.transition_for_test().lock().await;
        let (mut client, server) = UnixStream::pair().unwrap();
        let mut bytes = serde_json::to_vec(&request(&handle, HostVmCommand::Resume)).unwrap();
        bytes.push(b'\n');
        client.write_all(&bytes).await.unwrap();
        let waiter = tokio::spawn(serve(
            server,
            handle.controls(),
            handle.status.clone(),
            None,
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(AtomicBool::new(false)),
            Duration::from_millis(1),
        ));
        let response: AgentCtlHostResponse<HostVmResult> =
            tokio::time::timeout(IO_TIMEOUT, read_host_frame(&mut client))
                .await
                .unwrap()
                .unwrap();
        let error = response.result.unwrap_err();
        assert_eq!(error.code, AgentCtlHostErrorCode::Unavailable);
        assert!(error.message.contains("timed out"));
        drop(client);
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(transition);
        tokio::time::timeout(IO_TIMEOUT, waiter)
            .await
            .unwrap()
            .unwrap();
        handle.cancel();
        handle.wait().await.unwrap();
    }

    #[tokio::test]
    async fn disconnected_operation_storm_cannot_exceed_eight_pending_controls() {
        let handle = mock().await;
        handle.controls().wait_ready().await.unwrap();
        let path = handle.control_socket().unwrap();
        let transition = handle.control.transition_for_test().lock().await;
        let mut bytes = serde_json::to_vec(&request(&handle, HostVmCommand::Pause)).unwrap();
        bytes.push(b'\n');
        for _ in 0..MAX_CONNECTIONS {
            let mut client = connect(&path).await.unwrap();
            client.write_all(&bytes).await.unwrap();
            drop(client);
        }
        // All preceding accepted operations are blocked on the transition lock.
        // Closing their clients must not permit this ninth connection to enter.
        let mut ninth = connect(&path).await.unwrap();
        let rejected = tokio::time::timeout(
            Duration::from_secs(1),
            read_host_frame::<serde_json::Value>(&mut ninth),
        )
        .await;
        drop(transition);
        handle.cancel();
        handle.wait().await.unwrap();
        assert!(rejected.unwrap().is_err());
        removed(&path).await;
    }

    #[tokio::test]
    async fn disabled_instance_control_creates_no_endpoint_or_registry_entry() {
        struct NoEndpointVm;
        #[async_trait::async_trait]
        impl RunExecutor for NoEndpointVm {
            fn descriptor(&self) -> ExecutorPlan {
                MockVm.descriptor()
            }
            fn supports(&self, _: &RunInvocation) -> bool {
                true
            }
            async fn execute(&self, session: &Session) -> crate::ExecutorOutput {
                assert!(
                    !session
                        .spec()
                        .metadata
                        .contains_key(INSTANCE_CONTROL_DIRECTORY_METADATA)
                );
                ProcessExecutor::default().execute(session).await
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let mut spec = RunSpec::process("disabled-control", "test", "/bin/sleep");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["60".into()];
        // Caller metadata must not substitute for runtime-owned exclusion state.
        spec.metadata.insert(
            INSTANCE_CONTROL_DIRECTORY_METADATA.into(),
            serde_json::to_value(directory.path()).unwrap(),
        );
        let handle = PVisor::builder()
            .executors(vec![Arc::new(NoEndpointVm)])
            .network(crate::NetworkDriverConfig {
                mode: crate::OverlayNetMode::Off,
                ..Default::default()
            })
            .instance_control(false)
            .build()
            .run(spec)
            .await
            .unwrap();
        assert!(handle.control_socket().is_err());
        let identity = (handle.run_id().to_string(), handle.attempt_id().to_string());
        assert!(!endpoints().lock().unwrap().contains_key(&identity));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        let service = handle.service();
        assert_eq!(service.status().attempt.attempt_id, *handle.attempt_id());
        // Wait for execution so the fixture's metadata assertion is exercised.
        handle.controls().wait_ready().await.unwrap();
        service
            .dispatch(HostAttemptCommand::Terminate)
            .await
            .unwrap();
        handle.wait().await.unwrap();
        assert!(!endpoints().lock().unwrap().contains_key(&identity));
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn disabled_instance_control_rejects_custom_socket_in_either_builder_order() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unused.sock");
        for builder in [
            PVisor::builder()
                .control_socket(&path)
                .instance_control(false),
            PVisor::builder()
                .instance_control(false)
                .control_socket(&path),
        ] {
            let result = builder
                .executors(vec![Arc::new(MockVm)])
                .build()
                .run(RunSpec::process(
                    "disabled-custom-control",
                    "test",
                    "/bin/true",
                ))
                .await;
            assert!(matches!(
                result,
                Err(super::super::run::PVisorError::InvalidSpec(_))
            ));
            assert!(!path.exists());
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        }
    }

    #[tokio::test]
    async fn configured_socket_rejects_non_vm_before_start() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unused.sock");
        let result = PVisor::builder()
            .control_socket(&path)
            .build()
            .run(RunSpec::process(
                "non-vm-custom-control",
                "test",
                "/bin/true",
            ))
            .await;
        assert!(matches!(
            result,
            Err(super::super::run::PVisorError::InvalidSpec(_))
        ));
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn mock_protocol_identity_errors_permissions_and_terminal_cleanup() {
        let mut handle = mock().await;
        let path = handle.control_socket().unwrap();
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Binding is create-only, not replacement.
        assert!(UnixListener::bind(&path).is_err());
        let req = request(&handle, HostVmCommand::Status);
        let reply = exchange(&path, &req).await;
        assert_eq!(reply.status.run_id, *handle.run_id());
        assert_eq!(reply.status.attempt.attempt_id, *handle.attempt_id());
        let mut invalid = req.clone();
        invalid.version = 2;
        assert!(
            super::exchange(&path, &invalid)
                .await
                .unwrap_err()
                .to_string()
                .contains("version")
        );
        invalid = req.clone();
        invalid.target.as_mut().unwrap().attempt_id = Some("other-attempt".into());
        assert!(
            super::exchange(&path, &invalid)
                .await
                .unwrap()
                .result
                .unwrap_err()
                .to_string()
                .contains("identity")
        );
        for (command, kind) in [
            (HostVmCommand::Pause, OperationKind::RunPause),
            (HostVmCommand::Resume, OperationKind::RunResume),
            (
                HostVmCommand::Offload { file: None },
                OperationKind::RunOffload { file: None },
            ),
        ] {
            assert_eq!(
                host_operation(&request(&handle, command), &handle.status()).unwrap(),
                HostAttemptCommand::Operation { kind }
            );
        }
        handle.controls().wait_ready().await.unwrap();
        let mut events = handle.subscribe_events();
        // No native endpoint is attached to the mock. Failure is explicit and
        // still follows the existing serialized lifecycle/event path.
        let error = super::exchange(&path, &request(&handle, HostVmCommand::Pause))
            .await
            .unwrap()
            .result
            .unwrap_err();
        assert_eq!(error.code, AgentCtlHostErrorCode::Unavailable);
        let mut kinds = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let pvisor_core::event::Fact::Observation { name, .. } = event.data {
                kinds.push(name);
            }
        }
        assert!(kinds.iter().any(|kind| kind == "vm.control_requested"));
        assert!(kinds.iter().any(|kind| kind == "vm.control_failed"));
        // Closing the client after submission does not cancel accepted control.
        let mut stream = connect(&path).await.unwrap();
        let mut bytes = serde_json::to_vec(&request(&handle, HostVmCommand::Resume)).unwrap();
        bytes.push(b'\n');
        stream.write_all(&bytes).await.unwrap();
        drop(stream);
        tokio::time::timeout(IO_TIMEOUT, async {
            loop {
                let event = events.recv().await.unwrap();
                if let pvisor_core::event::Fact::Observation { name, .. } = event.data
                    && name == "vm.control_failed"
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
        handle.cancel();
        while !handle.status().state.is_terminal() {
            handle.status_changed().await.unwrap();
        }
        assert!(handle.control_socket().is_err());
        handle.wait().await.unwrap();
        removed(&path).await;
    }

    #[tokio::test]
    async fn malformed_frames_and_connection_bounds_do_not_block_termination() {
        let handle = mock().await;
        let path = handle.control_socket().unwrap();
        let mut stream = connect(&path).await.unwrap();
        stream.write_all(b"not-json\n").await.unwrap();
        // No envelope/correlation ID can be recovered from malformed JSON.
        assert!(
            read_host_frame::<serde_json::Value>(&mut stream)
                .await
                .is_err()
        );
        let mut idle = Vec::new();
        for _ in 0..MAX_CONNECTIONS + 2 {
            idle.push(connect(&path).await.unwrap());
        }
        handle.cancel();
        tokio::time::timeout(IO_TIMEOUT, handle.wait())
            .await
            .unwrap()
            .unwrap();
        removed(&path).await;
        drop(idle);
    }

    #[tokio::test]
    async fn host_envelope_rejects_version_target_generation_and_guest_token() {
        let handle = mock().await;
        let path = handle.control_socket().unwrap();
        for variant in 0..5 {
            let mut request = request(&handle, HostVmCommand::Status);
            let expected = if variant == 0 {
                AgentCtlHostErrorCode::VersionMismatch
            } else if variant == 4 {
                AgentCtlHostErrorCode::InvalidRequest
            } else {
                AgentCtlHostErrorCode::Conflict
            };
            match variant {
                0 => request.version += 1,
                1 => request.target = None,
                2 => request.target.as_mut().unwrap().attempt_id = Some("stale-attempt".into()),
                3 => request.target.as_mut().unwrap().generation = Some("stale-generation".into()),
                _ => request.request_id = "invalid\nidentity".into(),
            }
            let mut stream = connect(&path).await.unwrap();
            write_host_frame(&mut stream, &request).await.unwrap();
            let response: AgentCtlHostResponse<HostVmResult> =
                read_host_frame(&mut stream).await.unwrap();
            assert_eq!(response.request_id, request.request_id);
            assert_eq!(response.result.unwrap_err().code, expected);
        }
        let mut guest_request =
            serde_json::to_value(request(&handle, HostVmCommand::Status)).unwrap();
        guest_request["token"] = "cooperative-guest-token".into();
        let mut stream = connect(&path).await.unwrap();
        write_host_frame(&mut stream, &guest_request).await.unwrap();
        assert!(
            read_host_frame::<serde_json::Value>(&mut stream)
                .await
                .is_err()
        );
        handle.cancel();
        handle.wait().await.unwrap();
    }

    #[tokio::test]
    async fn host_wire_refuses_legacy_frames_and_load_alias() {
        let handle = mock().await;
        let path = handle.control_socket().unwrap();
        let envelope = serde_json::to_value(request(&handle, HostVmCommand::Status)).unwrap();
        let mut legacy_command = envelope.clone();
        legacy_command["command"] = serde_json::json!({"command": "status", "file": null});
        let mut load = envelope.clone();
        load["command"] = serde_json::json!({"operation": "load"});
        let mut misplaced_file = envelope;
        misplaced_file["command"] = serde_json::json!({"operation": "pause", "file": "/tmp/ram"});
        for wire in [
            serde_json::json!({
                "version": AGENTCTL_HOST_VERSION,
                "run_id": handle.run_id(),
                "attempt_id": handle.attempt_id(),
                "command": "status", "file": null,
            }),
            legacy_command,
            load,
            misplaced_file,
        ] {
            let mut stream = connect(&path).await.unwrap();
            write_host_frame(&mut stream, &wire).await.unwrap();
            assert!(
                tokio::time::timeout(
                    IO_TIMEOUT,
                    read_host_frame::<serde_json::Value>(&mut stream)
                )
                .await
                .unwrap()
                .is_err()
            );
        }
        handle.cancel();
        handle.wait().await.unwrap();
    }

    #[tokio::test]
    async fn client_requires_explicit_attempt_target_before_connecting() {
        let handle = mock().await;
        let path = handle.control_socket().unwrap();
        for variant in 0..3 {
            let mut request = request(&handle, HostVmCommand::Status);
            match variant {
                0 => request.target = None,
                1 => request.target.as_mut().unwrap().attempt_id = None,
                _ => request.target.as_mut().unwrap().generation = Some("unsupported".into()),
            }
            assert!(super::exchange(&path, &request).await.is_err());
        }
        handle.cancel();
        handle.wait().await.unwrap();
    }

    #[tokio::test]
    async fn client_rejects_wrong_response_version_correlation_and_target() {
        let handle = mock().await;
        let request = request(&handle, HostVmCommand::Status);
        for variant in 0..4 {
            let directory = allocate_host_directory("fake-control-").unwrap();
            let path = directory.path().join("fake.sock");
            let listener = UnixListener::bind(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let mut status = handle.status();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request: AgentCtlHostRequest<HostVmCommand> =
                    read_host_frame(&mut stream).await.unwrap();
                if variant == 2 {
                    status.attempt.attempt_id = AttemptId::from("other-attempt");
                } else if variant == 3 {
                    status.run_id = pvisor_core::RunId::from("other-job");
                }
                let response = AgentCtlHostResponse {
                    version: if variant == 0 {
                        99
                    } else {
                        AGENTCTL_HOST_VERSION
                    },
                    request_id: if variant == 1 {
                        "other-request".into()
                    } else {
                        request.request_id
                    },
                    result: Ok(HostVmResult {
                        status,
                        value: None,
                    }),
                };
                write_host_frame(&mut stream, &response).await.unwrap();
            });
            assert!(super::exchange(&path, &request).await.is_err());
            server.await.unwrap();
        }
        handle.cancel();
        handle.wait().await.unwrap();
    }

    #[tokio::test]
    async fn non_vm_discovery_is_explicit() {
        let handle = PVisor::new()
            .run(RunSpec::process("non-vm-control", "test", "/bin/true"))
            .await
            .unwrap();
        assert!(
            handle
                .control_socket()
                .unwrap_err()
                .to_string()
                .contains("VM executor")
        );
        handle.wait().await.unwrap();
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[tokio::test]
    #[ignore = "real KVM, static C compiler; PVISOR_TEST_BINARY and PVISOR_TEST_LIBRARY_DIR for GNU firmware"]
    async fn native_socket_pause_resume_offload_same_attempt() {
        let directory = tempfile::tempdir().unwrap();
        let rootfs = directory.path().join("rootfs");
        for name in ["bin", "dev", "proc", "tmp"] {
            std::fs::create_dir_all(rootfs.join(name)).unwrap();
        }
        let source = directory.path().join("probe.c");
        std::fs::write(&source, "#include <stdio.h>\n#include <unistd.h>\nint main(void) { puts(\"START-ONCE\"); fflush(stdout); for (;;) usleep(100000); }\n").unwrap();
        assert!(
            std::process::Command::new("cc")
                .args(["-O2", "-static"])
                .arg(&source)
                .arg("-o")
                .arg(rootfs.join("bin/probe"))
                .status()
                .unwrap()
                .success()
        );
        // A unit-test executable cannot dispatch pVisor's internal VM runner;
        // launch a real CLI process to exercise the native control endpoint.
        let binary = PathBuf::from(
            std::env::var_os("PVISOR_TEST_BINARY")
                .expect("set PVISOR_TEST_BINARY to a built pvisor executable"),
        );
        let stage = directory.path().join("stage");
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let mut command = std::process::Command::new(&binary);
        command
            .arg("run")
            .args([
                "--executor",
                "vm",
                "--overlaynet",
                "off",
                "--memory",
                "256MiB",
                "--stdio",
                "capture",
                "--rootfs",
            ])
            .arg(&rootfs)
            .arg("--stage")
            .arg(&stage)
            .arg("--vm-ram-backing")
            .arg(directory.path().join("ram"))
            .current_dir(&workspace)
            .stdout(std::process::Stdio::null())
            .stderr(std::fs::File::create(directory.path().join("native.log")).unwrap());
        if let Some(firmware) = std::env::var_os("PVISOR_TEST_LIBRARY_DIR") {
            command.arg("--vm-library-dir").arg(firmware);
        }
        command.args(["--", "/bin/probe"]);
        struct NativeChild {
            child: std::process::Child,
            binary: PathBuf,
            stage: PathBuf,
        }
        impl Drop for NativeChild {
            fn drop(&mut self) {
                if self.child.try_wait().ok().flatten().is_none() {
                    let _ = std::process::Command::new(&self.binary)
                        .arg("kill")
                        .arg(&self.stage)
                        .output();
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                }
            }
        }
        let mut native = NativeChild {
            child: command.spawn().unwrap(),
            binary,
            stage: stage.clone(),
        };
        let (path, base_request) = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                assert!(
                    native.child.try_wait().unwrap().is_none(),
                    "native CLI exited; inspect native.log"
                );
                if let Ok(record) = super::super::registry::RunRecord::read(&stage)
                    && let Some(attempt_id) = record.attempt_id
                {
                    let request = AgentCtlHostRequest {
                        version: AGENTCTL_HOST_VERSION,
                        request_id: uuid::Uuid::new_v4().to_string(),
                        target: Some(AgentCtlTarget {
                            job_id: record.run_id,
                            attempt_id: Some(attempt_id),
                            generation: None,
                        }),
                        command: HostVmCommand::Status,
                    };
                    if let Some(endpoint) = record.live_control {
                        let response = super::exchange(&endpoint.path, &request)
                            .await
                            .ok()
                            .and_then(|reply| reply.result.ok());
                        if response
                            .is_some_and(|response| response.status.state == RunState::Running)
                        {
                            return (endpoint.path, request);
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        for command in [
            HostVmCommand::Pause,
            HostVmCommand::Resume,
            HostVmCommand::Offload { file: None },
            HostVmCommand::Resume,
            HostVmCommand::Status,
        ] {
            let mut req = base_request.clone();
            req.command = command.clone();
            let reply = exchange(&path, &req).await;
            assert_eq!(
                reply.status.attempt.attempt_id.as_str(),
                req.target.as_ref().unwrap().attempt_id.as_deref().unwrap()
            );
            if command == (HostVmCommand::Offload { file: None }) {
                assert_eq!(reply.status.state, RunState::Suspended);
                assert!(matches!(
                    reply.value,
                    Some(pvisor_core::operation::Value::Vm {
                        memory: Some(_),
                        ..
                    })
                ));
            }
            if command == HostVmCommand::Resume {
                assert_eq!(reply.status.state, RunState::Running);
            }
        }
        let output = std::process::Command::new(&native.binary)
            .arg("kill")
            .arg(&stage)
            .output()
            .unwrap();
        assert!(output.status.success());
        tokio::time::timeout(IO_TIMEOUT, async {
            while native.child.try_wait().unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        removed(&path).await;
    }
}
