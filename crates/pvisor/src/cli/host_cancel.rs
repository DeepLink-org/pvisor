//! Private, attempt-fenced cooperative cancellation for CLI-owned Runs.
//! This host-only endpoint is not written into the Job or guest discovery state.
use crate::runtime::host_transport::{authorize_host_peer, read_host_frame, write_host_frame};
use anyhow::{Context, ensure};
use pvisor_core::host_protocol::{
    AgentCtlHostError, AgentCtlHostErrorCode, AgentCtlHostRequest, AgentCtlHostResponse,
};
use serde::{Deserialize, Serialize};
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelRun {}
struct ActiveRun {
    record: crate::RunRecord,
    cancellation: tokio_util::sync::CancellationToken,
}
static ACTIVE: OnceLock<Arc<Mutex<Option<ActiveRun>>>> = OnceLock::new();
fn path(directory: &Path, pid: u32) -> PathBuf {
    directory.join(format!("cancel-{pid}.sock"))
}

pub(super) struct EndpointFile {
    path: PathBuf,
    identity: (u64, u64),
}
impl Drop for EndpointFile {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path).is_ok_and(|m| (m.dev(), m.ino()) == self.identity)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(super) struct Server {
    _file: EndpointFile,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) fn own_endpoint(directory: &Path, pid: u32) -> anyhow::Result<EndpointFile> {
    let path = path(directory, pid);
    let m = std::fs::symlink_metadata(&path)?;
    ensure!(
        m.file_type().is_socket()
            && m.uid() == unsafe { libc::geteuid() }
            && m.mode() & 0o7777 == 0o600,
        "unsafe worker cancellation endpoint"
    );
    Ok(EndpointFile {
        path,
        identity: (m.dev(), m.ino()),
    })
}

pub(super) fn start(rt: &tokio::runtime::Runtime, directory: &Path) -> anyhow::Result<Server> {
    let path = path(directory, std::process::id());
    let _enter = rt.enter();
    let listener = tokio::net::UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let file = own_endpoint(directory, std::process::id())?;
    let active = Arc::new(Mutex::new(None));
    ACTIVE
        .set(active.clone())
        .map_err(|_| anyhow::anyhow!("duplicate Run cancellation endpoint"))?;
    let admission = Arc::new(tokio::sync::Semaphore::new(8));
    let task = rt.spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            if authorize_host_peer(&stream).is_err() {
                continue;
            }
            let Ok(permit) = admission.clone().try_acquire_owned() else {
                continue;
            };
            let active = active.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let request = tokio::time::timeout(
                    Duration::from_secs(2),
                    read_host_frame::<AgentCtlHostRequest<CancelRun>>(&mut stream),
                )
                .await;
                let Ok(Ok(request)) = request else {
                    return;
                };
                let response = AgentCtlHostResponse {
                    version: pvisor_core::host_protocol::AGENTCTL_HOST_VERSION,
                    request_id: request.request_id.clone(),
                    result: (|| -> Result<(), AgentCtlHostError> {
                        request.validate()?;
                        let active = active.lock().unwrap_or_else(|e| e.into_inner());
                        let run = active.as_ref().ok_or_else(|| {
                            AgentCtlHostError::new(
                                AgentCtlHostErrorCode::Unavailable,
                                "Run not yet registered; no cancellation admitted",
                            )
                        })?;
                        let target = request.target.as_ref().ok_or_else(|| {
                            AgentCtlHostError::new(
                                AgentCtlHostErrorCode::InvalidRequest,
                                "cancellation requires an exact target",
                            )
                        })?;
                        let validate = || -> anyhow::Result<()> {
                            let template = crate::runtime::job_execution::Job::read(&run.record)?;
                            let _job_lease = template
                                .as_ref()
                                .map(crate::runtime::job_execution::Job::lock)
                                .transpose()?;
                            if let Some(template) = template {
                                template.current()?.validate_record_target(&run.record)?;
                            }
                            let current = crate::RunRecord::read(&run.record.stage_dir())?;
                            super::host::check_selected_record(&run.record, &current)?;
                            super::host::check_target(target, &current)?;
                            ensure!(
                                current.pid == std::process::id() && !current.state.is_stopped(),
                                "Run no longer owned by this cancellation endpoint"
                            );
                            run.cancellation.cancel();
                            super::host_service::notify_cancel(libc::SIGTERM);
                            Ok(())
                        };
                        validate().map_err(|e| {
                            e.downcast_ref::<AgentCtlHostError>()
                                .cloned()
                                .unwrap_or_else(|| {
                                    AgentCtlHostError::new(
                                        AgentCtlHostErrorCode::Conflict,
                                        e.to_string(),
                                    )
                                })
                        })
                    })(),
                };
                let _ = tokio::time::timeout(
                    Duration::from_secs(2),
                    write_host_frame(&mut stream, &response),
                )
                .await;
            });
        }
    });
    Ok(Server { _file: file, task })
}

pub(super) fn register(handle: &crate::RunHandle, stage: &Path) -> anyhow::Result<()> {
    let Some(active) = ACTIVE.get() else {
        return Ok(());
    };
    let record = crate::RunRecord::read(stage)?;
    ensure!(
        record.run_id == handle.run_id().as_str()
            && record.attempt_id.as_deref() == Some(handle.attempt_id().as_str())
            && record.pid == std::process::id(),
        "cancellation endpoint Run identity mismatch"
    );
    let mut active = active.lock().unwrap_or_else(|e| e.into_inner());
    ensure!(active.is_none(), "worker already owns a registered Run");
    *active = Some(ActiveRun {
        record,
        cancellation: handle.cancellation.clone(),
    });
    Ok(())
}

/// None is returned ONLY when no connection was established. Once submitted,
/// cancellation is never retried through a numeric-PID fallback.
pub(super) fn request(record: &crate::RunRecord) -> anyhow::Result<bool> {
    let Some(directory) = super::host_service::worker_directory() else {
        return Ok(false);
    };
    let path = path(directory, record.pid);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        metadata.file_type().is_socket()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o7777 == 0o600,
        "unsafe cooperative cancellation endpoint"
    );
    let record = record.clone();
    std::thread::spawn(move || -> anyhow::Result<bool> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async {
            let mut stream = match tokio::net::UnixStream::connect(&path).await {
                Ok(stream) => stream,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    return Ok(false);
                }
                Err(e) => return Err(e.into()),
            };
            authorize_host_peer(&stream)?;
            let after = std::fs::symlink_metadata(path)?;
            ensure!(
                (metadata.dev(), metadata.ino()) == (after.dev(), after.ino()),
                "cancellation endpoint changed during connect"
            );
            let id = uuid::Uuid::new_v4().to_string();
            let request = AgentCtlHostRequest {
                version: pvisor_core::host_protocol::AGENTCTL_HOST_VERSION,
                request_id: id.clone(),
                target: Some(pvisor_core::host_protocol::AgentCtlTarget {
                    job_id: record.run_id,
                    attempt_id: record.attempt_id,
                    generation: record.overlay.map(|o| o.generation.to_string()),
                }),
                command: CancelRun {},
            };
            request.validate()?;
            tokio::time::timeout(
                Duration::from_secs(2),
                write_host_frame(&mut stream, &request),
            )
            .await
            .context("cancellation submission timeout; effects may be ambiguous; not retried")?
            .context("cancellation submission failed; effects may be ambiguous; not retried")?;
            let response: AgentCtlHostResponse<()> =
                tokio::time::timeout(Duration::from_secs(2), read_host_frame(&mut stream))
                    .await
                    .context("cancellation reply timeout; effects may be ambiguous; not retried")?
                    .context("cancellation reply lost; effects may be ambiguous; not retried")?;
            response.validate(&id)?;
            response.result?;
            Ok(true)
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("cancellation endpoint client panicked"))?
}
