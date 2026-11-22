//! One single-use, unassigned VM per resource profile.
use super::*;
use crate::api::{Config, WarmPoolProfile};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::oneshot,
};

pub(super) const PLAN: &str = "warm.json";
pub(super) const DIAGNOSTICS: &str = "warm-supervisor.log";
const INTENT: &str = "warm-preparing.json";
const PARK_TIMEOUT: Duration = Duration::from_secs(15);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const RETRY: Duration = Duration::from_secs(1);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    binding: Tombstone,
    cpu_millis: u64,
    memory_bytes: u64,
    memory_pool: Option<PathBuf>,
}

impl Plan {
    fn resources(&self) -> RuntimeSpec {
        RuntimeSpec {
            id: self.binding.id.clone(),
            image: String::new(),
            entrypoint: Vec::new(),
            env: BTreeMap::new(),
            cpu_millis: self.cpu_millis,
            memory_bytes: self.memory_bytes,
            e2b: None,
        }
    }

    fn validate(&self, directory: &Path) -> Result<()> {
        validate_tombstone(&self.binding, directory)?;
        validate_resources(self.cpu_millis, self.memory_bytes)
    }
}

pub(crate) fn validate_profiles(profiles: &[WarmPoolProfile], config: &Config) -> Result<()> {
    ensure!(profiles.len() <= 8, "at most eight warm-pool profiles");
    let mut seen = std::collections::BTreeSet::new();
    let mut cpu = 0u64;
    let mut memory = 0u64;
    for &profile in profiles {
        validate_resources(profile.cpu_millis, profile.memory_bytes)?;
        ensure!(seen.insert(profile), "duplicate warm-pool profile");
        cpu = cpu
            .checked_add(profile.cpu_millis)
            .context("warm CPU overflow")?;
        memory = memory
            .checked_add(profile.memory_bytes)
            .context("warm memory overflow")?;
    }
    ensure!(
        profiles.len() <= config.max_sandboxes
            && cpu <= config.cpu_millis
            && memory <= config.memory_bytes,
        "warm-pool idle capacity exceeds the configured node limits"
    );
    Ok(())
}

struct Job {
    spec: RuntimeSpec,
    image: PreparedImage,
    reply: oneshot::Sender<Result<()>>,
}

struct Slot {
    id: String,
    assign: oneshot::Sender<Job>,
}

pub(crate) struct Checkout {
    pub(crate) id: String,
    profile: WarmPoolProfile,
    pool: Weak<Pool>,
    pending: Option<(Slot, PreparedImage)>,
}

impl Checkout {
    pub(crate) async fn launch(mut self, spec: RuntimeSpec) -> Result<()> {
        let (slot, image) = self.pending.take().expect("unused warm checkout");
        let (reply, result) = oneshot::channel();
        slot.assign
            .send(Job { spec, image, reply })
            .map_err(|_| CreateError::new("warm slot lost before assignment; reconcile", true))?;
        result
            .await
            .context("warm owner disappeared after admission")?
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        if let Some((slot, _)) = self.pending.take()
            && let Some(pool) = self.pool.upgrade()
            && !*pool.shutdown.borrow()
            && !slot.assign.is_closed()
        {
            pool.ready
                .lock()
                .unwrap()
                .entry(self.profile)
                .or_insert(slot);
        }
    }
}

pub(crate) struct Pool {
    runtime: Arc<NativeRuntime>,
    ready: StdMutex<BTreeMap<WarmPoolProfile, Slot>>,
    shutdown: tokio::sync::watch::Sender<bool>,
    workers: StdMutex<Vec<tokio::task::JoinHandle<Result<()>>>>,
}

impl Drop for Pool {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

impl Pool {
    pub(crate) fn start(
        runtime: Arc<NativeRuntime>,
        profiles: Vec<WarmPoolProfile>,
        daemon_lock: Arc<dyn Send + Sync>,
    ) -> Arc<Self> {
        let (shutdown, _) = tokio::sync::watch::channel(false);
        let pool = Arc::new(Self {
            runtime,
            ready: StdMutex::default(),
            shutdown,
            workers: StdMutex::default(),
        });
        for profile in profiles {
            let weak = Arc::downgrade(&pool);
            let runtime = pool.runtime.clone();
            let shutdown = pool.shutdown.subscribe();
            let daemon_lock = daemon_lock.clone();
            pool.workers.lock().unwrap().push(tokio::spawn(async move {
                let _daemon_lock = daemon_lock;
                maintain(weak, runtime, profile, shutdown).await
            }));
        }
        pool
    }

    pub(crate) async fn stop(&self) -> Result<()> {
        self.shutdown.send_replace(true);
        let workers = std::mem::take(&mut *self.workers.lock().unwrap());
        let mut result = Ok(());
        for worker in workers {
            match worker.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => result = Err(error),
                Err(error) => result = Err(error.into()),
            }
        }
        result
    }

    pub(crate) fn take(
        self: &Arc<Self>,
        profile: WarmPoolProfile,
        name: &str,
        e2b: &Option<E2bTemplate>,
    ) -> Option<Checkout> {
        if *self.shutdown.borrow() || !self.ready.lock().unwrap().contains_key(&profile) {
            return None;
        }
        pvisor::docker::Store::validate_image_key(name).ok()?;
        let image: PreparedImage =
            read_json(&self.runtime.config.images_dir.join(format!("{name}.json"))).ok()?;
        if !image.native_guest || image.library_dir.is_some() || &image.e2b != e2b {
            return None;
        }
        validate_image(&image, &self.runtime.config.state_dir).ok()?;
        let slot = self.ready.lock().unwrap().remove(&profile)?;
        if slot.assign.is_closed() {
            return None;
        }
        Some(Checkout {
            id: slot.id.clone(),
            profile,
            pool: Arc::downgrade(self),
            pending: Some((slot, image)),
        })
    }
}

async fn maintain(
    pool: Weak<Pool>,
    runtime: Arc<NativeRuntime>,
    profile: WarmPoolProfile,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        let result = run_slot(&pool, &runtime, profile, &mut shutdown).await;
        if let Err(error) = result {
            eprintln!("warm pool slot unavailable: {error:#}");
            // Do not create another slot when absence is uncertain.
            if error.is::<UnsafeToReplace>() {
                return Err(error);
            }
            tokio::select! {
                _ = sleep(RETRY) => (),
                _ = shutdown.changed() => return Ok(()),
            }
        }
    }
}

#[derive(Debug)]
struct UnsafeToReplace;
impl fmt::Display for UnsafeToReplace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("warm slot cleanup uncertain; restart to reconcile")
    }
}
impl std::error::Error for UnsafeToReplace {}

async fn run_slot(
    pool: &Weak<Pool>,
    runtime: &NativeRuntime,
    profile: WarmPoolProfile,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let id = format!("sb-{}", uuid::Uuid::new_v4());
    let generation = uuid::Uuid::new_v4().to_string();
    let directory = runtime.directory(&id)?;
    let mut plan = Plan {
        binding: Tombstone {
            version: 1,
            owner: runtime.config.owner.clone(),
            id: id.clone(),
            cgroup: runtime
                .config
                .cgroup_root
                .join(format!("pvisor-{id}-{generation}")),
            generation,
            cgroup_device: 0,
            cgroup_inode: 0,
            boot_id: current_boot_id()?,
        },
        cpu_millis: profile.cpu_millis,
        memory_bytes: profile.memory_bytes,
        memory_pool: runtime.memory_pool.clone(),
    };
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let mut claimed = false;
    let result = async {
        File::open(&runtime.config.state_dir)?.sync_all()?;
        publish_new(&directory.join(INTENT), &plan)?;
        fs::create_dir(&plan.binding.cgroup)?;
        install_limits(&plan.binding.cgroup, profile.cpu_millis, profile.memory_bytes)?;
        let stat = fs::symlink_metadata(&plan.binding.cgroup)?;
        plan.binding.cgroup_device = stat.dev();
        plan.binding.cgroup_inode = stat.ino();
        publish_new(&directory.join(PLAN), &plan)?;
        private_directory(&directory.join("run"))?;
        private_directory(&directory.join("run/tmp"))?;
        let listener = UnixListener::bind(directory.join(STARTUP_SOCKET))?;
        let mut child = runtime.spawn_supervisor(&directory, &open_group(&plan)?)?;
        let result = async {
            let mut parked = timeout(PARK_TIMEOUT, async {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (mut stream, _) = accepted?;
                        authorize_host_peer(&stream)?;
                        ensure!(stream.read_u8().await? == 1, "warm supervisor did not park");
                        Ok(stream)
                    }
                    status = child.wait() => bail!("warm supervisor exited before parking: {}", status?),
                    _ = shutdown.changed() => bail!("warm pool stopping"),
                }
            }).await.context("warm supervisor did not park before deadline")??;

            let (assign, job) = oneshot::channel();
            {
                let pool = pool.upgrade().context("pool stopped")?;
                pool.ready.lock().unwrap().insert(profile, Slot { id: id.clone(), assign });
            }
            eprintln!("warm pool parked {id} ({}m CPU, {} MiB)", profile.cpu_millis, profile.memory_bytes / MIB);
            let job = tokio::select! {
                biased;
                job = job => job.ok(),
                _ = shutdown.changed() => None,
                _ = sleep(IDLE_TIMEOUT) => None,
                _ = child.wait() => None,
            };
            if let Some(pool) = pool.upgrade() {
                pool.ready.lock().unwrap().remove(&profile);
            }
            if let Some(Job { spec, image, reply }) = job {
                claimed = true;
                let outcome = assign_slot(runtime, &plan, &directory, spec, image, &mut parked, &listener, &mut child).await;
                let cleanup = if let Err(error) = &outcome {
                    eprintln!("warm launch failed: {error:#}");
                    runtime.cleanup_binding(plan.binding.clone()).await
                } else {
                    Ok(())
                };
                let _ = reply.send(outcome.map_err(|_| CreateError::new(
                    if cleanup.is_ok() { "warm creation failed; VM confirmed absent" } else { "warm creation failed; cleanup uncertain" },
                    cleanup.is_err(),
                ).into()));
                cleanup.map_err(|_| UnsafeToReplace)?;
            }
            Ok(())
        }.await;
        tokio::spawn(async move { let _ = child.wait().await; });
        result
    }
    .await;
    if !claimed {
        if result.is_err()
            && !*shutdown.borrow()
            && let Ok(log) = trusted_file(&directory.join(DIAGNOSTICS), true)
        {
            let mut text = String::new();
            if log.take(4096).read_to_string(&mut text).is_ok() && !text.is_empty() {
                eprintln!("unassigned warm supervisor {id}: {}", text.trim_end());
            }
        }
        runtime
            .cleanup_binding(plan.binding)
            .await
            .map_err(|_| UnsafeToReplace)?;
        fs::remove_dir_all(&directory)?;
    } else {
        let _ = fs::remove_file(directory.join(STARTUP_SOCKET));
    }
    result
}

async fn assign_slot(
    runtime: &NativeRuntime,
    plan: &Plan,
    directory: &Path,
    spec: RuntimeSpec,
    image: PreparedImage,
    parked: &mut UnixStream,
    listener: &UnixListener,
    child: &mut tokio::process::Child,
) -> Result<()> {
    validate_spec(&spec)?;
    ensure!(
        spec.id == plan.binding.id
            && spec.cpu_millis == plan.cpu_millis
            && spec.memory_bytes == plan.memory_bytes
            && spec.e2b == image.e2b,
        "warm assignment mismatch"
    );
    let identity = Identity {
        version: 1,
        owner: plan.binding.owner.clone(),
        token: random_token(),
        generation: plan.binding.generation.clone(),
        spec,
        image,
        cgroup: plan.binding.cgroup.clone(),
        cgroup_device: plan.binding.cgroup_device,
        cgroup_inode: plan.binding.cgroup_inode,
        boot_id: plan.binding.boot_id.clone(),
        memory_pool: plan.memory_pool.clone(),
        deferred_root: true,
    };
    make_run_spec(&identity)?;
    publish_new(&directory.join("identity.json"), &identity)?;
    timeout(READY_TIMEOUT, async {
        parked.write_u8(1).await?;
        tokio::select! {
            result = confirm_startup(listener, runtime, &identity) => result,
            status = child.wait() => bail!("assigned warm supervisor exited: {}", status?),
        }
    })
    .await
    .context("assigned warm supervisor did not become ready")?
}

fn open_group(plan: &Plan) -> Result<Group> {
    Group::open_binding(
        &plan.binding.cgroup,
        plan.binding.cgroup_device,
        plan.binding.cgroup_inode,
        &plan.binding.boot_id,
    )?
    .context("warm cgroup disappeared")
}

pub(super) async fn supervise_slot(
    directory: &Path,
    clock: &std::time::Instant,
    startup: &mut BTreeMap<&str, u128>,
) -> Result<(
    Identity,
    (pvisor::RunHandle, pvisor::docker::ImageRoot),
    GuestReady,
)> {
    let plan: Plan = read_json(&directory.join(PLAN))?;
    plan.validate(directory)?;
    let group = open_group(&plan)?;
    ensure!(
        plan.binding.cgroup_inode != 0 && group.contains_current_process()?,
        "warm supervisor must start inside its bound cgroup"
    );
    let resources = plan.resources();
    group.verify_limits(&resources)?;
    startup.insert("validated_us", clock.elapsed().as_micros());
    mark(directory, "started")?;

    let ports = directory.join("ports");
    private_directory(&ports)?;
    let ready = GuestReady::bind(&ports)?;
    let storage = directory.join("run");
    let root = storage.join("image");
    private_directory(&root)?;
    let images = pvisor::docker::Store::open(root.clone())?;
    let settings = vm_settings(&storage, &resources, &plan.memory_pool, &root, None)?;
    let (host, runner) = std::os::unix::net::UnixStream::pair()?;
    host.set_nonblocking(true)?;
    let mut host = UnixStream::from_std(host)?;
    let executor = pvisor::VmExecutor::new(settings)?
        .with_vsock_ports(BTreeMap::from([(
            pvisor_guest::api::CONTROL_PORT,
            ports.join("guest.sock"),
        )]))?
        .with_control_ready(ready.path.clone())?
        .with_warm_root(runner)?;
    let mut preboot = pvisor_core::RunSpec::process(
        format!("run-{}", plan.binding.generation),
        "warm-pool",
        "/.unassigned",
    );
    let pvisor_core::RunInvocation::Process(process) = &mut preboot.invocation;
    process.inherit_env = false;
    process.cwd = Some("/".into());
    process.stdin = pvisor_core::StdioMode::Null;
    process.stdout = pvisor_core::StdioMode::Capture;
    process.stderr = pvisor_core::StdioMode::Capture;
    let handle = start_guest(storage, executor, preboot).await?;
    startup.insert("execution_started_us", clock.elapsed().as_micros());
    let assignment = async {
        ensure!(host.read_u8().await? == 1, "warm runner did not park");
        startup.insert("pid1_waiting_for_root_us", clock.elapsed().as_micros());
        verify_live_controls(handle.execution()).await?;
        startup.insert("controls_verified_us", clock.elapsed().as_micros());
        publish_run(directory, &plan.binding.generation, &handle)?;
        let mut claimant = UnixStream::connect(directory.join(STARTUP_SOCKET)).await?;
        authorize_host_peer(&claimant)?;
        claimant.write_u8(1).await?;
        ensure!(claimant.read_u8().await? == 1, "warm slot not claimed");
        let identity: Identity = read_json(&directory.join("identity.json"))?;
        validate_identity(&identity, directory)?;
        validate_launch_resources(&identity, directory)?;
        ensure_not_deleting(directory)?;
        ensure!(
            Tombstone::from_identity(&identity) == plan.binding
                && identity.spec.cpu_millis == plan.cpu_millis
                && identity.spec.memory_bytes == plan.memory_bytes
                && identity.deferred_root
                && identity.image.native_guest
                && identity.image.library_dir.is_none(),
            "warm assignment does not match the parked VM",
        );
        let image = images.open_image_for_warm_runner(&identity.image.pooled_image)?;
        startup.insert("image_opened_us", clock.elapsed().as_micros());
        let run = make_run_spec(&identity)?;
        let pvisor_core::RunInvocation::Process(process) = run.invocation;
        send_frame(
            &mut host,
            &pvisor::WarmRootAssignment {
                root: image.rootfs().to_path_buf(),
                argv: std::iter::once(process.program)
                    .chain(process.args)
                    .collect(),
                env: process.env,
            },
        )
        .await?;
        startup.insert("warm_root_assigned_us", clock.elapsed().as_micros());
        Ok((identity, image))
    };
    match timeout(READY_TIMEOUT, assignment).await {
        Ok(Ok((identity, image))) => Ok((identity, (handle, image), ready)),
        outcome => {
            handle.execution().cancel();
            if let Ok(Ok(report)) = timeout(DELETE_TIMEOUT, handle.wait()).await
                && !startup.contains_key("pid1_waiting_for_root_us")
            {
                eprintln!(
                    "idle runner failed before reaching PID 1: {:?} {:?}",
                    report.failure, report.output.stderr
                );
            }
            match outcome {
                Ok(Err(error)) => Err(error),
                Err(error) => Err(error.into()),
                _ => unreachable!(),
            }
        }
    }
}

pub(super) async fn reclaim_unclaimed(runtime: &NativeRuntime) -> Result<()> {
    for entry in fs::read_dir(&runtime.config.state_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir()
            || validate_id(&entry.file_name().to_string_lossy()).is_err()
        {
            continue;
        }
        let directory = entry.path();
        let record = if directory.join(PLAN).try_exists()? {
            PLAN
        } else {
            INTENT
        };
        if !directory.join(record).try_exists()? || directory.join("identity.json").try_exists()? {
            continue;
        }
        let plan: Plan = read_json(&directory.join(record))?;
        plan.validate(&directory)?;
        ensure!(
            plan.binding.owner == runtime.config.owner
                && plan.binding.cgroup.parent() == Some(&runtime.config.cgroup_root),
            "warm plan belongs to another runtime"
        );
        runtime.cleanup_binding(plan.binding).await?;
        fs::remove_dir_all(&directory)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_are_distinct_and_bound_idle_capacity() {
        let config = Config {
            state_dir: PathBuf::new(),
            api_key: String::new(),
            public_endpoint: String::new(),
            max_sandboxes: 2,
            cpu_millis: 4000,
            memory_bytes: 512 * MIB,
            max_timeout_seconds: 60,
        };
        let first = WarmPoolProfile {
            cpu_millis: 1000,
            memory_bytes: 128 * MIB,
        };
        let second = WarmPoolProfile {
            cpu_millis: 3000,
            memory_bytes: 384 * MIB,
        };
        assert!(validate_profiles(&[], &config).is_ok());
        assert!(validate_profiles(&[first, second], &config).is_ok());
        assert!(validate_profiles(&[first, first], &config).is_err());
        let extra_cpu = WarmPoolProfile {
            cpu_millis: 4000,
            ..second
        };
        let extra_memory = WarmPoolProfile {
            memory_bytes: 385 * MIB,
            ..second
        };
        assert!(validate_profiles(&[first, extra_cpu], &config).is_err());
        assert!(validate_profiles(&[first, extra_memory], &config).is_err());
    }
}
