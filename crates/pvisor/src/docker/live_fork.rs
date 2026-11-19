//! Same-host fork of a quiescent OCI container, using a sealed shared RAM file.
use super::*;
use crate::environment_snapshot::{TreeInventory, copy_live_tree};
use crate::image::cache::RetainedImage;
use pvisor_vm::api::{
    MachineRestore, MachineSnapshot, RamRestoreExtent, SnapshotCapture, SnapshotControl,
    SnapshotState, VmmHandle,
};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixListener;
use std::sync::Arc;

const RAM_FD: &str = "PVISOR_INTERNAL_COW_RAM_FD";
const SEALS: i32 = libc::F_SEAL_WRITE | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;

pub(super) fn validate(config: &Create) -> anyhow::Result<()> {
    ensure!(
        config.branchable
            && !config.network
            && config.ports.is_empty()
            && config.mounts.is_empty()
            && config.peers.is_none()
            && !config.interactive
            && config.tty.is_none()
            && config.healthcheck.is_none()
            && !config.auto_remove
            && config.restart == RestartPolicy::No
            && config.memory_pool.is_none()
            && config.shared_kernel_base.is_none(),
        "live fork requires an isolated detached VM without mounts, networking, restart policy, health checks or memory pool"
    );
    Ok(())
}

pub(super) fn memory_file() -> io::Result<File> {
    let fd = unsafe {
        libc::memfd_create(
            c"pvisor-live-fork".as_ptr(),
            libc::MFD_ALLOW_SEALING | libc::MFD_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(super) fn pass_ram(command: &mut Command, ram: Option<&File>) {
    command.env_remove(RAM_FD);
    if let Some(ram) = ram {
        let fd = ram.as_raw_fd();
        command.env(RAM_FD, fd.to_string());
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

pub(super) fn inherited_ram(directory: &Path) -> anyhow::Result<Option<File>> {
    if !directory.join("fork.json").try_exists()? {
        return Ok(None);
    }
    let fd = std::env::var(RAM_FD)
        .context("live fork RAM handle missing")?
        .parse::<i32>()?;
    ensure!(fd > 2, "invalid inherited RAM descriptor");
    let seals = unsafe { libc::fcntl(fd, libc::F_GET_SEALS) };
    ensure!(
        seals >= 0 && seals & SEALS == SEALS,
        "live fork RAM must be sealed"
    );
    let file = File::open(format!("/proc/self/fd/{fd}"))?;
    ensure!(
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
        "could not isolate RAM descriptor"
    );
    Ok(Some(file))
}

#[derive(Serialize, Deserialize)]
pub(super) struct ForkState {
    machine: MachineSnapshot,
    frozen_root: PathBuf,
    extents: Vec<RamRestoreExtent>,
    image: RetainedImage,
    ram_identity: (u64, u64),
    source_control: PathBuf,
    upper: TreeInventory,
    work: TreeInventory,
}

pub(super) fn load(directory: &Path) -> anyhow::Result<Option<ForkState>> {
    match File::open(directory.join("fork.json")) {
        Ok(file) => Ok(Some(serde_json::from_reader(io::BufReader::new(file))?)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

impl ForkState {
    pub(super) fn open_image(&self) -> anyhow::Result<ImageRoot> {
        let owner = self.image.open()?;
        Ok(ImageRoot {
            root: owner.root().to_owned(),
            _owner: Some(owner),
        })
    }
    pub(super) fn restore(mut self, dir: &Path, control: &Path) -> anyhow::Result<MachineRestore> {
        let ram = inherited_ram(dir)?.context("missing fork RAM")?;
        let stat = ram.metadata()?;
        ensure!(
            self.ram_identity == (stat.dev(), stat.ino()),
            "foreign RAM handle"
        );
        rebind(&mut self.machine, &self.frozen_root, dir, &self.image)?;
        self.machine
            .rebind_vsock_listener(control::CONTROL_PORT, &self.source_control, control)?;
        Ok(MachineRestore {
            state: self.machine,
            ram_file: Arc::new(ram),
            ram_extents: self.extents,
        })
    }
}

pub(super) struct RunningRecord(std::thread::JoinHandle<anyhow::Result<tempfile::NamedTempFile>>);

impl RunningRecord {
    pub(super) fn prepare(store: &Store, instance: &Instance) -> anyhow::Result<Self> {
        let dir = store.directory(&instance.id);
        let mut copy = instance.clone();
        copy.state = "running".into();
        let session = store.session_record(&copy)?;
        let mut temp = tempfile::NamedTempFile::new_in(dir)?;
        let worker = std::thread::Builder::new()
            .name("fork-running-record".into())
            .spawn(move || {
                temp.write_all(&session.record_bytes()?)?;
                temp.as_file().sync_all()?;
                Ok(temp)
            })?;
        Ok(Self(worker))
    }
    pub(super) fn publish(self, store: &Store, dir: &Path) -> anyhow::Result<()> {
        let temp = self
            .0
            .join()
            .map_err(|_| anyhow::anyhow!("Session record preparation panicked"))??;
        let guard = store.lock()?;
        temp.persist(dir.join(crate::runtime::session_execution::STATE))?;
        drop(guard);
        File::open(dir)?.sync_all()?;
        Ok(())
    }
}

pub(super) struct Source {
    listener: UnixListener,
    store_root: PathBuf,
    dir: PathBuf,
    control: PathBuf,
    retained: RetainedImage,
    image: Option<ImageRoot>,
}
impl Source {
    pub(super) fn new<B>(
        store: &Store,
        instance: &Instance<B>,
        image: &mut ImageRoot,
    ) -> anyhow::Result<Self> {
        let control = socket(instance)?;
        let listener = UnixListener::bind(control.with_extension("fork"))?;
        let retained = image
            ._owner
            .as_mut()
            .context("missing OCI image owner")?
            .retain_for_branch()?;
        let dir = store.directory(&instance.id);
        register_image(&dir, &retained)?;
        Ok(Self {
            listener,
            store_root: store.root.clone(),
            dir,
            control,
            retained,
            image: None,
        })
    }
    pub(super) fn keep_image(&mut self, image: ImageRoot) {
        self.image = Some(image);
    }
    pub(super) fn serve(self, handle: VmmHandle) -> io::Result<()> {
        std::thread::Builder::new()
            .name("oci-live-fork".into())
            .spawn(move || {
                let _backend = self.image;
                for stream in self.listener.incoming() {
                    let Ok(mut stream) = stream else {
                        break;
                    };
                    if crate::host_transport::authorize_host_peer_sync(&stream).is_err() {
                        continue;
                    }
                    let outcome = (|| -> anyhow::Result<File> {
                        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                        let mut id = [0; 32];
                        stream.read_exact(&mut id)?;
                        ensure!(id.iter().all(u8::is_ascii_hexdigit), "invalid fork request");
                        let stage = self
                            .store_root
                            .join(format!("fork-{}", std::str::from_utf8(&id)?));
                        ensure!(
                            stage.canonicalize()? == stage && stage.is_dir(),
                            "invalid staging directory"
                        );
                        crate::util::startup_mark("oci-fork.source.capture.begin");
                        let ram = memory_file()?;
                        let (machine, extents, upper, work) = handle
                            .with_snapshot_quiesced(Duration::from_secs(30), |frozen| {
                                let op = (|| -> anyhow::Result<_> {
                                    let (mut state, extents) = frozen
                                        .capture_branch_state(&ram)
                                        .map_err(anyhow::Error::msg)?;
                                    crate::util::startup_mark("oci-fork.source.ram.captured");
                                    let upper = copy_live_tree(
                                        &self.dir.join("upper"),
                                        &stage.join("upper"),
                                    )?;
                                    let work = copy_live_tree(
                                        &self.dir.join("work"),
                                        &stage.join("work"),
                                    )?;
                                    rebind(&mut state, &self.dir, &stage, &self.retained)?;
                                    Ok((state, extents.unwrap_or_default(), upper, work))
                                })();
                                op.map_err(|e| format!("{e:#}"))
                            })
                            .map_err(anyhow::Error::msg)?;
                        crate::util::startup_mark("oci-fork.source.thawed");
                        ensure!(
                            unsafe { libc::fcntl(ram.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } == 0,
                            "seal captured RAM: {}",
                            io::Error::last_os_error()
                        );
                        let m = ram.metadata()?;
                        let saved = ForkState {
                            machine,
                            frozen_root: stage.clone(),
                            extents,
                            image: self.retained.clone(),
                            ram_identity: (m.dev(), m.ino()),
                            source_control: self.control.clone(),
                            upper,
                            work,
                        };
                        let mut writer =
                            io::BufWriter::new(File::create_new(stage.join("fork.json"))?);
                        serde_json::to_writer(&mut writer, &saved)?;
                        writer.flush()?;
                        Ok(ram)
                    })();
                    match outcome {
                        Ok(file) => {
                            if stream.write_all(&[1]).is_ok() {
                                let _ = crate::ram_backing::ipc::send_mapping_fd(
                                    &stream,
                                    file.as_raw_fd(),
                                );
                            }
                        }
                        Err(e) => {
                            let _ = stream.write_all(&[0]);
                            let _ = write!(stream, "{e:#}");
                        }
                    }
                }
            })?;
        Ok(())
    }
}

fn rebind(
    state: &mut MachineSnapshot,
    old: &Path,
    new: &Path,
    image: &RetainedImage,
) -> anyhow::Result<()> {
    let copies = ["upper", "work"].map(|part| (old.join(part), new.join(part)));
    ensure!(
        state.rebind_filesystem_stage("/dev/root", &copies, &[image.root().to_owned()])? == 1,
        "expected one OCI root"
    );
    Ok(())
}

fn register_image(dir: &Path, image: &RetainedImage) -> anyhow::Result<()> {
    let lease = dir.join("fork-images");
    private_directory(&lease)?;
    let file = File::create_new(lease.join(uuid()))?;
    serde_json::to_writer(file, image)?;
    Ok(())
}

// A retained lazy projection can be opened by multiple VMM processes. Session
// removal holds the catalog lock and releases the projection after the last
// member has released its on-disk reference.
pub(super) fn unreferenced_images(store: &Store, removed: &Path) -> anyhow::Result<Vec<PathBuf>> {
    fn read_refs(dir: &Path) -> anyhow::Result<Vec<RetainedImage>> {
        let refs = dir.join("fork-images");
        if !refs.try_exists()? {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        for entry in fs::read_dir(refs)? {
            result.push(serde_json::from_reader(File::open(entry?.path())?)?);
        }
        Ok(result)
    }
    let mut candidates = read_refs(removed)?;
    if candidates.is_empty() {
        return Ok(vec![]);
    }
    for dir in fs::read_dir(store.root.join("instances"))? {
        let dir = dir?.path();
        if dir == removed {
            continue;
        }
        for image in read_refs(&dir)? {
            candidates.retain(|c| c.root() != image.root());
        }
    }
    let mut owners = std::collections::BTreeSet::new();
    for candidate in candidates {
        candidate.verify()?;
        let owner = crate::image::cache::direct_image_owner(candidate.root())
            .context("invalid retained projection")?;
        let pool = owner.parent().context("missing projection pool")?;
        ensure!(
            pool == store.images.projections() || pool == store.root.join("images/projections"),
            "projection outside container image store"
        );
        owners.insert(owner.to_path_buf());
    }
    Ok(owners.into_iter().collect())
}

struct NewDirectory(Option<PathBuf>);
impl NewDirectory {
    fn create(path: PathBuf) -> io::Result<Self> {
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(Some(path)))
    }
    fn path(&self) -> &Path {
        self.0.as_deref().expect("owned directory")
    }
    fn keep(mut self) {
        self.0.take();
    }
}
impl Drop for NewDirectory {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_dir_all(path);
        }
    }
}

impl Store {
    /// Fork a quiescent branchable Linux OCI VM and wait for every child to answer Ping.
    /// Children retain private CPU/device state and disk writes, and map one sealed
    /// RAM object privately. This live generation cannot be restarted after it exits.
    pub fn fork_running(&self, selector: &str, names: &[String]) -> anyhow::Result<Vec<Instance>> {
        ensure!(
            !names.is_empty()
                && names.iter().all(|n| valid_name(n))
                && names
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == names.len(),
            "invalid or duplicate child names"
        );
        let source = self.resolve(selector)?;
        validate(&source.config)?;
        ensure!(
            source.state == "running" && self.active(&source)?,
            "source must be running"
        );
        let id = uuid();
        let stage = self.root.join(format!("fork-{id}"));
        let stage = NewDirectory::create(stage)?;
        ensure!(
            !self.directory(&source.id).join("fork.json").try_exists()?,
            "nested OCI fork is unsupported"
        );
        let mut stream = UnixStream::connect(socket(&source)?.with_extension("fork"))?;
        stream.set_read_timeout(Some(Duration::from_secs(120)))?;
        stream.write_all(id.as_bytes())?;
        let mut ok = [0];
        stream.read_exact(&mut ok)?;
        if ok != [1] {
            let mut error = String::new();
            stream.take(8192).read_to_string(&mut error)?;
            bail!("source capture failed: {error}");
        }
        crate::util::startup_mark("oci-fork.client.capture.received");
        let ram = File::from(crate::ram_backing::ipc::receive_fork_fd(&mut stream)?);
        let saved = load(stage.path())?.context("missing captured machine")?;
        let mut children = Vec::new();
        let mut staged = Vec::new();
        for name in names {
            let mut child = source.clone();
            child.id = format!("{}{}", uuid(), uuid());
            child.name = name.clone();
            child.config.name = Some(name.clone());
            child.generation = uuid();
            child.owner_epoch = child.generation.clone();
            child.state = "starting".into();
            child.exit_code = None;
            child.created_ms = crate::unix_now_ms();
            child.started_ms = crate::unix_now_ms();
            child.restart_count = 0;
            let dir = self.directory(&child.id);
            let guard = NewDirectory::create(dir.clone())?;
            for (part, inventory) in [("upper", &saved.upper), ("work", &saved.work)] {
                ensure!(
                    &copy_live_tree(&stage.path().join(part), &dir.join(part))? == inventory,
                    "fork filesystem changed"
                );
            }
            fs::hard_link(stage.path().join("fork.json"), dir.join("fork.json"))?;
            register_image(&dir, &saved.image)?;
            staged.push(guard);
            children.push(child);
        }
        crate::util::startup_mark("oci-fork.client.children.prepared");
        let leases = children
            .iter()
            .map(|child| acquire_lease(&self.directory(&child.id).join("owner.lock")))
            .collect::<anyhow::Result<Vec<_>>>()?;
        {
            let _guard = self.lock()?;
            let current = self.resolve(&source.id)?;
            ensure!(
                current.generation == source.generation && current.state == "running",
                "source generation changed"
            );
            let existing = self.all()?;
            ensure!(
                names
                    .iter()
                    .all(|name| !existing.iter().any(|i| &i.name == name)),
                "child name already in use"
            );
            std::thread::scope(|scope| {
                let writes: Vec<_> = children
                    .iter()
                    .map(|c| scope.spawn(|| self.save(c)))
                    .collect();
                for w in writes {
                    w.join()
                        .map_err(|_| anyhow::anyhow!("Session publication panicked"))??;
                }
                Ok::<_, anyhow::Error>(())
            })?;
            for guard in staged {
                guard.keep();
            }
        }
        crate::util::startup_mark("oci-fork.client.children.registered");
        let result = std::thread::scope(|s| {
            let workers: Vec<_> = children
                .iter()
                .zip(leases)
                .map(|(c, lease)| {
                    let ram = &ram;
                    s.spawn(move || {
                        if let Err(error) = self.spawn_owner(c, lease, Some(ram)) {
                            let _guard = self.lock()?;
                            finish_instance(self, &mut c.clone(), 125, Some(format!("{error:#}")))?;
                            return Err(error);
                        }
                        let child = self.wait_for_start(c.clone())?;
                        ensure!(
                            child.state == "running",
                            "fork child exited before readiness"
                        );
                        Ok(child)
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|w| {
                    w.join()
                        .map_err(|_| anyhow::anyhow!("child startup panicked"))?
                })
                .collect::<anyhow::Result<Vec<_>>>()
        });
        crate::util::startup_mark("oci-fork.client.children.started");
        if result.is_err() {
            for c in &children {
                let _ = self.stop(&c.id, Duration::from_secs(2), None);
                let _ = self.remove(&c.id, false);
            }
        }
        result
    }
}
use std::os::unix::fs::DirBuilderExt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_fork_mapping_is_read_only_and_independent_of_the_source_handle() {
        use std::os::unix::fs::FileExt;
        let file = memory_file().unwrap();
        file.write_all_at(b"fork", 0).unwrap();
        assert_eq!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, SEALS) },
            0
        );
        let (tx, mut rx) = UnixStream::pair().unwrap();
        crate::ram_backing::ipc::send_mapping_fd(&tx, file.as_raw_fd()).unwrap();
        drop(file);
        let received = File::from(crate::ram_backing::ipc::receive_fork_fd(&mut rx).unwrap());
        assert!(received.write_all_at(b"x", 0).is_err());
        let mut bytes = [0; 4];
        received.read_exact_at(&mut bytes, 0).unwrap();
        assert_eq!(&bytes, b"fork");
    }
}
