//! Host-only cooperative memory controls for the ordinary OCI VM, not a second
//! launcher or a Session adapter. Only an opted-in runner installs this endpoint.
//! The same-user scheduler owns admission and safe wait assertions. Direct exec,
//! attachment and background workloads must not bypass that cooperative owner.
use crate::host_transport::{
    authorize_host_peer_sync, host_authority_root, read_host_frame_sync, validate_instance_socket,
    write_host_frame_sync,
};
use anyhow::{Context, Result, ensure};
use pvisor_core::{
    host_protocol::HostVmCommand,
    operation::{VmMemory, VmState},
};
use pvisor_vm::api::VmControl;
mod backing;
pub(super) use backing::{Backing, OWNER_FD, ParentBacking};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// Exact container launch identity; never a Session/Execution identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// Full container ID, 64 hexadecimal characters.
    pub container_id: String,
    /// Launch generation, 32 hexadecimal characters; changes on every restart.
    pub generation: String,
}
impl Target {
    /// Reject incomplete identities before connecting or installing authority.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.container_id.len() == 64
                && self.container_id.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid container ID"
        );
        ensure!(
            self.generation.len() == 32 && self.generation.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid container generation"
        );
        Ok(())
    }
    /// Generation-scoped private host socket. This does not start a VM or import
    /// an image. A missing endpoint is an error, never permission to fall back.
    pub fn socket(&self) -> Result<PathBuf> {
        self.validate()?;
        Ok(host_authority_root()?.join(format!("docker-{}.sock", self.generation)))
    }
}

/// Bounded single-operation wire request. Mutation is never automatically retried.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Protocol version, currently 1.
    pub version: u32,
    /// Fresh UUID for response correlation, not permission to replay mutations.
    pub request_id: String,
    /// Exact live container launch.
    pub target: Target,
    /// Only Status, Offload {file: None}, and Resume are supported.
    pub command: HostVmCommand,
}
/// Acknowledged native state. Resume acknowledgement is not guest HTTP readiness;
/// the caller must check the existing guest channel before sending tools.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    /// Running, or Offloaded after native writeback/reclaim and, when selected,
    /// compressed generation publication. Publication failure never acknowledges.
    pub state: VmState,
    /// Best-effort native reclaim report; not a guaranteed physical saving.
    pub memory: Option<VmMemory>,
}
/// Correlated response. Failure may follow effects; retain reservations until
/// independent process/cgroup/mount teardown is confirmed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    /// Protocol version, currently 1.
    pub version: u32,
    /// Echo of the validated caller request ID.
    pub request_id: String,
    /// Echo of the exact live identity.
    pub target: Target,
    /// Native acknowledgement or bounded diagnostic.
    pub result: Result<Status, String>,
}

/// One authenticated exchange with a pinned same-user socket inode. 315-second
/// response bound; a lost reply is uncertain and must not be retried. Does not
/// launch, replace, or reauthenticate against a different generation.
pub fn exchange(
    path: &Path,
    device: u64,
    inode: u64,
    target: &Target,
    command: HostVmCommand,
) -> Result<Status> {
    target.validate()?;
    ensure!(
        path == target.socket()?,
        "unexpected container control path"
    );
    let check = || -> Result<()> {
        use std::os::unix::fs::FileTypeExt;
        validate_instance_socket(path)?;
        let m = fs::symlink_metadata(path)?;
        ensure!(
            m.file_type().is_socket()
                && m.uid() == unsafe { libc::geteuid() }
                && m.mode() & 0o7777 == 0o600
                && (m.dev(), m.ino()) == (device, inode),
            "container control identity changed"
        );
        Ok(())
    };
    check()?;
    let mut stream = UnixStream::connect(path)?;
    authorize_host_peer_sync(&stream)?;
    check()?;
    stream.set_read_timeout(Some(Duration::from_secs(315)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let request = Request {
        version: 1,
        request_id: uuid::Uuid::new_v4().to_string(),
        target: target.clone(),
        command,
    };
    write_host_frame_sync(&mut stream, &request)?;
    let response: Response = read_host_frame_sync(&mut stream)
        .context("container control reply uncertain; not retried")?;
    ensure!(
        response.version == 1
            && response.request_id == request.request_id
            && response.target == request.target,
        "container control response mismatch"
    );
    response.result.map_err(anyhow::Error::msg)
}

// Called only by the container owner after its VMM child has been reaped.
pub(super) fn retire(target: &Target) -> Result<()> {
    use std::os::unix::fs::FileTypeExt;
    let path = target.socket()?;
    match fs::symlink_metadata(&path) {
        Ok(m) => {
            ensure!(
                m.file_type().is_socket()
                    && m.uid() == unsafe { libc::geteuid() }
                    && m.mode() & 0o7777 == 0o600,
                "refusing replaced container endpoint"
            );
            fs::remove_file(path)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

pub(super) fn validate_config(config: &super::Create) -> Result<()> {
    ensure!(
        !config.managed_memory_compression || config.managed_memory,
        "compressed managed RAM requires managed memory"
    );
    if config.managed_memory {
        ensure!(
            cfg!(all(target_os = "linux", target_arch = "x86_64")),
            "managed container RAM requires Linux x86-64"
        );
        ensure!(
            config.memory_pool.is_none() && config.shared_kernel_base.is_none(),
            "managed whole-VM offload conflicts with RAM pooling/shared kernel RAM"
        );
    }
    Ok(())
}

struct Control {
    target: Target,
    backing: Arc<Backing>,
    vm: Arc<dyn VmControl>,
    status: Status,
    failed: bool,
}
impl Control {
    fn apply(&mut self, request: &Request) -> Result<Status> {
        ensure!(
            request.version == 1
                && uuid::Uuid::parse_str(&request.request_id).is_ok()
                && request.target == self.target,
            "invalid or stale container control request"
        );
        ensure!(!self.failed, "native control uncertain; teardown required");
        let result = match &request.command {
            HostVmCommand::Status => {
                let paused = self.vm.is_paused().map_err(anyhow::Error::msg)?;
                ensure!(
                    paused == (self.status.state == VmState::Offloaded),
                    "native state changed outside managed owner"
                );
                return Ok(self.status.clone());
            }
            HostVmCommand::Offload { file: None } => {
                ensure!(
                    self.status.state == VmState::Running,
                    "offload requires Running"
                );
                self.vm
                    .offload_ram()
                    .map_err(anyhow::Error::msg)
                    .and_then(|report| {
                        // The worker owns this transition through native reclaim AND
                        // compressed publication. No Resume can race the commit.
                        self.backing.finish_offload()?;
                        Ok(Status {
                            state: VmState::Offloaded,
                            memory: Some(VmMemory {
                                backing_file: self.backing.path(),
                                backed_bytes: report.backed_bytes,
                                resident_before_bytes: report.resident_before_bytes,
                                resident_after_bytes: report.resident_after_bytes,
                            }),
                        })
                    })
            }
            HostVmCommand::Resume => {
                ensure!(
                    self.status.state == VmState::Offloaded,
                    "resume requires Offloaded"
                );
                self.vm
                    .resume()
                    .map_err(anyhow::Error::msg)
                    .map(|()| Status {
                        state: VmState::Running,
                        memory: None,
                    })
            }
            _ => anyhow::bail!("unsupported managed container command"),
        };
        match result {
            Ok(status) => {
                self.status = status.clone();
                Ok(status)
            }
            Err(error) => {
                self.failed = true;
                anyhow::bail!("native control uncertain; teardown required: {error}")
            }
        }
    }
}

pub(super) struct Endpoint {
    listener: Option<UnixListener>,
    path: PathBuf,
    identity: (u64, u64),
    target: Target,
    backing: Arc<Backing>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Endpoint {
    pub(super) fn bind(target: Target, backing: Arc<Backing>) -> Result<Self> {
        let path = target.socket()?;
        let listener = UnixListener::bind(&path)?; // never remove a preexisting endpoint
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let m = fs::symlink_metadata(&path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener: Some(listener),
            path,
            identity: (m.dev(), m.ino()),
            target,
            backing,
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
        })
    }
    pub(super) fn start(&mut self, vm: Arc<dyn VmControl>) -> Result<()> {
        let listener = self
            .listener
            .take()
            .context("container control already started")?;
        let stop = self.stop.clone();
        let mut control = Control {
            target: self.target.clone(),
            backing: self.backing.clone(),
            vm,
            status: Status {
                state: VmState::Running,
                memory: None,
            },
            failed: false,
        };
        self.worker = Some(
            std::thread::Builder::new()
                .name("docker-memory".into())
                .spawn(move || {
                    // One worker serializes transitions; disconnect never cancels native I/O.
                    while !stop.load(Ordering::Acquire) {
                        match listener.accept() {
                            Ok((mut stream, _)) => {
                                let _ = (|| -> Result<()> {
                                    stream.set_nonblocking(false)?;
                                    authorize_host_peer_sync(&stream)?;
                                    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                                    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                                    let request: Request = read_host_frame_sync(&mut stream)?;
                                    let result = control
                                        .apply(&request)
                                        .map_err(|e| format!("{e:#}").chars().take(512).collect());
                                    write_host_frame_sync(
                                        &mut stream,
                                        &Response {
                                            version: 1,
                                            request_id: request.request_id,
                                            target: control.target.clone(),
                                            result,
                                        },
                                    )?;
                                    Ok(())
                                })();
                            }
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(10))
                            }
                            Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
                            Err(_) => break,
                        }
                    }
                })?,
        );
        Ok(())
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Keep the backing/mount alive until in-flight control has drained.
        // A native I/O stall is not treated as completed teardown.
        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                tracing::error!("managed memory worker panicked during shutdown");
            }
        }
        if fs::symlink_metadata(&self.path).is_ok_and(|m| (m.dev(), m.ino()) == self.identity) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pvisor_vm::api::RamReclaim;
    use std::sync::atomic::AtomicUsize;
    #[derive(Default)]
    struct Mock {
        paused: AtomicBool,
        calls: AtomicUsize,
        fail: AtomicBool,
    }
    impl VmControl for Mock {
        fn is_paused(&self) -> Result<bool, String> {
            Ok(self.paused.load(Ordering::SeqCst))
        }
        fn pause(&self) -> Result<(), String> {
            panic!("offload must own its native pause")
        }
        fn resume(&self) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.paused.store(false, Ordering::SeqCst);
            Ok(())
        }
        fn offload_ram(&self) -> Result<RamReclaim, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.paused.store(true, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err("writeback failed".into());
            }
            Ok(RamReclaim {
                backed_bytes: 4096,
                resident_before_bytes: Some(4096),
                resident_after_bytes: Some(0),
            })
        }
    }
    fn raw_backing() -> Arc<Backing> {
        let directory = tempfile::tempdir().unwrap();
        Arc::new(Backing::create(directory.path(), false).unwrap())
    }
    fn target() -> Target {
        Target {
            container_id: "a".repeat(64),
            generation: uuid::Uuid::new_v4().simple().to_string(),
        }
    }
    fn control() -> (Control, Arc<Mock>) {
        let vm = Arc::new(Mock::default());
        (
            Control {
                target: target(),
                backing: raw_backing(),
                vm: vm.clone(),
                status: Status {
                    state: VmState::Running,
                    memory: None,
                },
                failed: false,
            },
            vm,
        )
    }
    fn request(control: &Control, command: HostVmCommand) -> Request {
        Request {
            version: 1,
            request_id: uuid::Uuid::new_v4().to_string(),
            target: control.target.clone(),
            command,
        }
    }
    #[test]
    fn compressed_commit_precedes_ack_and_preserves_changed_data() {
        let (mut c, vm) = control();
        let (backing, store) = backing::staged();
        c.backing = backing;
        let lock = store.lock().unwrap();
        let (send, recv) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let r = request(&c, HostVmCommand::Offload { file: None });
            let result = c.apply(&r);
            send.send((c, result)).unwrap();
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while vm.calls.load(Ordering::SeqCst) == 0 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        // Native reclaim finished, but publication is deliberately blocked.
        assert!(recv.recv_timeout(Duration::from_millis(50)).is_err());
        drop(lock);
        let (mut c, result) = recv.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.join().unwrap();
        assert_eq!(result.unwrap().state, VmState::Offloaded);
        let first = store.lock().unwrap().head_id().unwrap();
        let r = request(&c, HostVmCommand::Resume);
        c.apply(&r).unwrap();
        store
            .lock()
            .unwrap()
            .write_at(0, b"second generation")
            .unwrap();
        let r = request(&c, HostVmCommand::Offload { file: None });
        c.apply(&r).unwrap();
        let store = store.lock().unwrap();
        assert_ne!(first, store.head_id().unwrap());
        let mut data = [0; 17];
        store.read_at(0, &mut data).unwrap();
        assert_eq!(&data, b"second generation");
    }

    #[test]
    fn compressed_publication_failure_never_acknowledges_or_resumes() {
        let (mut c, vm) = control();
        let (backing, store) = backing::staged();
        c.backing = backing;
        let _ = std::panic::catch_unwind(|| {
            let _guard = store.lock().unwrap();
            panic!("injected store failure");
        });
        let r = request(&c, HostVmCommand::Offload { file: None });
        assert!(c.apply(&r).is_err());
        assert_eq!(c.status.state, VmState::Running); // no success publication
        for command in [
            HostVmCommand::Status,
            HostVmCommand::Resume,
            HostVmCommand::Offload { file: None },
        ] {
            let r = request(&c, command);
            assert!(c.apply(&r).is_err());
        }
        assert_eq!(vm.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn shutdown_waits_for_in_flight_publication_before_releasing_backing() {
        let (backing, store) = backing::staged();
        let weak = Arc::downgrade(&backing);
        let target = target();
        let path = target.socket().unwrap();
        let mut endpoint = Endpoint::bind(target.clone(), backing).unwrap();
        let vm = Arc::new(Mock::default());
        endpoint.start(vm.clone()).unwrap();
        let (dev, ino) = endpoint.identity;
        let lock = store.lock().unwrap();
        let request = std::thread::spawn(move || {
            exchange(
                &path,
                dev,
                ino,
                &target,
                HostVmCommand::Offload { file: None },
            )
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while vm.calls.load(Ordering::SeqCst) == 0 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        let (send, recv) = std::sync::mpsc::channel();
        let shutdown = std::thread::spawn(move || {
            drop(endpoint);
            send.send(()).unwrap();
        });
        assert!(recv.recv_timeout(Duration::from_millis(50)).is_err());
        assert!(weak.upgrade().is_some());
        drop(lock);
        assert_eq!(request.join().unwrap().unwrap().state, VmState::Offloaded);
        recv.recv_timeout(Duration::from_secs(5)).unwrap();
        shutdown.join().unwrap();
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn stale_generation_and_unsupported_path_never_dispatch() {
        let (mut c, vm) = control();
        let mut r = request(&c, HostVmCommand::Offload { file: None });
        r.target.generation = target().generation;
        assert!(c.apply(&r).is_err());
        let r = request(
            &c,
            HostVmCommand::Offload {
                file: Some("/tmp/other".into()),
            },
        );
        assert!(c.apply(&r).is_err());
        assert_eq!(vm.calls.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn native_acknowledgement_gates_offload_and_resume() {
        let (mut c, vm) = control();
        let r = request(&c, HostVmCommand::Offload { file: None });
        let result = c.apply(&r).unwrap();
        assert_eq!(result.state, VmState::Offloaded);
        assert_eq!(result.memory.unwrap().backed_bytes, 4096);
        assert!(c.apply(&r).is_err()); // no repeated writeback after duplicate delivery
        let r = request(&c, HostVmCommand::Resume);
        assert_eq!(c.apply(&r).unwrap().state, VmState::Running);
        assert!(c.apply(&r).is_err());
        assert_eq!(vm.calls.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn failed_native_transition_fences_later_controls() {
        let (mut c, vm) = control();
        vm.fail.store(true, Ordering::SeqCst);
        let r = request(&c, HostVmCommand::Offload { file: None });
        assert!(c.apply(&r).is_err());
        let r = request(&c, HostVmCommand::Resume);
        assert!(c.apply(&r).is_err());
        let r = request(&c, HostVmCommand::Status);
        assert!(c.apply(&r).is_err());
        assert_eq!(vm.calls.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn private_endpoint_roundtrip_preserves_generation_and_rejects_inode_change() {
        let target = target();
        let mut ep = Endpoint::bind(target.clone(), raw_backing()).unwrap();
        let vm = Arc::new(Mock::default());
        ep.start(vm).unwrap();
        let (dev, ino) = ep.identity;
        assert_eq!(
            exchange(&ep.path, dev, ino, &target, HostVmCommand::Status)
                .unwrap()
                .state,
            VmState::Running
        );
        assert_eq!(
            exchange(
                &ep.path,
                dev,
                ino,
                &target,
                HostVmCommand::Offload { file: None }
            )
            .unwrap()
            .state,
            VmState::Offloaded
        );
        assert_eq!(
            exchange(&ep.path, dev, ino, &target, HostVmCommand::Resume)
                .unwrap()
                .state,
            VmState::Running
        );
        assert!(exchange(&ep.path, dev, ino + 1, &target, HostVmCommand::Status).is_err());
        let path = ep.path.clone();
        drop(ep);
        assert!(!path.exists());
    }
    #[test]
    fn retirement_rejects_replaced_endpoint_and_only_removes_its_generation() {
        let t = target();
        let path = t.socket().unwrap();
        fs::write(&path, b"not a socket").unwrap();
        assert!(retire(&t).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"not a socket");
        fs::remove_file(&path).unwrap();
        let ep = Endpoint::bind(t.clone(), raw_backing()).unwrap();
        retire(&t).unwrap();
        assert!(!path.exists());
        drop(ep);
        retire(&t).unwrap();
    }

    #[test]
    fn identities_cannot_escape_private_root() {
        let mut t = target();
        t.generation = "../escape".into();
        assert!(t.socket().is_err());
        t = target();
        t.container_id = "a".repeat(63);
        assert!(t.validate().is_err());
    }
}
