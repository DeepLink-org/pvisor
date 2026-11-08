use super::{
    api::*,
    policy::{Action, Policy},
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use pvisor::host_transport::{authorize_host_peer, read_host_frame, write_host_frame};
use pvisor_core::host_protocol::{AGENTCTL_HOST_VERSION, AgentCtlHostRequest, HostVmCommand};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{Mutex, Semaphore},
};

#[derive(Clone, Serialize, Deserialize)]
struct Bound {
    binding: Binding,
    cgroup_dev: u64,
    cgroup_ino: u64,
    socket_dev: u64,
    socket_ino: u64,
}

struct Owner {
    policy: Policy,
    bindings: BTreeMap<String, Bound>,
    directory: PathBuf,
    failed: bool,
    root_identity: (u64, u64),
}

impl Owner {
    fn plan(&mut self) -> Result<Vec<Action>> {
        ensure!(!self.failed, "storage uncertain");
        let before = serde_json::to_vec(&self.policy.snapshot.entries)?;
        let (parent, charges) = self.measure()?;
        let actions = self.policy.plan(parent, &charges)?;
        if before != serde_json::to_vec(&self.policy.snapshot.entries)? {
            self.save()?;
        }
        Ok(actions)
    }
    fn save(&mut self) -> Result<()> {
        let result = (|| {
            let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
            serde_json::to_writer(
                &mut file,
                &serde_json::json!({
                    "config": self.policy.config, "snapshot": self.policy.snapshot,
                    "bindings": self.bindings,
                }),
            )?;
            file.flush()?;
            file.as_file().sync_all()?;
            file.persist(self.directory.join("pressure.json"))?;
            File::open(&self.directory)?.sync_all()?;
            Ok(())
        })();
        if let Err(error) = &result {
            self.failed = true;
            self.policy.snapshot.error = Some(
                format!("storage uncertain: {error:#}")
                    .chars()
                    .take(512)
                    .collect(),
            );
        }
        result
    }

    fn measure(&self) -> Result<(u64, BTreeMap<String, u64>)> {
        let m = fs::symlink_metadata(&self.policy.config.cgroup_root)?;
        ensure!(
            m.is_dir() && (m.dev(), m.ino()) == self.root_identity,
            "node cgroup identity changed"
        );
        check_budget(
            &self.policy.config.cgroup_root,
            self.policy.config.memory_bytes,
        )?;
        let parent = number(&self.policy.config.cgroup_root.join("memory.current"))?;
        let mut children = BTreeMap::new();
        for (id, bound) in &self.bindings {
            if self.policy.snapshot.entries[id].state == State::Released {
                continue;
            }
            bound.check_cgroup()?;
            ensure!(
                number(&bound.binding.cgroup.join("memory.max"))?
                    <= self.policy.snapshot.entries[id].memory_bytes,
                "VM memory limit changed"
            );
            ensure!(
                number(&bound.binding.cgroup.join("memory.swap.max"))? == 0,
                "VM swap limit changed"
            );
            children.insert(
                id.clone(),
                number(&bound.binding.cgroup.join("memory.current"))?,
            );
        }
        let parent = parent.max(number(
            &self.policy.config.cgroup_root.join("memory.current"),
        )?);
        let child_sum = children
            .values()
            .try_fold(0u64, |sum, &v| sum.checked_add(v))
            .context("charge sum overflow")?;
        ensure!(
            child_sum <= parent,
            "inconsistent parent/child sample; retry without granting"
        );
        Ok((parent, children))
    }

    async fn request(&mut self, envelope: Envelope) -> Result<Snapshot> {
        if matches!(envelope.request, Request::Status {}) {
            return Ok(self.policy.snapshot.clone());
        }
        ensure!(
            !self.failed,
            "storage uncertain; no new transitions are permitted"
        );
        ensure!(
            envelope.instance.as_deref() == Some(&self.policy.snapshot.instance),
            "daemon instance changed"
        );
        match envelope.request {
            Request::Reserve { id, memory_bytes } => self.policy.reserve(id, memory_bytes)?,
            Request::Bind { id, binding } => {
                ensure!(!self.bindings.contains_key(&id), "execution already bound");
                let entry = self
                    .policy
                    .snapshot
                    .entries
                    .get(&id)
                    .context("unknown VM")?;
                ensure!(entry.state == State::Admitted, "VM was not admitted");
                match &binding.target {
                    Target::Session(target) => {
                        target.validate()?;
                        ensure!(
                            target.execution_id.is_some(),
                            "explicit Execution is required"
                        );
                    }
                    Target::Container(target) => {
                        target.validate()?;
                        ensure!(
                            binding.socket == target.socket()?,
                            "unexpected container control path"
                        );
                    }
                }
                ensure!(binding.socket.is_absolute(), "socket must be absolute");
                let root = &self.policy.config.cgroup_root;
                let group = binding.cgroup.canonicalize()?;
                ensure!(
                    group == binding.cgroup && group != *root && group.starts_with(root),
                    "cgroup must be canonical and below node root"
                );
                for bound in self.bindings.values() {
                    ensure!(
                        !group.starts_with(&bound.binding.cgroup)
                            && !bound.binding.cgroup.starts_with(&group),
                        "overlapping VM cgroups"
                    );
                    ensure!(
                        binding.target != bound.binding.target,
                        "execution already owned"
                    );
                }
                ensure!(
                    number(&group.join("memory.max"))? <= entry.memory_bytes,
                    "VM hard limit exceeds reservation"
                );
                ensure!(
                    number(&group.join("memory.swap.max"))? == 0,
                    "managed VM requires swap0"
                );
                let cgroup = fs::symlink_metadata(&group)?;
                let socket = fs::symlink_metadata(&binding.socket)?;
                let bound = Bound {
                    binding,
                    cgroup_dev: cgroup.dev(),
                    cgroup_ino: cgroup.ino(),
                    socket_dev: socket.dev(),
                    socket_ino: socket.ino(),
                };
                // This read-only exchange validates UID and exact execution identity.
                // Retain the binding before a potentially failing read-only
                // probe: a started VM must never become an unbound releasable
                // reservation merely because its status response was lost.
                self.policy.bound(&id)?;
                self.policy.snapshot.entries.get_mut(&id).unwrap().state = State::Binding;
                self.bindings.insert(id, bound);
            }
            Request::Wait { id, token } => self.policy.wait(&id, token)?,
            Request::Acquire { id, token } => self.policy.acquire(&id, token)?,
            Request::Release { id } => {
                if let Some(bound) = self.bindings.get(&id) {
                    bound.empty()?;
                }
                // Unbound reservations can only be relinquished by their trusted
                // owner after cancelling startup. No timer releases them.
                self.policy.release(&id)?;
            }
            Request::Status {} => unreachable!(),
        }
        self.save()?;
        Ok(self.policy.snapshot.clone())
    }
}

async fn handle(owner: &Arc<Mutex<Owner>>, envelope: Envelope) -> Result<Snapshot> {
    let bind_id = match &envelope.request {
        Request::Bind { id, .. } => Some(id.clone()),
        _ => None,
    };
    let snapshot = owner.lock().await.request(envelope).await?;
    let Some(id) = bind_id else {
        return Ok(snapshot);
    };
    let bound = owner.lock().await.bindings[&id].clone();
    // Never retain the node mutex across an execution's IPC. A slow startup
    // identity probe must not prevent unrelated wake-ups or writeback commits.
    let result = bound.control(HostVmCommand::Status).await;
    let mut state = owner.lock().await;
    let entry = state.policy.snapshot.entries.get_mut(&id).unwrap();
    match &result {
        Ok(()) => entry.state = State::Active,
        Err(error) => {
            entry.state = State::Failed;
            entry.error = Some(format!("{error:#}").chars().take(512).collect());
        }
    }
    state.save()?;
    result?;
    Ok(state.policy.snapshot.clone())
}

fn number(path: &Path) -> Result<u64> {
    fs::read_to_string(path)?
        .trim()
        .parse()
        .with_context(|| format!("invalid counter {}", path.display()))
}

fn check_budget(root: &Path, budget: u64) -> Result<()> {
    for (depth, group) in root.ancestors().enumerate() {
        if depth > 0 && !group.join("cgroup.controllers").is_file() {
            break;
        }
        match fs::read_to_string(group.join("memory.max")) {
            Ok(value) if value.trim() == "max" => (),
            Ok(value) => ensure!(
                budget <= value.trim().parse::<u64>()?,
                "scheduler budget exceeds an ancestor hard limit"
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (), // root cgroup has no memory.max
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

impl Bound {
    fn check_cgroup(&self) -> Result<()> {
        let m = fs::symlink_metadata(&self.binding.cgroup)?;
        ensure!(
            m.is_dir() && m.dev() == self.cgroup_dev && m.ino() == self.cgroup_ino,
            "VM cgroup identity changed"
        );
        Ok(())
    }

    fn empty(&self) -> Result<()> {
        self.check_cgroup()?;
        let events = fs::read_to_string(self.binding.cgroup.join("cgroup.events"))?;
        ensure!(
            events.lines().any(|line| line == "populated 0"),
            "VM subtree still populated"
        );
        for file in ["cgroup.procs", "cgroup.threads"] {
            ensure!(
                fs::read_to_string(self.binding.cgroup.join(file))?
                    .trim()
                    .is_empty(),
                "VM tasks remain"
            );
        }
        Ok(())
    }

    async fn control(&self, command: HostVmCommand) -> Result<()> {
        self.check_cgroup()?;
        let m = fs::symlink_metadata(&self.binding.socket)?;
        ensure!(
            m.dev() == self.socket_dev && m.ino() == self.socket_ino,
            "VM endpoint replaced"
        );
        if let Target::Container(target) = &self.binding.target {
            let target = target.clone();
            let path = self.binding.socket.clone();
            let (device, inode) = (self.socket_dev, self.socket_ino);
            let expected = match &command {
                HostVmCommand::Status | HostVmCommand::Resume => {
                    pvisor_core::operation::VmState::Running
                }
                HostVmCommand::Offload { file: None } => pvisor_core::operation::VmState::Offloaded,
                _ => anyhow::bail!("unsupported scheduler command"),
            };
            let status = tokio::task::spawn_blocking(move || {
                pvisor::docker::managed::exchange(&path, device, inode, &target, command)
            })
            .await??;
            ensure!(
                status.state == expected,
                "container native state was not acknowledged"
            );
            if expected == pvisor_core::operation::VmState::Offloaded {
                let memory = status.memory.context("missing container RAM report")?;
                ensure!(
                    memory.backed_bytes > 0 && memory.backing_file.is_absolute(),
                    "invalid container RAM report"
                );
            } else {
                ensure!(status.memory.is_none(), "unexpected container RAM report");
            }
            return Ok(());
        }
        let Target::Session(target) = &self.binding.target else {
            unreachable!()
        };
        let status_only = matches!(command, HostVmCommand::Status);
        let expected = match command {
            HostVmCommand::Offload { .. } => Some(pvisor_core::operation::VmState::Offloaded),
            HostVmCommand::Resume => Some(pvisor_core::operation::VmState::Running),
            _ => None,
        };
        let response = pvisor::host_vm_exchange(
            &self.binding.socket,
            &AgentCtlHostRequest {
                version: AGENTCTL_HOST_VERSION,
                request_id: uuid::Uuid::new_v4().to_string(),
                target: Some(target.clone()),
                command,
            },
        )
        .await?
        .result?;
        ensure!(!response.status.state.is_terminal(), "execution has ended");
        if status_only {
            ensure!(
                response.status.state == pvisor_core::RunState::Running,
                "initial execution is not Running"
            );
        }
        if let Some(expected) = expected {
            match response.value {
                Some(pvisor_core::operation::Value::Vm { state, memory }) if state == expected => {
                    if expected == pvisor_core::operation::VmState::Offloaded {
                        let memory = memory.context("offload acknowledgement has no RAM report")?;
                        ensure!(
                            memory.backed_bytes > 0 && memory.backing_file.is_absolute(),
                            "invalid offload RAM report"
                        );
                    } else {
                        ensure!(memory.is_none(), "resume returned an offload RAM report");
                    }
                }
                _ => anyhow::bail!("native control did not acknowledge expected VM state"),
            }
        }
        Ok(())
    }
}

/// Serve one opt-in pressure scheduler in a fresh private directory on Linux.
/// Same-UID clients enroll ordinary OCI executions through existing HostVm IPC.
/// No image, backing, CPU quota, process or cgroup is created/modified here.
/// Refuses an undrained prior ledger after restart; accepted effects are never
/// retried automatically. Stop only after all records are Released. On crash,
/// preserve the ledger and externally audit/clean its resources before a new run.
pub async fn serve(directory: PathBuf, config: Config) -> Result<()> {
    ensure!(
        cfg!(target_os = "linux"),
        "pressure accounting requires Linux cgroup v2"
    );
    ensure!(directory.is_absolute(), "state directory must be absolute");
    let policy = Policy::new(config)?;
    let root = &policy.config.cgroup_root;
    ensure!(
        root.is_absolute() && root.canonicalize()? == *root,
        "canonical cgroup root required"
    );
    // cgroup.controllers plus filesystem magic prevent ordinary counter fixtures
    // from being used as production accounting authority.
    ensure!(
        root.join("cgroup.controllers").is_file(),
        "cgroup v2 required"
    );
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let path = CString::new(root.as_os_str().as_bytes())?;
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        ensure!(
            unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } == 0,
            "cannot inspect cgroup filesystem"
        );
        ensure!(
            unsafe { stat.assume_init() }.f_type == libc::CGROUP2_SUPER_MAGIC,
            "not a cgroup v2 filesystem"
        );
    }
    number(&root.join("memory.current"))?;
    check_budget(root, policy.config.memory_bytes)?;
    let root_metadata = fs::symlink_metadata(root)?;
    let root_identity = (root_metadata.dev(), root_metadata.ino());
    match fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.into()),
    }
    let m = fs::symlink_metadata(&directory)?;
    ensure!(
        m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o777 == 0o700,
        "state must be owned and private"
    );
    ensure!(
        directory.canonicalize()? == directory,
        "state path must be canonical"
    );
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("pressure.lock"))?;
    lock.try_lock_exclusive()
        .context("scheduler already running")?;
    let ledger = directory.join("pressure.json");
    if ledger.exists() {
        let previous: serde_json::Value = serde_json::from_reader(File::open(&ledger)?)?;
        let snapshot: Snapshot = serde_json::from_value(previous["snapshot"].clone())?;
        ensure!(
            snapshot
                .entries
                .values()
                .all(|e| e.state == State::Released),
            "undrained scheduler ledger; preserve and audit existing resources"
        );
    }
    let socket = directory.join("pressure.sock");
    if socket.exists() {
        fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let mut owner = Owner {
        policy,
        bindings: BTreeMap::new(),
        directory,
        failed: false,
        root_identity,
    };
    owner.save()?;
    println!(
        "{}",
        serde_json::json!({"socket": socket, "instance": owner.policy.snapshot.instance})
    );
    let owner = Arc::new(Mutex::new(owner));
    let slots = Arc::new(Semaphore::new(32));
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (mut stream, _) = accepted?;
                let Ok(slot) = slots.clone().try_acquire_owned() else { continue; };
                let owner = owner.clone();
                tokio::spawn(async move {
                    let _slot = slot;
                    let result: Result<Snapshot> = async {
                        authorize_host_peer(&stream)?;
                        let envelope = tokio::time::timeout(Duration::from_secs(5), read_host_frame(&mut stream)).await??;
                        handle(&owner, envelope).await
                    }.await;
                    let response: std::result::Result<Snapshot, String> = result.map_err(|e| format!("{e:#}"));
                    let _ = tokio::time::timeout(Duration::from_secs(5), write_host_frame(&mut stream, &response)).await;
                });
            }
            _ = ticker.tick() => {
                let mut state = owner.lock().await;
                if state.failed { continue; }
                let actions = match state.plan() {
                    Ok(actions) => actions,
                    Err(error) => { state.policy.snapshot.error = Some(format!("{error:#}").chars().take(512).collect()); continue; }
                };
                for action in actions {
                    let id = match &action { Action::Offload(id) | Action::Resume(id) => id };
                    let bound = state.bindings[id].clone();
                    let owner = owner.clone();
                    tokio::spawn(async move {
                        let command = match action { Action::Offload(_) => HostVmCommand::Offload { file: None }, Action::Resume(_) => HostVmCommand::Resume };
                        // No select/abort on caller disconnect: native writeback
                        // must finish or retain uncertainty before releasing slots.
                        let result = bound.control(command).await;
                        let mut state = owner.lock().await;
                        state.policy.finish(action, result);
                        let _ = state.save();
                    });
                }
            }
        }
    }
}

/// One same-user, bounded request to a private scheduler. A response timeout is
/// uncertainty, not cancellation; query Status using the pinned instance.
pub async fn exchange(socket: &Path, envelope: &Envelope) -> Result<Snapshot> {
    let mut stream = UnixStream::connect(socket).await?;
    authorize_host_peer(&stream)?;
    tokio::time::timeout(
        Duration::from_secs(5),
        write_host_frame(&mut stream, envelope),
    )
    .await??;
    let result: std::result::Result<Snapshot, String> =
        tokio::time::timeout(Duration::from_secs(40), read_host_frame(&mut stream)).await??;
    let snapshot = result.map_err(|e| anyhow::anyhow!(e))?;
    if let Some(expected) = &envelope.instance {
        ensure!(
            &snapshot.instance == expected,
            "scheduler response belongs to another instance"
        );
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Owner) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("group");
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        fs::write(root.join("memory.current"), "0\n").unwrap();
        let m = fs::metadata(&root).unwrap();
        let directory = temp.path().join("state");
        fs::create_dir(&directory).unwrap();
        let policy = Policy::new(Config {
            memory_bytes: 20,
            high_bytes: 18,
            low_bytes: 14,
            max_writebacks: 16,
            cgroup_root: root,
        })
        .unwrap();
        (
            temp,
            Owner {
                policy,
                bindings: BTreeMap::new(),
                directory,
                failed: false,
                root_identity: (m.dev(), m.ino()),
            },
        )
    }

    fn envelope(owner: &Owner, request: Request) -> Envelope {
        Envelope {
            instance: Some(owner.policy.snapshot.instance.clone()),
            request,
        }
    }

    #[tokio::test]
    async fn admitted_grant_is_durable_even_without_control_action() {
        let (_temp, mut owner) = fixture();
        let id = uuid::Uuid::new_v4().to_string();
        owner
            .request(envelope(
                &owner,
                Request::Reserve {
                    id: id.clone(),
                    memory_bytes: 8,
                },
            ))
            .await
            .unwrap();
        assert!(owner.plan().unwrap().is_empty());
        let value: serde_json::Value =
            serde_json::from_reader(File::open(owner.directory.join("pressure.json")).unwrap())
                .unwrap();
        assert_eq!(value["snapshot"]["entries"][&id]["state"], "admitted");
    }

    #[tokio::test]
    async fn instance_fences_mutations_and_failed_storage_fences_grants() {
        let (_temp, mut owner) = fixture();
        let id = uuid::Uuid::new_v4().to_string();
        let request = Request::Reserve {
            id: id.clone(),
            memory_bytes: 8,
        };
        assert!(
            owner
                .request(Envelope {
                    instance: None,
                    request: request.clone()
                })
                .await
                .is_err()
        );
        assert!(owner.policy.snapshot.entries.is_empty());
        owner.request(envelope(&owner, request)).await.unwrap();
        // Deterministic I/O failure, not a timing or permission assumption.
        owner.directory = owner.directory.join("missing");
        assert!(owner.plan().is_err());
        assert!(owner.failed);
        let status = owner
            .request(Envelope {
                instance: None,
                request: Request::Status {},
            })
            .await
            .unwrap();
        assert!(status.error.unwrap().contains("storage uncertain"));
        assert!(
            owner
                .request(envelope(&owner, Request::Release { id }))
                .await
                .is_err()
        );
    }

    #[test]
    fn replaced_accounting_root_does_not_become_empty_capacity() {
        let (_temp, owner) = fixture();
        let root = &owner.policy.config.cgroup_root;
        fs::rename(root, root.with_extension("old")).unwrap();
        fs::create_dir(root).unwrap();
        fs::write(root.join("memory.current"), "0").unwrap();
        assert!(
            owner
                .measure()
                .unwrap_err()
                .to_string()
                .contains("identity")
        );
    }

    #[test]
    fn inherited_hard_limit_cannot_be_hidden_by_unlimited_leaf() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("parent");
        let leaf = root.join("leaf");
        fs::create_dir_all(&leaf).unwrap();
        fs::write(root.join("cgroup.controllers"), "memory").unwrap();
        fs::write(root.join("memory.max"), "16").unwrap();
        fs::write(leaf.join("memory.max"), "max").unwrap();
        assert!(check_budget(&leaf, 20).is_err());
        assert!(check_budget(&leaf, 16).is_ok());
    }

    #[tokio::test]
    async fn bound_release_requires_real_empty_group_and_no_control() {
        let (_temp, mut owner) = fixture();
        let id = uuid::Uuid::new_v4().to_string();
        owner.policy.reserve(id.clone(), 8).unwrap();
        owner.plan().unwrap();
        owner.policy.bound(&id).unwrap();
        let group = owner.policy.config.cgroup_root.join("vm");
        fs::create_dir(&group).unwrap();
        let m = fs::metadata(&group).unwrap();
        owner.bindings.insert(
            id.clone(),
            Bound {
                cgroup_dev: m.dev(),
                cgroup_ino: m.ino(),
                socket_dev: 0,
                socket_ino: 0,
                binding: Binding {
                    cgroup: group.clone(),
                    socket: "/unused".into(),
                    target: Target::Session(pvisor_core::host_protocol::AgentCtlTarget {
                        session_id: "s".into(),
                        execution_id: Some("e".into()),
                        generation: None,
                    }),
                },
            },
        );
        fs::write(group.join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
        fs::write(group.join("cgroup.procs"), "").unwrap();
        fs::write(group.join("cgroup.threads"), "").unwrap();
        assert!(
            owner
                .request(envelope(&owner, Request::Release { id: id.clone() }))
                .await
                .is_err()
        );
        fs::write(group.join("cgroup.events"), "populated 0\nfrozen 0\n").unwrap();
        fs::write(group.join("cgroup.threads"), "42\n").unwrap();
        assert!(
            owner
                .request(envelope(&owner, Request::Release { id: id.clone() }))
                .await
                .is_err()
        );
        fs::write(group.join("cgroup.threads"), "").unwrap();
        owner
            .request(envelope(&owner, Request::Release { id: id.clone() }))
            .await
            .unwrap();
        assert_eq!(owner.policy.snapshot.entries[&id].state, State::Released);
    }

    #[tokio::test]
    async fn wire_client_rejects_a_reply_from_another_instance() {
        let (_temp, owner) = fixture();
        let socket = owner.directory.join("p.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let response = owner.policy.snapshot.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _: Envelope = read_host_frame(&mut stream).await.unwrap();
            write_host_frame(&mut stream, &Ok::<_, String>(response))
                .await
                .unwrap();
        });
        let result = exchange(
            &socket,
            &Envelope {
                instance: Some("wrong".into()),
                request: Request::Status {},
            },
        )
        .await;
        assert!(result.unwrap_err().to_string().contains("another instance"));
        server.await.unwrap();
    }

    fn host_socket() -> (tempfile::TempDir, UnixListener, PathBuf) {
        let root = Path::new("/tmp")
            .canonicalize()
            .unwrap()
            .join(format!("pvisor-host-{}", unsafe { libc::geteuid() }));
        match fs::DirBuilder::new().mode(0o700).create(&root) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => panic!("{e}"),
        }
        let temp = tempfile::Builder::new()
            .prefix("pressure-test-")
            .tempdir_in(root)
            .unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = temp.path().join("c.sock");
        let listener = UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        (temp, listener, path)
    }

    fn binding_fixture(owner: &Owner, socket: PathBuf) -> Binding {
        let group = owner.policy.config.cgroup_root.join("vm");
        fs::create_dir(&group).unwrap();
        for (name, value) in [
            ("memory.max", "8"),
            ("memory.swap.max", "0"),
            ("memory.current", "8"),
            ("cgroup.events", "populated 1\n"),
            ("cgroup.procs", "1\n"),
            ("cgroup.threads", "1\n"),
        ] {
            fs::write(group.join(name), value).unwrap();
        }
        fs::write(owner.policy.config.cgroup_root.join("memory.current"), "8").unwrap();
        Binding {
            socket,
            cgroup: group.canonicalize().unwrap(),
            target: Target::Session(pvisor_core::host_protocol::AgentCtlTarget {
                session_id: "pressure-session".into(),
                execution_id: Some("pressure-execution".into()),
                generation: None,
            }),
        }
    }

    #[tokio::test]
    async fn ordinary_container_binding_uses_container_identity_and_checks_ack() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, owner) = fixture();
        let target = pvisor::docker::managed::Target {
            container_id: "a".repeat(64),
            generation: uuid::Uuid::new_v4().simple().to_string(),
        };
        let path = target.socket().unwrap();
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let mut binding = binding_fixture(&owner, path.clone());
        binding.target = Target::Container(target.clone());
        let group = fs::metadata(&binding.cgroup).unwrap();
        let socket = fs::metadata(&path).unwrap();
        let bound = Bound {
            binding,
            cgroup_dev: group.dev(),
            cgroup_ino: group.ino(),
            socket_dev: socket.dev(),
            socket_ino: socket.ino(),
        };
        let server = tokio::spawn(async move {
            for (expected, state) in [
                (
                    HostVmCommand::Status,
                    pvisor_core::operation::VmState::Running,
                ),
                (
                    HostVmCommand::Offload { file: None },
                    pvisor_core::operation::VmState::Offloaded,
                ),
                (
                    HostVmCommand::Resume,
                    pvisor_core::operation::VmState::Running,
                ),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request: pvisor::docker::managed::Request =
                    read_host_frame(&mut stream).await.unwrap();
                assert_eq!(request.target, target);
                assert_eq!(request.command, expected);
                let memory = (state == pvisor_core::operation::VmState::Offloaded).then(|| {
                    pvisor_core::operation::VmMemory {
                        backing_file: "/private/ram".into(),
                        backed_bytes: 4096,
                        resident_before_bytes: Some(4096),
                        resident_after_bytes: Some(0),
                    }
                });
                write_host_frame(
                    &mut stream,
                    &pvisor::docker::managed::Response {
                        version: 1,
                        request_id: request.request_id,
                        target: target.clone(),
                        result: Ok(pvisor::docker::managed::Status { state, memory }),
                    },
                )
                .await
                .unwrap();
            }
        });
        bound.control(HostVmCommand::Status).await.unwrap();
        bound
            .control(HostVmCommand::Offload { file: None })
            .await
            .unwrap();
        bound.control(HostVmCommand::Resume).await.unwrap();
        server.await.unwrap();
        fs::remove_file(path).unwrap();
    }

    fn reply(request: &AgentCtlHostRequest<HostVmCommand>, state: &str) -> serde_json::Value {
        serde_json::json!({"version": AGENTCTL_HOST_VERSION, "request_id": request.request_id,
            "result": {"Ok": {"status": {"run_id": "pressure-session", "state": "running",
                "updated_at_unix_ms": 1, "execution": {"execution_id": "pressure-execution", "number": 1,
                    "executor": {"name": "vm", "kind": "virtual_machine", "isolation": "virtual_machine",
                        "supports_checkpoint": false, "supports_migration": false}}},
                "value": {"vm": {"state": state, "memory": null}}}}})
    }

    #[tokio::test]
    async fn slow_binding_does_not_hold_node_lock_and_failed_probe_retains_cleanup_binding() {
        let (_temp, mut owner) = fixture();
        let (_socket_temp, listener, path) = host_socket();
        let id = uuid::Uuid::new_v4().to_string();
        owner.policy.reserve(id.clone(), 8).unwrap();
        owner.plan().unwrap();
        let binding = binding_fixture(&owner, path);
        let request = envelope(
            &owner,
            Request::Bind {
                id: id.clone(),
                binding,
            },
        );
        let owner = Arc::new(Mutex::new(owner));
        let gate = Arc::new(tokio::sync::Notify::new());
        let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
        let server_gate = gate.clone();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: AgentCtlHostRequest<HostVmCommand> =
                read_host_frame(&mut stream).await.unwrap();
            assert_eq!(request.command, HostVmCommand::Status);
            observed_tx.send(()).unwrap();
            server_gate.notified().await;
            // Exact execution identity mismatch must not release the reservation.
            let mut response = reply(&request, "running");
            response["result"]["Ok"]["status"]["run_id"] = "wrong".into();
            write_host_frame(&mut stream, &response).await.unwrap();
        });
        let request_owner = owner.clone();
        let work = tokio::spawn(async move { handle(&request_owner, request).await });
        tokio::time::timeout(Duration::from_secs(2), observed_rx)
            .await
            .unwrap()
            .unwrap();
        let status = tokio::time::timeout(
            Duration::from_secs(2),
            handle(
                &owner,
                Envelope {
                    instance: None,
                    request: Request::Status {},
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(status.entries[&id].state, State::Binding);
        gate.notify_one();
        assert!(work.await.unwrap().is_err());
        server.await.unwrap();
        let mut owner = owner.lock().await;
        assert_eq!(owner.policy.snapshot.entries[&id].state, State::Failed);
        assert!(owner.bindings.contains_key(&id));
        let request = envelope(&owner, Request::Release { id });
        assert!(owner.request(request).await.is_err());
    }

    #[tokio::test]
    async fn native_adapter_uses_existing_offload_without_changing_backing_and_checks_reply() {
        let (_temp, owner) = fixture();
        let (_socket_temp, listener, path) = host_socket();
        let binding = binding_fixture(&owner, path);
        let group = fs::metadata(&binding.cgroup).unwrap();
        let socket = fs::metadata(&binding.socket).unwrap();
        let bound = Bound {
            binding,
            cgroup_dev: group.dev(),
            cgroup_ino: group.ino(),
            socket_dev: socket.dev(),
            socket_ino: socket.ino(),
        };
        let server = tokio::spawn(async move {
            for (index, state) in ["paused", "offloaded", "offloaded", "running"]
                .into_iter()
                .enumerate()
            {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request: AgentCtlHostRequest<HostVmCommand> =
                    read_host_frame(&mut stream).await.unwrap();
                assert_eq!(
                    request.command,
                    if index == 3 {
                        HostVmCommand::Resume
                    } else {
                        HostVmCommand::Offload { file: None }
                    }
                );
                let mut response = reply(&request, state);
                if index == 2 {
                    response["result"]["Ok"]["value"]["vm"]["memory"] = serde_json::json!({
                        "backing_file": "/original/ram", "backed_bytes": 8,
                        "resident_before_bytes": 8, "resident_after_bytes": 0 });
                }
                write_host_frame(&mut stream, &response).await.unwrap();
            }
        });
        let error = bound
            .control(HostVmCommand::Offload { file: None })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("expected VM state"), "{error:#}");
        assert!(
            bound
                .control(HostVmCommand::Offload { file: None })
                .await
                .unwrap_err()
                .to_string()
                .contains("no RAM report")
        );
        bound
            .control(HostVmCommand::Offload { file: None })
            .await
            .unwrap();
        bound.control(HostVmCommand::Resume).await.unwrap();
        server.await.unwrap();
    }
}
