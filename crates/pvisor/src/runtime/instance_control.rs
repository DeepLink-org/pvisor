//! Host-only, attempt-scoped live VM control. One JSON line per connection.
//!
//! This is not snapshot restart: `resume` resumes the same live Attempt. Endpoint
//! discovery is process-local so no management socket is projected into a guest.

use super::host_transport::{
    allocate_host_directory, authorize_host_peer, host_authority_root, read_host_frame,
    validate_host_target, write_host_frame,
};
use super::run::{AttemptService, RunControlHandle, RunHandle};
#[cfg(test)]
use pvisor_core::AttemptId;
use pvisor_core::RunStatus;
#[cfg(test)]
use pvisor_core::host_protocol::{AGENTCTL_HOST_MAX_FRAME_BYTES, AgentCtlTarget};
use pvisor_core::host_protocol::{
    AGENTCTL_HOST_VERSION, AgentCtlHostError, AgentCtlHostErrorCode, AgentCtlHostRequest,
    AgentCtlHostResponse, HostAttemptCommand, HostVmCommand, HostVmResult,
};
#[cfg(test)]
use pvisor_core::operation::{OperationKind, Value};
use std::collections::HashMap;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
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
    temporary: Option<tempfile::TempDir>,
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
        // Do not recursively delete a replacement socket or any new contents.
        if let Some(directory) = self.temporary.take() {
            let path = directory.keep();
            let _ = std::fs::remove_dir(path);
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
    pub(crate) fn bind(path: Option<&Path>) -> anyhow::Result<Self> {
        // Fixed short default path also fits macOS sockaddr_un. Custom parents
        // must already exist; never create them, follow a parent symlink, or
        // unlink an existing entry to make binding succeed.
        let authority_root = host_authority_root()?;
        let temporary = if path.is_none() {
            Some(allocate_host_directory("vm-")?)
        } else {
            None
        };
        let path = match path {
            Some(path) => {
                anyhow::ensure!(
                    path.file_name().is_some(),
                    "control socket needs a file name"
                );
                let absolute = std::path::absolute(path)?;
                let parent = absolute
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("control socket needs a parent directory"))?;
                let metadata = std::fs::symlink_metadata(parent)?;
                anyhow::ensure!(
                    metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
                    "control socket parent must be a non-symlink directory"
                );
                anyhow::ensure!(
                    metadata.uid() == unsafe { libc::geteuid() },
                    "control socket parent must belong to the effective user"
                );
                anyhow::ensure!(
                    metadata.mode() & 0o777 == 0o700,
                    "control socket parent must have mode 0700"
                );
                let canonical = parent.canonicalize()?;
                let checked = std::fs::symlink_metadata(&canonical)?;
                anyhow::ensure!(
                    metadata.dev() == checked.dev() && metadata.ino() == checked.ino(),
                    "control socket parent changed during validation"
                );
                canonical.join(absolute.file_name().unwrap())
            }
            None => temporary.as_ref().unwrap().path().join("ctrl.sock"),
        };
        // Exclude all authority siblings when this endpoint is in the common
        // root. External custom sockets retain their checked parent exclusion.
        let exclusion_directory = if path.starts_with(&authority_root) {
            authority_root
        } else {
            path.parent().unwrap().to_owned()
        };
        let listener = UnixListener::bind(&path)?;
        let metadata = std::fs::symlink_metadata(&path)?;
        let socket = OwnedSocket {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            temporary,
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

    pub(crate) fn directory(&self) -> &Path {
        &self.exclusion_directory
    }

    pub(crate) fn start(mut self, handle: &RunHandle) {
        let identity = (handle.run_id().to_string(), handle.attempt_id().to_string());
        endpoints()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(identity.clone(), self.socket.path.clone());
        self.identity = Some(identity);
        let controls = handle.controls();
        let mut status = handle.status.clone();
        let attempt_task = handle.join.abort_handle();
        tokio::spawn(async move {
            let mut clients = JoinSet::new();
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
                    _ = clients.join_next(), if !clients.is_empty() => {}
                    accepted = self.listener.accept() => {
                        let Ok((stream, _)) = accepted else { break; };
                        if clients.len() >= MAX_CONNECTIONS || authorize_host_peer(&stream).is_err() { continue; }
                        let controls = controls.clone();
                        let status = status.clone();
                        clients.spawn(async move { serve(stream, controls, status, OPERATION_TIMEOUT).await; });
                    }
                }
            }
            // Drop aborts client waiters without waiting for I/O or controls.
            // RunControlHandle owns accepted transitions independently.
            drop(clients);
            drop(self);
        });
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
    operation_timeout: Duration,
) {
    if authorize_host_peer(&stream).is_err() {
        return;
    }
    // A malformed frame has no trustworthy correlation ID; close without a reply.
    let Ok(Ok(request)) = tokio::time::timeout(
        IO_TIMEOUT,
        read_host_frame::<AgentCtlHostRequest<HostVmCommand>>(&mut stream),
    )
    .await
    else {
        return;
    };
    let request_id = request.request_id.clone();
    let result = host_operation(&request, &status.borrow().clone());
    let result = match result {
        Ok(command) => {
            let service = AttemptService::new(controls);
            let operation = service.dispatch(command);
            tokio::pin!(operation);
            match tokio::time::timeout(operation_timeout, &mut operation).await {
                Ok(result) => result.map(HostVmResult::from),
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
                    // Keep this JoinSet slot until the existing control returns.
                    // A timed-out/disconnected client cannot free admission and
                    // accumulate unbounded detached transition waiters.
                    drop(stream);
                    let _ = operation.await;
                    return;
                }
            }
        }
        Err(error) => Err(error),
    };
    respond(&mut stream, &request_id, result).await;
}

async fn respond(
    stream: &mut UnixStream,
    request_id: &str,
    result: Result<HostVmResult, AgentCtlHostError>,
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
    let mut stream = tokio::time::timeout(IO_TIMEOUT, UnixStream::connect(path))
        .await
        .map_err(|_| anyhow::anyhow!("control connection timed out"))??;
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
    use crate::{PVisor, RunExecutor, Session};
    use pvisor_core::{
        ExecutorKind, ExecutorPlan, IsolationKind, RunInvocation, RunSpec, RunState,
    };
    use std::sync::Arc;

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
        let automatic = InstanceControlServer::bind(None).unwrap();
        assert_eq!(automatic.directory(), root);
        assert_eq!(
            automatic.socket.path.parent().unwrap().parent(),
            Some(root.as_path())
        );
        let sibling = allocate_host_directory("exec-").unwrap();
        let nested =
            InstanceControlServer::bind(Some(&sibling.path().join("custom.sock"))).unwrap();
        assert_eq!(nested.directory(), root);
        let path = automatic.socket.path.clone();
        drop(automatic);
        assert!(!path.exists());
        assert!(nested.socket.path.exists());
        assert!(root.is_dir());
    }

    #[tokio::test]
    async fn custom_path_permissions_create_only_and_inode_safe_cleanup() {
        let directory = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let path = directory.path().join("custom.sock");
        let server = InstanceControlServer::bind(Some(&path)).unwrap();
        assert_eq!(server.directory(), directory.path().canonicalize().unwrap());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(InstanceControlServer::bind(Some(&path)).is_err());
        drop(server);
        assert!(!path.exists());
        assert!(directory.path().is_dir());

        let server = InstanceControlServer::bind(Some(&path)).unwrap();
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

        let server = InstanceControlServer::bind(Some(&path)).unwrap();
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
        let directory = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        assert!(
            InstanceControlServer::bind(Some(&directory.path().join("missing/socket"))).is_err()
        );
        let public = directory.path().join("public");
        std::fs::create_dir(&public).unwrap();
        std::fs::set_permissions(&public, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(InstanceControlServer::bind(Some(&public.join("socket"))).is_err());
        let alias = directory.path().join("alias");
        std::os::unix::fs::symlink(directory.path(), &alias).unwrap();
        assert!(InstanceControlServer::bind(Some(&alias.join("socket"))).is_err());
        let path = directory.path().join("existing");
        std::fs::write(&path, b"keep-me").unwrap();
        assert!(InstanceControlServer::bind(Some(&path)).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep-me");
        let link = directory.path().join("existing-link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(InstanceControlServer::bind(Some(&link)).is_err());
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[tokio::test]
    async fn custom_sdk_path_client_exchange_and_terminal_cleanup() {
        let directory = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
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
        let transition = handle.vm_control.transition.lock().await;
        let (mut client, server) = UnixStream::pair().unwrap();
        let mut bytes = serde_json::to_vec(&request(&handle, HostVmCommand::Resume)).unwrap();
        bytes.push(b'\n');
        client.write_all(&bytes).await.unwrap();
        let waiter = tokio::spawn(serve(
            server,
            handle.controls(),
            handle.status.clone(),
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
        let transition = handle.vm_control.transition.lock().await;
        let mut bytes = serde_json::to_vec(&request(&handle, HostVmCommand::Pause)).unwrap();
        bytes.push(b'\n');
        for _ in 0..MAX_CONNECTIONS {
            let mut client = UnixStream::connect(&path).await.unwrap();
            client.write_all(&bytes).await.unwrap();
            drop(client);
        }
        // All preceding accepted operations are blocked on the transition lock.
        // Closing their clients must not permit this ninth connection to enter.
        let mut ninth = UnixStream::connect(&path).await.unwrap();
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
        let mut stream = UnixStream::connect(&path).await.unwrap();
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
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream.write_all(b"not-json\n").await.unwrap();
        // No envelope/correlation ID can be recovered from malformed JSON.
        assert!(
            read_host_frame::<serde_json::Value>(&mut stream)
                .await
                .is_err()
        );
        let mut idle = Vec::new();
        for _ in 0..MAX_CONNECTIONS + 2 {
            idle.push(UnixStream::connect(&path).await.unwrap());
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
    async fn frame_limit_and_single_request_framing() {
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        writer.write_all(b"1\n2\n").await.unwrap();
        assert_eq!(read_host_frame::<u32>(&mut reader).await.unwrap(), 1);
        assert_eq!(read_host_frame::<u32>(&mut reader).await.unwrap(), 2);
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let sender = tokio::spawn(async move {
            writer
                .write_all(&vec![b'x'; AGENTCTL_HOST_MAX_FRAME_BYTES + 1])
                .await
                .unwrap();
        });
        assert!(
            read_host_frame::<serde_json::Value>(&mut reader)
                .await
                .unwrap_err()
                .to_string()
                .contains("too large")
        );
        sender.await.unwrap();
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
            let mut stream = UnixStream::connect(&path).await.unwrap();
            write_host_frame(&mut stream, &request).await.unwrap();
            let response: AgentCtlHostResponse<HostVmResult> =
                read_host_frame(&mut stream).await.unwrap();
            assert_eq!(response.request_id, request.request_id);
            assert_eq!(response.result.unwrap_err().code, expected);
        }
        let mut guest_request =
            serde_json::to_value(request(&handle, HostVmCommand::Status)).unwrap();
        guest_request["token"] = "cooperative-guest-token".into();
        let mut stream = UnixStream::connect(&path).await.unwrap();
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
            let mut stream = UnixStream::connect(&path).await.unwrap();
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
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("fake.sock");
            let listener = UnixListener::bind(&path).unwrap();
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
        // A unit-test executable cannot dispatch pVisor's internal VM runner.
        // Use one real CLI process, independent of the future ctrl frontend.
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
                    // Test-only discovery: production discovery is the SDK
                    // getter/CLI stderr, with no guest-visible stage locator.
                    for entry in std::fs::read_dir(host_authority_root().unwrap())
                        .unwrap()
                        .flatten()
                    {
                        if !entry.file_name().to_string_lossy().starts_with("vm-") {
                            continue;
                        }
                        let path = entry.path().join("ctrl.sock");
                        let probe = async {
                            let response =
                                super::exchange(&path, &request).await.ok()?.result.ok()?;
                            (response.status.state == RunState::Running).then_some(())
                        };
                        if tokio::time::timeout(Duration::from_millis(200), probe)
                            .await
                            .ok()
                            .flatten()
                            .is_some()
                        {
                            return (path, request);
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
                    Some(Value::Vm {
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
