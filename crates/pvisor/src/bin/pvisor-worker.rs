//! Per-host cluster executor. Task bodies never inherit controller credentials.
use anyhow::{Context, ensure};
use clap::{Parser, ValueEnum};
use fs2::FileExt;
use pvisor::{ContainerExecutor, PVisor, ProcessExecutor, RunExecutor, VmExecutor};
use pvisor_cluster::admission::{AdmissionPolicy, sample_linux};
use pvisor_cluster::{client::Client, *};
use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation, StdioMode};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::Instant,
};
#[path = "worker/artifacts.rs"]
mod artifacts;
#[path = "worker/checkpoints.rs"]
mod checkpoints;
#[path = "worker/cpu.rs"]
mod cpu;
#[path = "worker/environment.rs"]
mod environment;
#[path = "worker/gateway.rs"]
mod gateway;
#[path = "worker/memory.rs"]
mod memory;
#[path = "worker/outbox.rs"]
mod outbox;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Backend {
    Host,
    Rootless,
    Container,
    Vm,
}

/// Only fields actually connected to the cluster worker are accepted.
#[derive(Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct WorkerProfile {
    checkpoint_storage: Option<checkpoints::Profile>,
    #[serde(skip)]
    checkpoints: Option<Arc<checkpoints::Repository>>,
    gateway: gateway::Profile,
    vm: pvisor::VmSettings,
    container: pvisor::ContainerSettings,
    overlaynet: pvisor::OverlayNetSettings,
    /// Shared read-only inputs, highest priority first. Upper storage is private.
    lower_layers: Vec<PathBuf>,
    admission: AdmissionPolicy,
    environments: EnvironmentProfile,
    memory_sampling: memory::Profile,
    cpu_qos: CpuQosProfile,
    cpu_sampling: cpu::Profile,
    #[cfg(target_os = "linux")]
    #[serde(skip)]
    cpu_group: Option<Arc<pvisor::CpuQosGroup>>,
}
#[derive(Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CpuQosProfile {
    enabled: bool,
}

#[derive(Clone, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct EnvironmentProfile {
    enabled: bool,
    max_layers: usize,
}
impl Default for EnvironmentProfile {
    fn default() -> Self {
        Self {
            enabled: false,
            max_layers: 128,
        }
    }
}

#[derive(Clone, Parser)]
#[command(about = "Execute distributed tasks with the host-local pVisor kernel")]
struct Args {
    #[arg(
        long,
        default_value = "http://127.0.0.1:19800",
        env = "PVISOR_CLUSTER_URL"
    )]
    url: String,
    #[arg(long, env = "PVISOR_CLUSTER_WORKER_TOKEN", hide_env_values = true)]
    token: String,
    #[arg(long)]
    id: String,
    #[arg(long, default_value = ".pvisor/worker")]
    state: PathBuf,
    /// Host is explicitly a trusted process backend; it has no sandbox boundary.
    #[arg(long, value_enum, default_value = "rootless")]
    backend: Backend,
    /// Host-owned TOML worker profile supplies rootfs, layers, network and cache settings.
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, default_value_t = 16)]
    slots: u32,
    #[arg(long, default_value_t = 8 * 1024 * 1024 * 1024)]
    memory_bytes: u64,
    #[arg(long, default_value_t = 4000)]
    cpu_millis: u64,
    #[arg(long, default_value_t = 1000)]
    poll_ms: u64,
    #[arg(long, value_parser = pair)]
    label: Vec<(String, String)>,
    #[arg(long)]
    cache_key: Vec<String>,
}
fn pair(value: &str) -> Result<(String, String), String> {
    value
        .split_once('=')
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .ok_or_else(|| "expected key=value".into())
}
struct Active {
    key: LeaseKey,
    resources: Resources,
    full_resources: Resources,
    deadline: Instant,
    lease_clock: watch::Sender<Instant>,
    stop: watch::Sender<bool>,
    native_terminal: bool,
    commands: mpsc::Sender<ControlCommand>,
    acknowledgement: Option<ControlAcknowledgement>,
    control_revision: u64,
    rejection: Option<AdmissionRejection>,
    retain_bundle: bool,
    retention: Option<ArtifactRetention>,
    memory_controls: Option<pvisor::RunControlHandle>,
}

fn publish_memory_targets(
    active: &BTreeMap<String, Active>,
    sender: &Option<watch::Sender<Vec<memory::Target>>>,
) {
    if let Some(sender) = sender {
        sender.send_replace(
            active
                .values()
                .filter_map(|entry| {
                    Some(memory::Target {
                        key: entry.key.clone(),
                        controls: entry.memory_controls.clone()?,
                    })
                })
                .collect(),
        );
    }
}

// HTTP and durable outbox I/O never execute inside the renewal branch.
const MAX_DELIVERIES: usize = 16;
#[derive(Clone)]
enum Delivery {
    Decline(AdmissionRejection),
    Acknowledge(ControlAcknowledgement),
    Complete {
        completion: Box<Completion>,
        retain_bundle: bool,
        retention: Option<ArtifactRetention>,
    },
}
impl Delivery {
    fn key(&self) -> &LeaseKey {
        match self {
            Self::Decline(value) => &value.key,
            Self::Acknowledge(value) => &value.command.key,
            Self::Complete { completion, .. } => &completion.key,
        }
    }
    fn id(&self) -> String {
        let key = outbox::key_name(self.key());
        match self {
            Self::Decline(_) => format!("decline-{key}"),
            Self::Acknowledge(value) => format!("ack-{key}-{}", value.command.revision),
            Self::Complete { .. } => format!("complete-{key}"),
        }
    }
    async fn send(self, client: Client, outbox: Arc<outbox::Outbox>) -> DeliveryResult {
        let mut durable = false;
        let mut storage_error = false;
        let result = match &self {
            Self::Decline(value) => client.decline(value).await.map(|_| ()),
            Self::Acknowledge(value) => client.acknowledge_control(value).await.map(|_| ()),
            Self::Complete {
                completion,
                retain_bundle,
                retention,
            } => {
                match outbox::save(
                    outbox.clone(),
                    completion.as_ref().clone(),
                    *retain_bundle,
                    true,
                    retention.clone(),
                )
                .await
                {
                    Err(error) => {
                        storage_error = true;
                        Err(error)
                    }
                    Ok(pending) => {
                        durable = true;
                        let disposition = match client.complete(completion).await {
                            Ok(task) => Ok(outbox::Disposition::Accepted { phase: task.phase }),
                            Err(error) if outbox::conflict(&error) => {
                                Ok(outbox::Disposition::Fenced)
                            }
                            Err(error) => Err(error),
                        };
                        match disposition {
                            Err(error) => Err(error),
                            Ok(disposition) => {
                                let result = outbox::finish(outbox, pending, disposition).await;
                                storage_error = result.is_err();
                                result
                            }
                        }
                    }
                }
            }
        };
        DeliveryResult {
            delivery: self,
            result,
            durable,
            storage_error,
        }
    }
}
struct DeliveryResult {
    delivery: Delivery,
    result: anyhow::Result<()>,
    durable: bool,
    storage_error: bool,
}
struct TerminalDelivery {
    completion: Completion,
    retain_bundle: bool,
    retention: Option<ArtifactRetention>,
    durable: bool,
    retry_after: Instant,
}
fn queue_delivery(
    delivery: Delivery,
    jobs: &mut JoinSet<DeliveryResult>,
    in_flight: &mut BTreeSet<String>,
    client: &Client,
    outbox: &Arc<outbox::Outbox>,
) {
    let terminal = matches!(delivery, Delivery::Complete { .. });
    let class_used = in_flight
        .iter()
        .filter(|id| id.starts_with("complete-") == terminal)
        .count();
    // Reserve half the budget for each failure domain: repeated control errors
    // cannot starve terminal delivery, and a terminal backlog cannot starve acks.
    if class_used < MAX_DELIVERIES / 2
        && jobs.len() < MAX_DELIVERIES
        && in_flight.insert(delivery.id())
    {
        jobs.spawn(delivery.send(client.clone(), outbox.clone()));
    }
}
fn expire_active(
    active: &mut BTreeMap<String, Active>,
    terminal: &BTreeMap<String, TerminalDelivery>,
) {
    active.retain(|_, entry| {
        if Instant::now() < entry.deadline {
            return true;
        }
        entry.stop.send_replace(true);
        // Only a known native terminal with a durable delivery owner can release
        // its reservation. A running/unknown execution must remain supervised.
        !(entry.native_terminal
            && terminal
                .get(&outbox::key_name(&entry.key))
                .is_some_and(|pending| pending.durable))
    });
}

#[derive(Clone)]
struct NodeSample {
    started: Instant,
    measurements: Result<NodeMeasurements, String>,
}
fn node_sampler(mode: AdmissionMode, interval: Duration) -> watch::Receiver<NodeSample> {
    let (tx, rx) = watch::channel(NodeSample {
        started: Instant::now(),
        measurements: Err("awaiting first node sample".into()),
    });
    if mode == AdmissionMode::LinuxPressure {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tick.tick().await;
                let started = Instant::now();
                // Exactly one probe can be in flight. A stalled read ages out
                // the cached sample; it never blocks lease timers or spawns
                // an accumulating queue of replacement probes.
                let measurements = match tokio::task::spawn_blocking(sample_linux).await {
                    Ok(result) => result.map_err(|error| format!("{error:#}")),
                    Err(error) => Err(format!("node probe failed: {error}")),
                };
                if tx
                    .send(NodeSample {
                        started,
                        measurements,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
    }
    rx
}
fn node_report(
    policy: &AdmissionPolicy,
    capacity: Resources,
    used: Resources,
    sample: &NodeSample,
    lease_limit_ms: Option<u64>,
) -> anyhow::Result<AdmissionReport> {
    let policy = AdmissionPolicy {
        max_sample_age_ms: policy
            .max_sample_age_ms
            .min(lease_limit_ms.unwrap_or(policy.max_sample_age_ms)),
        ..policy.clone()
    };
    policy.report(
        capacity,
        used,
        sample
            .started
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX),
        sample.measurements.clone(),
    )
}
fn resume_allowed(report: &AdmissionReport, used: Resources) -> bool {
    report.mode == AdmissionMode::Reservations
        || (report.error.is_none()
            && !report
                .blocked
                .iter()
                .any(|block| *block != AdmissionBlock::CpuQuota)
            && report.measurements.as_ref().is_some_and(|_| {
                report
                    .cpu_reservation_limit_millis()
                    .is_some_and(|limit| used.cpu_millis <= limit)
            }))
}

fn executor(
    args: &Args,
    config: &WorkerProfile,
    resources: Resources,
) -> anyhow::Result<Arc<dyn RunExecutor>> {
    Ok(match args.backend {
        Backend::Host => Arc::new(ProcessExecutor::default()),
        Backend::Rootless => Arc::new(ProcessExecutor::rootless_with_launcher(
            std::env::current_exe()?,
        )?),
        Backend::Container => Arc::new(ContainerExecutor::new(config.container.clone())?),
        Backend::Vm => Arc::new(with_cpu_controls(
            VmExecutor::new(vm_settings(config, resources)?)?,
            config,
        )),
    })
}

fn with_cpu_controls(executor: VmExecutor, config: &WorkerProfile) -> VmExecutor {
    #[cfg(target_os = "linux")]
    {
        let executor = if config.cpu_sampling.enabled {
            executor.with_cpu_observation()
        } else {
            executor
        };
        if let Some(group) = &config.cpu_group {
            executor.with_cpu_qos_group(group.clone())
        } else {
            executor
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = config;
        executor
    }
}

fn vm_settings(config: &WorkerProfile, resources: Resources) -> anyhow::Result<pvisor::VmSettings> {
    let mut settings = config.vm.clone();
    ensure!(
        resources.memory_bytes.is_multiple_of(1024 * 1024),
        "VM memory budget must be a whole MiB"
    );
    settings.memory_mib = u32::try_from(resources.memory_bytes / (1024 * 1024))?;
    settings.cpus = u16::try_from(resources.cpu_millis.div_ceil(1000))?;
    settings.ram_backing = None;
    settings.rootfs_immutable = true;
    Ok(settings)
}

fn runtime(
    args: &Args,
    config: &WorkerProfile,
    assignment: &Assignment,
    storage: &Path,
) -> anyhow::Result<PVisor> {
    assignment.spec.validate_cpu_qos()?;
    assignment.spec.validate_gateway()?;
    assignment.spec.validate_artifacts()?;
    ensure!(
        assignment.spec.cpu_qos.is_none()
            || (config.cpu_qos.enabled
                && cfg!(target_os = "linux")
                && matches!(args.backend, Backend::Vm)),
        "worker cannot enforce requested CPU QoS"
    );
    let (executor, restore_overlay): (Arc<dyn RunExecutor>, Option<pvisor::OverlayHint>) =
        if let Some(checkpoint) = &assignment.checkpoint {
            ensure!(
                assignment.spec.restore.is_some()
                    && matches!(args.backend, Backend::Vm)
                    && config.overlaynet.mode == pvisor::OverlayNetMode::Off,
                "execution restore assignment/profile mismatch"
            );
            let state = args.state.canonicalize()?;
            let store = checkpoint.store.canonicalize()?;
            if store.starts_with(state.join("checkpoint-imports")) {
                let publication = assignment
                    .checkpoint_publication
                    .as_ref()
                    .context("imported checkpoint has no controller provenance")?;
                checkpoints::Repository::validate_import(&state, checkpoint, publication)?;
            } else {
                ensure!(
                    store.starts_with(state.join("tasks"))
                        && store
                            .file_name()
                            .is_some_and(|name| name == "execution-snapshots"),
                    "execution checkpoint is outside this Worker's owned task storage"
                );
                let source = pvisor::RunRecord::read(
                    store.parent().context("snapshot source storage missing")?,
                )?;
                ensure!(
                    source.run_id == checkpoint.source_run_id
                        && source.attempt_id.as_deref() == Some(&checkpoint.source_attempt_id),
                    "execution checkpoint source Run/Attempt storage binding mismatch"
                );
            }
            let (executor, overlay) = VmExecutor::restore(
                vm_settings(config, assignment.spec.resources)?,
                checkpoint.clone(),
                storage,
            )?;
            (Arc::new(with_cpu_controls(executor, config)), Some(overlay))
        } else {
            ensure!(
                assignment.spec.restore.is_none(),
                "execution restore assignment has no checkpoint"
            );
            (executor(args, config, assignment.spec.resources)?, None)
        };
    let descriptor = executor.descriptor();
    ensure!(
        assignment.spec.execution
            == ExecutionClass {
                executor: descriptor.kind,
                isolation: descriptor.isolation
            },
        "worker cannot enforce selected execution class"
    );
    let network = pvisor_core::NetworkConfig {
        mode: match config.overlaynet.policy {
            pvisor::OverlayNetPolicy::Public => pvisor_core::NetworkMode::Public,
            pvisor::OverlayNetPolicy::Deny => pvisor_core::NetworkMode::NoNetwork,
            pvisor::OverlayNetPolicy::Allowlist => pvisor_core::NetworkMode::Allowlist,
        },
        allowed_hosts: config.overlaynet.allow.clone(),
        rules: config.overlaynet.rules.clone(),
        deny_rules: config.overlaynet.deny.clone(),
        limits: config.overlaynet.limits.clone(),
        capability: None,
    };
    let mut builder = PVisor::builder()
        .executors(vec![executor])
        .storage(storage)
        .event_sink(Arc::new(pvisor::trace::Journal::open(
            &storage.join("trace"),
        )?))
        .network(
            pvisor::NetworkDriverConfig::new(config.overlaynet.mode, network).listen("127.0.0.1:0"),
        );
    builder = gateway::attach(builder, &config.gateway, &assignment.spec, storage)?;
    if let Some(overlay) = restore_overlay {
        builder = builder.overlay(overlay);
    } else if !config.lower_layers.is_empty() {
        // Core filesystem rules remain in RunSpec. Host-owned lower layers are
        // shared; writable upper and merged mount are private to each lease.
        builder = builder.overlay(pvisor::OverlayHint {
            lower_dirs: config.lower_layers.clone(),
            stage_dir: Some(storage.to_owned()),
            protect_target: true,
            ..Default::default()
        });
    }
    Ok(builder.build())
}

#[derive(Clone)]
struct AttemptRuntime {
    args: Arc<Args>,
    profile: Arc<WorkerProfile>,
    environments: Option<Arc<environment::EnvironmentMounts>>,
}
struct PreparedRuntime {
    runtime: PVisor,
    mounts: environment::MountOwners<pvisor::cache::MountedImage>,
}
impl AttemptRuntime {
    async fn prepare(
        &self,
        assignment: &Assignment,
        storage: &Path,
    ) -> anyhow::Result<PreparedRuntime> {
        let mut profile = (*self.profile).clone();
        let mounts = if assignment.checkpoint.is_some() {
            environment::MountOwners::new()
        } else {
            match (&assignment.spec.environment, &assignment.environment) {
                (None, None) => environment::MountOwners::new(),
                (Some(digest), Some(record)) => {
                    ensure!(
                        *digest == record.digest && matches!(self.args.backend, Backend::Vm),
                        "environment assignment identity/backend mismatch"
                    );
                    ensure!(
                        profile.lower_layers.is_empty(),
                        "immutable environment cannot include unversioned worker lower_layers"
                    );
                    let pool = self
                        .environments
                        .as_ref()
                        .context("worker immutable environments are disabled")?;
                    let mounts = pool.prepare(record).await?;
                    profile.vm.rootfs = Some(mounts.mounts()[0].rootfs().to_owned());
                    profile.vm.image = None;
                    profile.lower_layers = mounts
                        .mounts()
                        .iter()
                        .rev()
                        .map(|m| m.rootfs().to_owned())
                        .collect();
                    mounts
                }
                _ => anyhow::bail!("environment assignment is incomplete"),
            }
        };
        let args = self.args.clone();
        let mut assigned = assignment.clone();
        if let Some(publication) = &assignment.checkpoint_publication {
            publication.validate()?;
            ensure!(
                assignment.checkpoint.as_ref() == Some(&publication.checkpoint)
                    && assignment.spec.restore.is_some(),
                "checkpoint publication does not match restore assignment"
            );
            let local = publication
                .checkpoint
                .store
                .canonicalize()
                .ok()
                .is_some_and(|store| {
                    store.starts_with(self.args.state.join("tasks"))
                        && store
                            .join("objects")
                            .join(&publication.checkpoint.snapshot_id)
                            .is_dir()
                });
            if !local {
                let repository = profile
                    .checkpoints
                    .as_ref()
                    .context("remote checkpoint repository is disabled")?;
                assigned.checkpoint = Some(repository.import(&self.args.state, publication).await?);
            }
        }

        let destination = storage.to_owned();
        tokio::task::spawn_blocking(move || {
            // A cancelled preparation must keep its lowers alive until the
            // blocking runtime construction finishes, even if nobody awaits it.
            Ok(PreparedRuntime {
                runtime: runtime(&args, &profile, &assigned, &destination)?,
                mounts,
            })
        })
        .await?
    }
}
async fn lease_expired(mut clock: watch::Receiver<Instant>) {
    loop {
        let deadline = *clock.borrow();
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            changed = clock.changed() => { if changed.is_err() { break; } }
        }
    }
}

struct AttemptChannels {
    stop: watch::Receiver<bool>,
    lease_clock: watch::Receiver<Instant>,
    commands: mpsc::Receiver<ControlCommand>,
    acknowledgements: mpsc::Sender<ControlAcknowledgement>,
    memory_ready: Option<mpsc::Sender<(LeaseKey, pvisor::RunControlHandle)>>,
    native_done: mpsc::Sender<NativeDoneReceipt>,
}

async fn upload_retry(client: &Client, key: &LeaseKey, bytes: Vec<u8>) -> anyhow::Result<BlobRef> {
    let mut delay = Duration::from_millis(50);
    loop {
        match client.upload_artifact(key, bytes.clone()).await {
            Ok(reference) => return Ok(reference),
            Err(error) => {
                let retry = error.downcast_ref::<reqwest::Error>().is_some_and(|e| {
                    e.status().is_none_or(|s| {
                        (s.is_server_error() && s != reqwest::StatusCode::INSUFFICIENT_STORAGE)
                            || s == reqwest::StatusCode::TOO_MANY_REQUESTS
                    })
                });
                if !retry {
                    return Err(error);
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(2));
            }
        }
    }
}

async fn execute(
    runtime: AttemptRuntime,
    assignment: Assignment,
    channels: AttemptChannels,
    storage: PathBuf,
    client: Client,
    outbox: Arc<outbox::Outbox>,
) -> Completion {
    let AttemptChannels {
        mut stop,
        mut lease_clock,
        mut commands,
        acknowledgements,
        memory_ready,
        native_done,
    } = channels;
    let requested_bundle = assignment.spec.requires_artifacts();
    let retention = assignment.spec.retain_artifacts.clone();
    let checkpoint_repository = runtime.profile.checkpoints.clone();
    let checkpoint_filesystem_pool = runtime.profile.vm.snapshot_filesystem_pool.clone();
    let mut export_journal = None;
    let mut terminal_control = None;
    let lease_key = assignment.lease.key.clone();
    let mut mounts = environment::MountOwners::new();
    let result: anyhow::Result<pvisor_core::RunResult> = async {
        ensure!(!*stop.borrow(), "lease ended before environment preparation");
        let prepared = {
          let preparing = runtime.prepare(&assignment, &storage);
          tokio::pin!(preparing);
          tokio::select! {
            prepared = &mut preparing => prepared?,
            _ = stop.changed() => { let _ = preparing.await; anyhow::bail!("environment preparation cancelled") },
            _ = lease_expired(lease_clock.clone()) => { let _ = preparing.await; anyhow::bail!("environment preparation lease expired") },
          }
        };
        let runtime = prepared.runtime;
        export_journal = runtime.journal();
        mounts = prepared.mounts; // Keep shared lowers through native teardown.
        let mut spec = assignment.spec.run;
        ensure!(!spec.metadata.contains_key("pvisor.orchestration.artifact_retention"), "task overrides artifact retention provenance");
        if let Some(retention) = &retention {
            spec.metadata.insert("pvisor.orchestration.artifact_retention".into(), serde_json::to_value(retention)?);
        }
        spec.runtime.cpu_qos = assignment.spec.cpu_qos;
        ensure!(!spec.metadata.contains_key("pvisor.orchestration.gateway"), "task overrides Gateway provenance");
        if let Some(requirement) = assignment.spec.gateway {
            spec.metadata.insert("pvisor.orchestration.gateway".into(), serde_json::to_value(requirement)?);
        }
        ensure!(!spec.metadata.contains_key("pvisor.orchestration.checkpoint_publication"), "task overrides checkpoint publication provenance");
        if let Some(publication) = assignment.checkpoint_publication {
            spec.metadata.insert("pvisor.orchestration.checkpoint_publication".into(), serde_json::to_value(publication)?);
        }
        ensure!(!spec.metadata.contains_key("pvisor.orchestration.execution_restore"), "task overrides execution restore provenance");
        if let Some(checkpoint) = assignment.checkpoint {
            spec.metadata.insert("pvisor.lineage".into(), serde_json::json!({"parent_run_id": checkpoint.source_run_id, "checkpoint_id": checkpoint.snapshot_id}));
            spec.metadata.insert("pvisor.orchestration.execution_restore".into(), serde_json::to_value(checkpoint)?);
        }
        if let Some(record) = assignment.environment {
            let RunInvocation::Process(process) = &spec.invocation;
            ensure!(!spec.metadata.keys().any(|k| k.starts_with("pvisor.vm.") || k.starts_with("pvisor.orchestration.environment")), "environment task overrides host preparation metadata");
            let cwd = process.cwd.clone().unwrap_or_else(|| "/".into());
            ensure!(Path::new(&cwd).is_absolute(), "environment cwd must be an absolute guest path");
            spec.metadata.insert("pvisor.vm.guest_cwd".into(), serde_json::json!(cwd));
            spec.metadata.insert("pvisor.orchestration.environment".into(), serde_json::to_value(record)?);
        }
        let output_limit = spec.runtime.max_output_bytes;
        let RunInvocation::Process(process) = &mut spec.invocation;
        ensure!(!process.inherit_env, "worker refuses inherited environment");
        process.stdin = StdioMode::Null;
        process.stdout = StdioMode::Capture;
        process.stderr = StdioMode::Capture;
        // The common RunSpec runtime stays authoritative for policies, output
        // bounds and timeouts; no shell-string or metadata dispatch layer.
        ensure!(
            !*stop.borrow() && Instant::now() < *lease_clock.borrow(),
            "lease ended before execution"
        );
        let handle = runtime.run(spec).await?;
        let cancellation = handle.cancellation();
        let controls = handle.controls();
        if let Some(sender) = memory_ready {
            let _ = sender.try_send((lease_key.clone(), controls.clone()));
        }
        let mut halted = *stop.borrow() || Instant::now() >= *lease_clock.borrow();
        if halted {
            cancellation.cancel();
        }
        tokio::pin! { let wait = handle.wait(); }
        let expired = async {
            loop {
                let deadline = *lease_clock.borrow();
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => break,
                    change = lease_clock.changed() => { if change.is_err() { break; } }
                }
            }
        };
        tokio::pin!(expired);
        let mut last_acknowledgement: Option<ControlAcknowledgement> = None;
        let mut result = 'execution: loop {
            tokio::select! {
                result = &mut wait => break result,
                _ = stop.changed(), if !halted => { halted = true; cancellation.cancel(); },
                _ = &mut expired, if !halted => { halted = true; cancellation.cancel(); },
                command = commands.recv(), if !halted => {
                    let Some(command) = command else { halted = true; cancellation.cancel(); continue; };
                    if command.key != assignment.lease.key { halted = true; cancellation.cancel(); continue; }
                    if let Some(previous) = &last_acknowledgement {
                        if previous.command == command {
                            let _ = acknowledgements.try_send(previous.clone());
                            continue;
                        }
                        if command.revision < previous.command.revision { continue; }
                        if command.revision == previous.command.revision { halted = true; cancellation.cancel(); continue; }
                    }
                    // Native control and task completion can progress concurrently.
                    // Lease expiry interrupts waiting without acknowledging uncertain
                    // effects; cancellation owns the eventual native teardown.
                    let action = command.request.action;
                    let request_id = command.request.request_id.clone();
                    let operation = async {
                        controls.clone().wait_ready().await?;
                        let operation = action.operation(&request_id);
                        controls.control(operation).await
                    };
                    tokio::pin!(operation);
                    let outcome = tokio::select! {
                        result = &mut wait => break 'execution result,
                        _ = stop.changed() => { halted = true; cancellation.cancel(); continue 'execution; },
                        _ = &mut expired => { halted = true; cancellation.cancel(); continue 'execution; },
                        result = &mut operation => result,
                    };
                    let mut outcome = match outcome {
                        Ok(pvisor_core::operation::Value::Vm { state, memory }) => ControlOutcome::Succeeded { state, memory },
                        Ok(pvisor_core::operation::Value::ExecutionCheckpoint { checkpoint }) => ControlOutcome::Checkpointed { checkpoint },
                        Ok(_) => ControlOutcome::Failed { error: "native control returned no VM observation".into() },
                        Err(error) => {
                            let mut message = Some(format!("{error:#}"));
                            bound_text(&mut message, &mut false, 8192);
                            ControlOutcome::Failed { error: message.unwrap() }
                        },
                    };
                    if let Err(error) = outcome.validate(command.request.action) {
                        outcome = ControlOutcome::Failed { error: format!("invalid native control observation: {error}") };
                    }
                    let acknowledgement = ControlAcknowledgement { command, outcome };
                    if let Err(error) = persist(&storage.join(format!("control-{}.json", acknowledgement.command.revision)), &acknowledgement) {
                        eprintln!("worker control evidence write failed: {error:#}");
                        halted = true;
                        cancellation.cancel();
                        continue;
                    }
                    if matches!(acknowledgement.outcome, ControlOutcome::Failed { .. }) {
                        halted = true;
                        cancellation.cancel();
                    }
                    // Full channels never block the lease timer: redelivery of
                    // the same command replays this durable acknowledgement.
                    let _ = acknowledgements.try_send(acknowledgement.clone());
                    last_acknowledgement = Some(acknowledgement);
                }
            }
        }?;
        terminal_control = last_acknowledgement;
        // Lossy UTF-8 decoding can expand raw output. Bound the wire form as
        // well as the executor's byte buffer so results remain deliverable.
        bound_text(
            &mut result.output.stdout,
            &mut result.output.stdout_truncated,
            output_limit,
        );
        bound_text(
            &mut result.output.stderr,
            &mut result.output.stderr_truncated,
            output_limit,
        );
        // JSON can expand control bytes sixfold. Keep each delivered stream
        // within 1 MiB encoded, leaving room under the 4 MiB HTTP body limit.
        // The full native capture stays in the Bundle when retention is required.
        bound_json_text(&mut result.output.stdout, &mut result.output.stdout_truncated, 1024 * 1024);
        bound_json_text(&mut result.output.stderr, &mut result.output.stderr_truncated, 1024 * 1024);
        Ok(result)
    }
    .await;
    // Do not publish completion or release controller reservations until the
    // native teardown and final shared-lower release have both finished. The
    // blocking join must leave heartbeat and lease timers free to run.
    let release = mounts.release().await;
    let result = result.and_then(|result| release.map(|()| result));
    let mut completion = match result {
        Ok(result) => Completion {
            key: lease_key.clone(),
            result: Some(result),
            error: None,
            artifacts: None,
            artifact_error: None,
        },
        Err(error) => {
            let mut message = Some(format!("{error:#}"));
            bound_text(&mut message, &mut false, 8192);
            Completion {
                key: lease_key,
                result: None,
                error: message,
                artifacts: None,
                artifact_error: None,
            }
        }
    };
    // Native execution has already terminated. Preserve that result before any
    // potentially long/retried upload so restart can publish without re-execution.
    let native_durable = if requested_bundle && completion.result.is_some() {
        match outbox::save(
            outbox.clone(),
            completion.clone(),
            true,
            false,
            retention.clone(),
        )
        .await
        {
            Ok(_) => true,
            Err(error) => {
                eprintln!("worker native terminal outbox write failed: {error:#}");
                false
            }
        }
    } else {
        false
    };
    if requested_bundle && let Some(result) = &completion.result {
        let sealed = if *stop.borrow() {
            Err(anyhow::anyhow!(
                "bundle retention interrupted by cancellation"
            ))
        } else {
            artifacts::seal_attempt(
                &completion.key,
                result,
                &storage,
                retention.clone(),
                export_journal.clone(),
                checkpoint_repository.clone(),
                checkpoint_filesystem_pool.clone(),
            )
            .await
        };
        drop(export_journal.take());
        if sealed.is_ok() && native_durable && !*stop.borrow() {
            // A very fast exit after resume can overtake the main-loop ack.
            // Preserve the observed control result before a handoff can settle
            // pending controls or clear the local native accounting.
            let controls_settled = if let Some(acknowledgement) = &terminal_control {
                tokio::select! {
                    receipt = client.acknowledge_control(acknowledgement) => receipt.is_ok(),
                    _ = stop.changed() => false,
                    _ = lease_expired(lease_clock.clone()) => false,
                }
            } else {
                true
            };
            let acknowledged = tokio::select! {
                receipt = async {
                    if controls_settled { artifacts::handoff(&client, &completion.key, result).await }
                    else { None }
                } => receipt,
                _ = stop.changed() => None,
                _ = lease_expired(lease_clock.clone()) => None,
            };
            if let Some(receipt) = acknowledged {
                // Only a matching, durable controller receipt permits local reuse.
                let _ = native_done.send(receipt).await;
            }
        }
        let expired = async {
            loop {
                let deadline = *lease_clock.borrow();
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => break,
                    changed = lease_clock.changed() => { if changed.is_err() { break; } },
                }
            }
        };
        let exported = match sealed {
            Err(error) => Err(error),
            Ok(_) if *stop.borrow() => Err(anyhow::anyhow!(
                "bundle retention interrupted by cancellation"
            )),
            Ok(manifest) => tokio::select! {
                exported = artifacts::upload(&client, &completion.key, &storage, manifest) => exported,
                _ = stop.changed() => Err(anyhow::anyhow!("bundle retention interrupted by cancellation")),
                _ = expired => Err(anyhow::anyhow!("bundle retention lease expired")),
            },
        };
        match exported {
            Ok(reference) => completion.artifacts = Some(reference),
            Err(error) => {
                let mut message = Some(format!("{error:#}"));
                bound_text(&mut message, &mut false, 8192);
                completion.artifact_error = message;
            }
        }
    }
    if let Err(error) = outbox::save(
        outbox,
        completion.clone(),
        requested_bundle,
        true,
        retention.clone(),
    )
    .await
    {
        eprintln!("worker final outbox write failed: {error:#}");
    }
    // Keep evidence even after acknowledgement; the controller journal retains
    // small results while worker storage retains local bundles/trace/artifacts.
    if let Err(error) = persist(&storage.join("completion.json"), &completion) {
        eprintln!("worker completion evidence write failed: {error:#}");
    }
    completion
}
fn bound_text(text: &mut Option<String>, truncated: &mut bool, limit: usize) {
    if let Some(text) = text
        && text.len() > limit
    {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        *truncated = true;
    }
}
fn bound_json_text(text: &mut Option<String>, truncated: &mut bool, limit: usize) {
    let Some(text) = text else {
        return;
    };
    // Small normal agent output needs no second traversal.
    if text.len() <= limit / 6 {
        return;
    }
    let mut encoded = 0;
    let cut = text.char_indices().find_map(|(position, character)| {
        let cost = match character {
            '"' | '\\' | '\u{0008}' | '\u{000c}' | '\n' | '\r' | '\t' => 2,
            character if character < ' ' => 6,
            character => character.len_utf8(),
        };
        encoded += cost;
        (encoded > limit).then_some(position)
    });
    if let Some(position) = cut {
        text.truncate(position);
        *truncated = true;
    }
}
fn persist(path: &Path, value: &impl serde::Serialize) -> anyhow::Result<()> {
    outbox::persist(path, value)
}

fn main() -> anyhow::Result<()> {
    // Linux user-namespace setup requires a single-threaded process. Re-enter
    // native VM/rootless launchers before Tokio creates any worker threads.
    if pvisor::run_krun_internal_if_requested()? || pvisor::sandbox::run_internal_if_requested()? {
        return Ok(());
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(worker_main())
}
async fn worker_main() -> anyhow::Result<()> {
    let mut args = Args::parse();
    ensure!(
        args.poll_ms > 0 && args.poll_ms <= 10_000,
        "poll interval must be 1..10000 ms"
    );
    std::fs::create_dir_all(&args.state)?;
    args.state = args.state.canonicalize()?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(args.state.join("owner.lock"))?;
    lock.try_lock_exclusive()
        .context("worker state already owned")?;
    let client = Client::new(&args.url, args.token.clone())?;
    let outbox = Arc::new(outbox::Outbox::open(&args.state, &args.id, &args.url)?);
    let mut config: WorkerProfile = match &args.config {
        Some(path) => toml::from_str(&std::fs::read_to_string(path)?)?,
        None => WorkerProfile::default(),
    };
    if let Some(pool) = &config.vm.snapshot_filesystem_pool {
        ensure!(
            cfg!(all(target_os = "linux", target_arch = "x86_64"))
                && matches!(args.backend, Backend::Vm)
                && config.overlaynet.mode == pvisor::OverlayNetMode::Off
                && config.vm.memory_pool.is_none()
                && !config.vm.ram_compression
                && config.vm.ram_backing.is_none(),
            "immutable snapshot pool requires the private-RAM no-network native Linux VM profile"
        );
        ensure!(
            pool.is_absolute(),
            "snapshot filesystem pool must be an absolute host path"
        );
        std::fs::create_dir_all(pool)?;
        pvisor::environment_snapshot::SnapshotStore::new(pool)?;
        config.vm.snapshot_filesystem_pool = Some(pool.canonicalize()?);
    }
    if let Some(profile) = &config.checkpoint_storage {
        ensure!(
            cfg!(all(target_os = "linux", target_arch = "x86_64"))
                && matches!(args.backend, Backend::Vm)
                && config.overlaynet.mode == pvisor::OverlayNetMode::Off
                && config.vm.memory_pool.is_none()
                && !config.vm.ram_compression,
            "checkpoint repository requires a no-network native Linux VM profile"
        );
        config.checkpoints = Some(Arc::new(checkpoints::Repository::new(profile, &config.vm)?));
    }
    outbox::recover(
        outbox.clone(),
        client.clone(),
        args.poll_ms,
        config.checkpoints.clone(),
        config.vm.snapshot_filesystem_pool.clone(),
    )
    .await?;
    config.gateway.validate(config.overlaynet.mode)?;
    if config.cpu_qos.enabled {
        ensure!(
            cfg!(target_os = "linux") && matches!(args.backend, Backend::Vm),
            "cpu_qos requires a Linux VM worker"
        );
        #[cfg(target_os = "linux")]
        {
            config.cpu_group =
                Some(tokio::task::spawn_blocking(pvisor::CpuQosGroup::shared).await??);
        }
    }
    ensure!(
        (1000..=60_000).contains(&config.cpu_sampling.interval_ms),
        "CPU sampling interval must be 1000..60000 ms"
    );
    ensure!(
        !config.cpu_sampling.enabled
            || (cfg!(target_os = "linux") && matches!(args.backend, Backend::Vm)),
        "cpu_sampling requires a Linux VM worker"
    );
    ensure!(
        (1000..=60_000).contains(&config.memory_sampling.interval_ms),
        "memory sampling interval must be 1000..60000 ms"
    );
    if config.memory_sampling.enabled {
        ensure!(
            cfg!(target_os = "linux") && matches!(args.backend, Backend::Vm),
            "memory_sampling requires a Linux VM worker"
        );
    }
    let args = Arc::new(args);
    let config = Arc::new(config);
    let environments = if config.environments.enabled {
        ensure!(
            matches!(args.backend, Backend::Vm),
            "immutable environment profile requires VM backend"
        );
        ensure!(
            config.lower_layers.is_empty(),
            "immutable environment profile cannot include unversioned lower_layers"
        );
        Some(Arc::new(environment::EnvironmentMounts::new(
            &args.state,
            config.environments.max_layers,
        )?))
    } else {
        None
    };
    let attempt_runtime = AttemptRuntime {
        args: args.clone(),
        profile: config.clone(),
        environments,
    };
    config.admission.validate()?;
    ensure!(
        config.admission.mode != AdmissionMode::LinuxPressure
            || config.admission.max_sample_age_ms >= args.poll_ms * 2,
        "node sample age limit must allow at least two poll intervals"
    );
    let samples = node_sampler(config.admission.mode, Duration::from_millis(args.poll_ms));
    let capacity = Resources {
        slots: args.slots,
        memory_bytes: args.memory_bytes,
        cpu_millis: args.cpu_millis,
    };
    ensure!(
        capacity.slots > 0 && capacity.memory_bytes > 0 && capacity.cpu_millis > 0,
        "worker capacity must be positive"
    );
    let class = match args.backend {
        Backend::Host => ExecutionClass {
            executor: ExecutorKind::Process,
            isolation: IsolationKind::HostProcess,
        },
        Backend::Rootless => ExecutionClass {
            executor: ExecutorKind::Process,
            isolation: IsolationKind::RootlessProcess,
        },
        Backend::Container => ExecutionClass {
            executor: ExecutorKind::Container,
            isolation: IsolationKind::Container,
        },
        Backend::Vm => ExecutionClass {
            executor: ExecutorKind::VirtualMachine,
            isolation: IsolationKind::VirtualMachine,
        },
    };
    let uses_memory_pool = config.vm.memory_pool.is_some()
        || (cfg!(all(target_os = "macos", target_arch = "aarch64"))
            && std::env::var_os("PVISOR_EXPERIMENTAL_MEMORY_POOL").is_some());
    let registration = WorkerRegistration {
        checkpoint_storage: config
            .checkpoints
            .as_ref()
            .map(|repository| repository.support.clone()),
        artifact_export: Some(ArtifactExportSupport {
            execution_checkpoint: config
                .checkpoints
                .as_ref()
                .is_some_and(|repository| repository.support.publish),
            version: ARTIFACT_EXPORT_VERSION,
            trace: true,
            workspace_upper: matches!(args.backend, Backend::Vm),
        }),
        gateway: config.gateway.support(),
        cpu_observation_protocol: config
            .cpu_sampling
            .enabled
            .then_some(pvisor_core::cpu::CPU_OBSERVATION_PROTOCOL_VERSION),
        cpu_qos_classes: if config.cpu_qos.enabled {
            vec![
                pvisor_core::CpuQosClass::BestEffort,
                pvisor_core::CpuQosClass::LatencySensitive,
            ]
        } else {
            vec![]
        },
        parked_execution_suspend_protocol: (matches!(args.backend, Backend::Vm)
            && cfg!(all(target_os = "linux", target_arch = "x86_64"))
            && config.overlaynet.mode == pvisor::OverlayNetMode::Off
            && !uses_memory_pool
            && !config.vm.ram_compression)
            .then_some(CLUSTER_VERSION),
        execution_restore_protocol: (matches!(args.backend, Backend::Vm)
            && cfg!(any(
                all(target_os = "linux", target_arch = "x86_64"),
                all(target_os = "macos", target_arch = "aarch64")
            ))
            && config.overlaynet.mode == pvisor::OverlayNetMode::Off
            && !uses_memory_pool
            && !config.vm.ram_compression)
            .then_some(CLUSTER_VERSION),
        version: CLUSTER_VERSION,
        id: args.id.clone(),
        incarnation: uuid::Uuid::new_v4().to_string(),
        capacity,
        execution: vec![class],
        labels: args.label.iter().cloned().collect(),
        cache_keys: args.cache_key.clone(),
        vm_control_protocol: matches!(args.backend, Backend::Vm).then_some(CLUSTER_VERSION),
        artifact_protocol: Some(CLUSTER_VERSION),
        environment_support: attempt_runtime
            .environments
            .as_ref()
            .map(|_| EnvironmentSupport {
                version: CLUSTER_VERSION,
                architecture: match std::env::consts::ARCH {
                    "x86_64" => "amd64",
                    "aarch64" => "arm64",
                    other => other,
                }
                .into(),
            }),
        vm_control_actions: if matches!(args.backend, Backend::Vm) {
            let mut actions = vec![ControlAction::Pause, ControlAction::Resume];
            if !uses_memory_pool {
                actions.push(ControlAction::Offload);
                if cfg!(any(
                    all(target_os = "linux", target_arch = "x86_64"),
                    all(target_os = "macos", target_arch = "aarch64")
                )) && config.overlaynet.mode == pvisor::OverlayNetMode::Off
                {
                    actions.push(ControlAction::Checkpoint);
                    actions.push(ControlAction::Suspend);
                }
            }
            actions
        } else {
            Vec::new()
        },
    };
    // Unknown old executions are never adopted. They must expire before a new
    // incarnation starts work; known terminal outbox records were delivered above.
    loop {
        match client.register(&registration).await {
            Ok(_) => break,
            Err(error) if outbox::conflict(&error) || outbox::retryable(&error) => {
                eprintln!(
                    "worker registration waiting for controller/old lease fencing: {error:#}"
                );
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(error) => return Err(error),
        }
    }
    let (finished_tx, mut finished_rx) = mpsc::channel::<Completion>(capacity.slots as usize);
    let (acknowledgements_tx, mut acknowledgements_rx) =
        mpsc::channel::<ControlAcknowledgement>(capacity.slots as usize);
    let (native_done_tx, mut native_done_rx) =
        mpsc::channel::<NativeDoneReceipt>(capacity.slots as usize);
    let mut active = BTreeMap::<String, Active>::new();
    // Independent of active leases: expiry does not discard delivery ownership.
    let mut terminal = BTreeMap::<String, TerminalDelivery>::new();
    let mut deliveries = JoinSet::<DeliveryResult>::new();
    let mut in_flight = BTreeSet::<String>::new();
    let (memory_ready_tx, mut memory_ready_rx) =
        mpsc::channel::<(LeaseKey, pvisor::RunControlHandle)>(capacity.slots as usize);
    let memory_targets = if config.memory_sampling.enabled {
        Some(memory::start(
            client.clone(),
            &config.memory_sampling,
            &registration,
        )?)
    } else {
        None
    };
    let cpu_targets = if config.cpu_sampling.enabled {
        Some(cpu::start(
            client.clone(),
            &config.cpu_sampling,
            &registration,
        )?)
    } else {
        None
    };
    let mut tick = tokio::time::interval(Duration::from_millis(args.poll_ms));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut watchdog = tokio::time::interval(Duration::from_millis(50));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let shutdown = async {
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    };
    tokio::pin!(shutdown);
    let mut stopping = false;
    let mut admission_lease_limit = None;
    let mut last_probe_error = None;
    eprintln!("pVisor worker {} registered ({:?})", args.id, args.backend);
    loop {
        tokio::select! {
            _ = &mut shutdown, if !stopping => { stopping = true; for entry in active.values() { entry.stop.send_replace(true); } },
            Some(receipt) = native_done_rx.recv() => {
                if let Some(entry) = active.get_mut(&receipt.key.task_id) && entry.key == receipt.key && receipt.reserved.fits(entry.resources) {
                    entry.resources = receipt.reserved;
                    entry.memory_controls = None;
                    entry.acknowledgement = None;
                }
                publish_memory_targets(&active, &memory_targets);
                publish_memory_targets(&active, &cpu_targets);
            },
            Some(completion) = finished_rx.recv() => {
                if let Some(entry) = active.get_mut(&completion.key.task_id) && entry.key == completion.key {
                    terminal.insert(outbox::key_name(&completion.key), TerminalDelivery {
                        completion: completion.clone(), retain_bundle: entry.retain_bundle, retention: entry.retention.clone(), durable: false, retry_after: Instant::now(),
                    });
                    entry.native_terminal = true;
                    entry.memory_controls = None;
                }
                publish_memory_targets(&active, &memory_targets);
                publish_memory_targets(&active, &cpu_targets);
            },
            Some(joined) = deliveries.join_next(), if !deliveries.is_empty() => {
                let event = joined.context("worker delivery task failed")?;
                in_flight.remove(&event.delivery.id());
                let key = event.delivery.key().clone();
                if event.storage_error {
                    // Do not admit or abandon attempts after an uncertain durable write.
                    stopping = true;
                    for entry in active.values() { entry.stop.send_replace(true); }
                }
                match event.delivery {
                    Delivery::Complete { .. } => {
                        if let Some(pending) = terminal.get_mut(&outbox::key_name(&key)) {
                            pending.durable |= event.durable;
                            if event.result.is_err() {
                                pending.retry_after = Instant::now() + Duration::from_millis(250);
                            }
                        }
                        match event.result {
                            Ok(()) => {
                                terminal.remove(&outbox::key_name(&key));
                                if active.get(&key.task_id).is_some_and(|entry| entry.key == key) {
                                    active.remove(&key.task_id);
                                }
                            }
                            Err(error) => {
                                eprintln!("terminal delivery remains pending for {}: {error:#}", key.task_id);
                                if !event.storage_error && !outbox::retryable(&error) {
                                    stopping = true;
                                    for entry in active.values() { entry.stop.send_replace(true); }
                                }
                            }
                        }
                    }
                    Delivery::Decline(rejection) => {
                        if let Some(entry) = active.get_mut(&key.task_id) && entry.key == key {
                            match event.result {
                                Ok(()) => { active.remove(&key.task_id); }
                                Err(error) if outbox::conflict(&error) => {
                                    // Cancellation won the decline race: no native run began.
                                    let completion = Completion { key: rejection.key, result: None,
                                        error: Some("node admission rejected before execution".into()),
                                        artifacts: None, artifact_error: None };
                                    terminal.insert(outbox::key_name(&key), TerminalDelivery {
                                        completion: completion.clone(), retain_bundle: entry.retain_bundle, retention: entry.retention.clone(), durable: false, retry_after: Instant::now(),
                                    });
                                    entry.rejection = None;
                                    entry.native_terminal = true;
                                }
                                Err(error) => {
                                    eprintln!("admission rejection delivery failed for {}: {error:#}", key.task_id);
                                    if Instant::now() >= entry.deadline { active.remove(&key.task_id); }
                                }
                            }
                        }
                    }
                    Delivery::Acknowledge(acknowledgement) => {
                        if let Some(entry) = active.get_mut(&key.task_id)
                            && entry.key == key && entry.control_revision == acknowledgement.command.revision {
                            match event.result {
                                Ok(()) => { entry.acknowledgement = None; }
                                Err(error) if outbox::conflict(&error) => {
                                    entry.acknowledgement = None;
                                    entry.stop.send_replace(true);
                                }
                                Err(error) => eprintln!("control acknowledgement delivery failed for {}: {error:#}", key.task_id),
                            }
                        }
                    }
                }
                publish_memory_targets(&active, &memory_targets);
                publish_memory_targets(&active, &cpu_targets);
            },
            Some((key, controls)) = memory_ready_rx.recv() => {
                if let Some(entry) = active.get_mut(&key.task_id) && entry.key == key && !entry.native_terminal && entry.resources.slots > 0 {
                    entry.memory_controls = Some(controls);
                    publish_memory_targets(&active, &memory_targets);
                    publish_memory_targets(&active, &cpu_targets);
                }
            },
            Some(acknowledgement) = acknowledgements_rx.recv() => {
                if let Some(entry) = active.get_mut(&acknowledgement.command.key.task_id)
                    && entry.key == acknowledgement.command.key
                    && entry.control_revision == acknowledgement.command.revision
                    && entry.resources.slots > 0 {
                    entry.resources = acknowledgement.outcome.reservation(entry.full_resources, entry.resources);
                    entry.acknowledgement = Some(acknowledgement);
                }
            },
            _ = watchdog.tick() => {
                expire_active(&mut active, &terminal)
            },
            _ = tick.tick() => {
                let used = active.values().try_fold(Resources::default(), |r, entry| r.checked_add(entry.resources)).context("worker reservation overflow")?;
                let report = node_report(&config.admission, capacity, used, &samples.borrow(), admission_lease_limit)?;
                if report.error != last_probe_error {
                    if let Some(error) = &report.error { eprintln!("node admission blocked: {error}"); }
                    last_probe_error = report.error.clone();
                }
                let request = PollRequest { worker_id: registration.id.clone(), incarnation: registration.incarnation.clone(),
                    active: active.values().filter(|a| a.rejection.is_none()).map(|a| a.key.clone()).collect(), available: report.available,
                    max_assignments: if stopping || terminal.len() >= MAX_DELIVERIES { 0 } else { capacity.slots.min(64).min((capacity.slots as usize + MAX_ARTIFACT_DELIVERIES).saturating_sub(active.len()) as u32) }, admission: Some(report) };
                let began = Instant::now();
                // The lease watchdog must remain live while HTTP waits. This
                // branch awaits only via a nested select that observes expiry.
                let poll = client.poll(&request);
                tokio::pin!(poll);
                let response = loop {
                    tokio::select! {
                        response = &mut poll => break response,
                        _ = watchdog.tick() => { expire_active(&mut active, &terminal) },
                        _ = &mut shutdown, if !stopping => { stopping = true; for entry in active.values() { entry.stop.send_replace(true); } },
                    }
                };
                match response {
                    Ok(response) => {
                        ensure!(response.version == CLUSTER_VERSION, "unsupported controller protocol");
                        let duration = Duration::from_millis(response.lease_duration_ms);
                        ensure!(args.poll_ms * 3 < response.lease_duration_ms, "poll interval must be below one third of lease duration");
                        admission_lease_limit = Some(response.lease_duration_ms);
                        for key in response.renewed {
                            if let Some(entry) = active.get_mut(&key.task_id) && entry.key == key && Instant::now() < entry.deadline { entry.deadline = began + duration; entry.lease_clock.send_replace(entry.deadline); }
                        }
                        for key in response.stop { if let Some(entry) = active.get(&key.task_id) && entry.key == key { entry.stop.send_replace(true); } }
                        for command in response.controls {
                            if let Some(entry) = active.get_mut(&command.key.task_id) && entry.key == command.key && entry.resources.slots > 0 {
                                if command.revision < entry.control_revision { continue; }
                                if command.revision > entry.control_revision {
                                    entry.control_revision = command.revision;
                                    entry.acknowledgement = None;
                                }
                                if command.request.action == ControlAction::Resume {
                                    entry.resources = entry.full_resources;
                                } else {
                                    let _ = entry.commands.try_send(command);
                                    continue;
                                }
                                // Re-sample admission after HTTP, including all
                                // new controller charges, before native resume.
                                let used = active.values().try_fold(Resources::default(), |r, entry| r.checked_add(entry.resources)).context("worker reservation overflow")?;
                                let report = node_report(&config.admission, capacity, used, &samples.borrow(), admission_lease_limit)?;
                                if !stopping && Instant::now() < began + duration && resume_allowed(&report, used) {
                                    let _ = active.get(&command.key.task_id).unwrap().commands.try_send(command);
                                }
                            }
                        }
                        let used = active.values().try_fold(Resources::default(), |r, entry| r.checked_add(entry.resources)).context("worker reservation overflow")?;
                        let final_report = node_report(&config.admission, capacity, used, &samples.borrow(), admission_lease_limit)?;
                        let mut available = final_report.available;
                        for assignment in response.assignments {
                            let id = assignment.spec.id.clone();
                            if active.contains_key(&id) { continue; }
                            let resources = assignment.spec.resources;
                            let retain_bundle = assignment.spec.requires_artifacts();
                            let retention = assignment.spec.retain_artifacts.clone();
                            let key = assignment.lease.key.clone();
                            let storage = args.state.join("tasks").join(format!("{}-{}", id, key.generation));
                            std::fs::create_dir_all(&storage)?;
                            persist(&storage.join("assignment.json"), &assignment)?;
                            let (stop_tx, stop_rx) = watch::channel(stopping || Instant::now() >= began + duration);
                            let (lease_tx, lease_rx) = watch::channel(began + duration);
                            let (commands_tx, commands_rx) = mpsc::channel(1);
                            let rejection = if stopping || Instant::now() >= began + duration || !resources.fits(available) {
                                let rejection = AdmissionRejection { key: key.clone(), reason: "node final admission denied before execution".into() };
                                persist(&storage.join("admission-rejection.json"), &rejection)?;
                                Some(rejection)
                            } else {
                                available = available.checked_sub(resources).unwrap();
                                let runtime = attempt_runtime.clone();
                                let tx = finished_tx.clone();
                                let ack_tx = acknowledgements_tx.clone();
                                let publisher = client.clone();
                                let pending = outbox.clone();
                                let native_done = native_done_tx.clone();
                                let memory_ready = (memory_targets.is_some() || cpu_targets.is_some()).then(|| memory_ready_tx.clone());
                                tokio::spawn(async move { let completion = execute(runtime, assignment, AttemptChannels { stop: stop_rx, lease_clock: lease_rx, commands: commands_rx, acknowledgements: ack_tx, memory_ready, native_done }, storage, publisher, pending).await; let _ = tx.send(completion).await; });
                                None
                            };
                            active.insert(id, Active { key, resources, full_resources: resources, deadline: began + duration, lease_clock: lease_tx, stop: stop_tx, native_terminal: false, commands: commands_tx, acknowledgement: None, control_revision: 0, rejection, retain_bundle, retention, memory_controls: None });
                        }
                    }
                    Err(error) => eprintln!("worker poll failed: {error:#}"),
                }
                // Bounded detached I/O; none of these requests can delay the
                // next poll/renewal. Exact keys/revisions fence late responses.
                for entry in active.values() {
                    if let Some(rejection) = &entry.rejection {
                        queue_delivery(Delivery::Decline(rejection.clone()), &mut deliveries, &mut in_flight, &client, &outbox);
                    }
                    if let Some(acknowledgement) = &entry.acknowledgement {
                        queue_delivery(Delivery::Acknowledge(acknowledgement.clone()), &mut deliveries, &mut in_flight, &client, &outbox);
                    }
                }
                for pending in terminal.values().filter(|pending| Instant::now() >= pending.retry_after) {
                    queue_delivery(Delivery::Complete {
                        completion: Box::new(pending.completion.clone()), retain_bundle: pending.retain_bundle, retention: pending.retention.clone(),
                    }, &mut deliveries, &mut in_flight, &client, &outbox);
                }
            }
        }
        if stopping && active.is_empty() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    fn terminal_fixture() -> Completion {
        Completion {
            key: LeaseKey {
                task_id: "task".into(),
                generation: 1,
                worker_id: "worker".into(),
                incarnation: "incarnation".into(),
            },
            result: None,
            error: Some("native stopped".into()),
            artifacts: None,
            artifact_error: None,
        }
    }
    fn terminal_active(completion: &Completion) -> Active {
        let (stop, _) = watch::channel(false);
        let deadline = Instant::now();
        let (lease_clock, _) = watch::channel(deadline);
        let (commands, _) = mpsc::channel(1);
        Active {
            key: completion.key.clone(),
            resources: Resources::default(),
            full_resources: Resources::default(),
            deadline,
            lease_clock,
            stop,
            native_terminal: true,
            commands,
            acknowledgement: None,
            control_revision: 0,
            rejection: None,
            retain_bundle: false,
            retention: None,
            memory_controls: None,
        }
    }
    #[tokio::test]
    async fn terminal_delivery_outlives_active_expiry_only_after_durable_save() {
        let completion = terminal_fixture();
        let key = outbox::key_name(&completion.key);
        let mut active = BTreeMap::from([("task".into(), terminal_active(&completion))]);
        let mut terminal = BTreeMap::from([(
            key.clone(),
            TerminalDelivery {
                completion,
                retain_bundle: false,
                retention: None,
                durable: false,
                retry_after: Instant::now(),
            },
        )]);
        expire_active(&mut active, &terminal);
        assert_eq!(
            active.len(),
            1,
            "uncertain persistence must retain ownership"
        );
        terminal.get_mut(&key).unwrap().durable = true;
        expire_active(&mut active, &terminal);
        assert!(active.is_empty());
        assert_eq!(
            terminal.len(),
            1,
            "expiry must not discard retry responsibility"
        );
    }
    #[tokio::test]
    async fn slow_terminal_http_does_not_block_renewal_and_can_retry_after_expiry() {
        use axum::{Json, Router, http::StatusCode, routing::post};
        use tokio::sync::Notify;
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let app = Router::new()
            .route(
                "/v1/workers/complete",
                post({
                    let entered = entered.clone();
                    let release = release.clone();
                    let failed = failed.clone();
                    move || {
                        let entered = entered.clone();
                        let release = release.clone();
                        let failed = failed.clone();
                        async move {
                            if !failed.swap(true, std::sync::atomic::Ordering::SeqCst) {
                                entered.notify_one();
                                release.notified().await;
                                (
                                    StatusCode::SERVICE_UNAVAILABLE,
                                    Json(serde_json::json!({"error":"retry"})),
                                )
                            } else {
                                (
                                    StatusCode::CONFLICT,
                                    Json(serde_json::json!({"error":"fenced"})),
                                )
                            }
                        }
                    }
                }),
            )
            .route(
                "/v1/workers/poll",
                post(|| async {
                    Json(PollResponse {
                        version: CLUSTER_VERSION,
                        lease_duration_ms: 30_000,
                        assignments: vec![],
                        renewed: vec![],
                        stop: vec![],
                        controls: vec![],
                    })
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = Client::new(&url, "test".into()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let outbox = Arc::new(outbox::Outbox::open(directory.path(), "worker", &url).unwrap());
        let completion = terminal_fixture();
        let mut jobs = JoinSet::new();
        let mut in_flight = BTreeSet::new();
        let delivery = Delivery::Complete {
            completion: Box::new(completion.clone()),
            retain_bundle: false,
            retention: None,
        };
        queue_delivery(
            delivery.clone(),
            &mut jobs,
            &mut in_flight,
            &client,
            &outbox,
        );
        queue_delivery(
            delivery.clone(),
            &mut jobs,
            &mut in_flight,
            &client,
            &outbox,
        );
        assert_eq!(jobs.len(), 1, "one in-flight delivery per exact key");
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            client.poll(&PollRequest {
                worker_id: "worker".into(),
                incarnation: "incarnation".into(),
                active: vec![completion.key.clone()],
                available: Resources::default(),
                max_assignments: 0,
                admission: None,
            }),
        )
        .await
        .unwrap()
        .unwrap();
        release.notify_one();
        let event = jobs.join_next().await.unwrap().unwrap();
        assert!(event.result.is_err() && event.durable && !event.storage_error);
        in_flight.remove(&event.delivery.id());
        let mut active = BTreeMap::from([("task".into(), terminal_active(&completion))]);
        let terminal = BTreeMap::from([(
            outbox::key_name(&completion.key),
            TerminalDelivery {
                completion,
                retain_bundle: false,
                retention: None,
                durable: true,
                retry_after: Instant::now(),
            },
        )]);
        expire_active(&mut active, &terminal);
        assert!(active.is_empty());
        queue_delivery(delivery, &mut jobs, &mut in_flight, &client, &outbox);
        assert!(jobs.join_next().await.unwrap().unwrap().result.is_ok());
        server.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn final_node_admission_uses_the_stricter_lease_sample_age_limit() {
        let policy = AdmissionPolicy {
            mode: AdmissionMode::LinuxPressure,
            memory_reserve_bytes: 0,
            ..Default::default()
        };
        let capacity = Resources {
            slots: 1,
            memory_bytes: 1024,
            cpu_millis: 250,
        };
        let sample = NodeSample {
            started: Instant::now(),
            measurements: Ok(NodeMeasurements {
                local_cpu_quota_millis: None,
                system_memory_available_bytes: 4096,
                cgroup_memory_headroom_bytes: None,
                cpu_limit_millis: 250,
                cpu_some_avg10_bps: 0,
                memory_full_avg10_bps: 0,
            }),
        };
        assert_eq!(
            node_report(&policy, capacity, Resources::default(), &sample, Some(500))
                .unwrap()
                .available,
            capacity
        );
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(
            node_report(&policy, capacity, Resources::default(), &sample, None)
                .unwrap()
                .available,
            capacity
        );
        let report =
            node_report(&policy, capacity, Resources::default(), &sample, Some(500)).unwrap();
        assert_eq!(report.available, Resources::default());
        assert!(report.blocked.contains(&AdmissionBlock::StaleSample));
        assert!(!resume_allowed(&report, Resources::default()));
    }
    #[test]
    fn resume_uses_precharged_budget_but_requires_fresh_healthy_node_observations() {
        let policy = AdmissionPolicy {
            mode: AdmissionMode::LinuxPressure,
            memory_reserve_bytes: 0,
            ..Default::default()
        };
        let full = Resources {
            slots: 1,
            memory_bytes: 1024,
            cpu_millis: 250,
        };
        let measurements = NodeMeasurements {
            local_cpu_quota_millis: None,
            system_memory_available_bytes: 4096,
            cgroup_memory_headroom_bytes: None,
            cpu_limit_millis: 250,
            cpu_some_avg10_bps: 0,
            memory_full_avg10_bps: 0,
        };
        let overcommit = AdmissionPolicy {
            cpu_overcommit_bps: 20_000,
            ..policy.clone()
        };
        let overcommitted_full = Resources {
            cpu_millis: 500,
            ..full
        };
        let mut local = measurements.clone();
        local.local_cpu_quota_millis = Some(250);
        let doubled = overcommit
            .report(overcommitted_full, overcommitted_full, 0, Ok(local.clone()))
            .unwrap();
        assert_eq!(doubled.available.cpu_millis, 0);
        assert!(resume_allowed(&doubled, overcommitted_full));
        local.cpu_some_avg10_bps = overcommit.cpu_some_avg10_limit_bps;
        let pressured = overcommit
            .report(overcommitted_full, overcommitted_full, 0, Ok(local))
            .unwrap();
        assert!(!resume_allowed(&pressured, overcommitted_full));
        let report = policy
            .report(full, full, 0, Ok(measurements.clone()))
            .unwrap();
        assert_eq!(report.available.cpu_millis, 0);
        assert!(resume_allowed(&report, full)); // quota already reserved, not another charge
        let mut pressure = measurements.clone();
        pressure.cpu_some_avg10_bps = policy.cpu_some_avg10_limit_bps;
        assert!(!resume_allowed(
            &policy.report(full, full, 0, Ok(pressure)).unwrap(),
            full
        ));
        assert!(!resume_allowed(
            &policy
                .report(
                    full,
                    full,
                    policy.max_sample_age_ms,
                    Ok(measurements.clone())
                )
                .unwrap(),
            full
        ));
        assert!(!resume_allowed(
            &policy
                .report(full, full, 0, Err("unavailable".into()))
                .unwrap(),
            full
        ));
        let mut reduced_quota = measurements;
        reduced_quota.cpu_limit_millis = 249;
        assert!(!resume_allowed(
            &policy.report(full, full, 0, Ok(reduced_quota)).unwrap(),
            full
        ));
    }
}
