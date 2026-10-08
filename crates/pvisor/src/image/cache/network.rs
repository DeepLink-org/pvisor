//! Preserve host cache connectivity while the VM runner has private networking.
//! Only the already pinned image's stat/list/read requests can cross this socket.
use super::{
    CacheClient, Request, Response,
    client::ClientBinding,
    protocol::{Envelope, read_frame, write_frame},
};
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

const INTERNAL_ENV: &str = "PVISOR_IMAGE_NETWORK_ACCESS";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Spec {
    client: ClientBinding,
    handle: String,
    socket: PathBuf,
}

pub(super) struct NetworkAccess {
    child: Child,
    _directory: tempfile::TempDir,
    _descriptor: tempfile::NamedTempFile,
}
impl Drop for NetworkAccess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl NetworkAccess {
    pub(super) fn start(
        parent: &Path,
        client: ClientBinding,
        handle: &str,
    ) -> anyhow::Result<(Self, ClientBinding)> {
        // Keep sun_path short even when the image store has a long pathname.
        // Only the socket lives here; credentials remain in the hidden owner.
        let directory = tempfile::Builder::new()
            .prefix("pvisor-image-net-")
            .tempdir_in("/tmp")?;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
        let socket = directory.path().join("cache.sock");
        let mut descriptor = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(
            descriptor.as_file_mut(),
            &Spec {
                client,
                handle: handle.into(),
                socket: socket.clone(),
            },
        )?;
        descriptor.flush()?;
        let mut command = Command::new(std::env::current_exe()?);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            let parent_pid = std::process::id() as libc::pid_t;
            // SAFETY: the child hook uses only async-signal-safe syscalls.
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::getppid() != parent_pid {
                        return Err(io::Error::from_raw_os_error(libc::ESRCH));
                    }
                    Ok(())
                });
            }
        }
        let child = command
            .env(INTERNAL_ENV, descriptor.path())
            .env_remove("PVISOR_KRUN_RUNNER_SPEC")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut owner = Self {
            child,
            _directory: directory,
            _descriptor: descriptor,
        };
        let mut ready = String::new();
        io::BufReader::new(
            owner
                .child
                .stdout
                .take()
                .context("missing cache access readiness pipe")?,
        )
        .read_line(&mut ready)?;
        ensure!(
            ready == "ready\n",
            "host image cache access failed to start"
        );
        Ok((owner, ClientBinding::unix(&socket)))
    }
}

fn forward(client: &CacheClient, handle: &str, request: Request) -> (Response, Vec<u8>) {
    if matches!(request, Request::Ping) {
        return (Response::Ready, vec![]);
    }
    let pinned = match &request {
        Request::Stat { digest, .. }
        | Request::List { digest, .. }
        | Request::Read { digest, .. } => digest == handle,
        _ => false,
    };
    if !pinned {
        return (
            Response::Error {
                code: "permission_denied".into(),
                message: "only reads of the bound immutable image are allowed".into(),
            },
            vec![],
        );
    }
    client.request(request).unwrap_or_else(|error| {
        let code = match error.downcast_ref::<io::Error>().map(io::Error::kind) {
            Some(io::ErrorKind::NotFound) => "not_found",
            Some(io::ErrorKind::PermissionDenied) => "permission_denied",
            _ => "request_failed",
        };
        (
            Response::Error {
                code: code.into(),
                message: error.to_string(),
            },
            vec![],
        )
    })
}
fn connection(mut socket: UnixStream, client: &CacheClient, handle: &str) -> anyhow::Result<()> {
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    socket.set_write_timeout(Some(Duration::from_secs(300)))?;
    let envelope: Envelope = read_frame(&mut socket)?;
    ensure!(envelope.version == 1, "unsupported image cache protocol");
    let (response, body) = forward(client, handle, envelope.request);
    write_frame(&mut socket, &response)?;
    socket.write_all(&body)?;
    Ok(())
}

pub(crate) fn run_internal_if_requested() -> anyhow::Result<bool> {
    let Some(path) = std::env::var_os(INTERNAL_ENV) else {
        return Ok(false);
    };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0
            && metadata.len() < 1024 * 1024,
        "unsafe host image access descriptor"
    );
    let spec: Spec = serde_json::from_reader(file)?;
    let client = CacheClient::from_binding(spec.client)?;
    let listener = UnixListener::bind(&spec.socket)?;
    fs::set_permissions(&spec.socket, fs::Permissions::from_mode(0o600))?;
    let (send, receive) = mpsc::sync_channel::<UnixStream>(16);
    let receive = Arc::new(Mutex::new(receive));
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let receive = receive.clone();
            let client = &client;
            let handle = &spec.handle;
            scope.spawn(move || {
                loop {
                    let Ok(socket) = receive.lock().unwrap().recv() else {
                        break;
                    };
                    if let Err(error) = connection(socket, client, handle) {
                        tracing::debug!("host image access: {error:#}");
                    }
                }
            });
        }
        println!("ready");
        io::stdout().flush()?;
        for socket in listener.incoming() {
            let _ = send.try_send(socket?);
        }
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn access_is_confined_to_pinned_reads_and_preserves_verified_bytes() {
        let (_temp, server, client, digest) = crate::image::cache::backend::tests::fixture();
        for request in [
            Request::Prepare {
                image: "anything".into(),
                architecture: "amd64".into(),
                refresh: false,
            },
            Request::Open {
                handle: digest.clone(),
                architecture: "amd64".into(),
            },
            Request::Read {
                digest: "another image".into(),
                path: b"large".to_vec(),
                offset: 0,
                length: 16,
            },
        ] {
            assert!(
                matches!(forward(&client, &digest, request).0, Response::Error { code, .. } if code == "permission_denied")
            );
        }
        assert_eq!(server.reads.load(std::sync::atomic::Ordering::Relaxed), 0);
        let (response, body) = forward(
            &client,
            &digest,
            Request::Read {
                digest: digest.clone(),
                path: b"large".to_vec(),
                offset: 0,
                length: 16,
            },
        );
        assert!(matches!(response, Response::Data { length: 16, .. }));
        assert_eq!(body, [42; 16]);
    }
}
