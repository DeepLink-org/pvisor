//! Per-host cluster executor. Task bodies never inherit controller credentials.
use anyhow::{Context, ensure};
use clap::{Parser, ValueEnum};
use fs2::FileExt;
use pvisor::{ContainerExecutor, PVisor, ProcessExecutor, RunExecutor, VmExecutor};
use pvisor_cluster::admission::{AdmissionPolicy, sample_linux};
use pvisor_cluster::{client::Client, *};
use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation, StdioMode};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Backend {
    Host,
    Rootless,
    Container,
    Vm,
}

/// Only fields actually connected to the cluster worker are accepted.
#[derive(Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
struct WorkerProfile {
    vm: pvisor::VmSettings,
    container: pvisor::ContainerSettings,
    overlaynet: pvisor::OverlayNetSettings,
    /// Shared read-only inputs, ordered bottom to top. Upper storage is private.
    lower_layers: Vec<PathBuf>,
    admission: AdmissionPolicy,
}

#[derive(Parser)]
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
    completion: Option<Completion>,
    commands: mpsc::Sender<ControlCommand>,
    acknowledgement: Option<ControlAcknowledgement>,
    control_revision: u64,
    rejection: Option<AdmissionRejection>,
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
) -> anyhow::Result<AdmissionReport> {
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
            && report
                .measurements
                .as_ref()
                .is_some_and(|m| used.cpu_millis <= m.cpu_limit_millis))
}

#[cfg(test)]
mod admission_tests {
    use super::*;
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
            system_memory_available_bytes: 4096,
            cgroup_memory_headroom_bytes: None,
            cpu_limit_millis: 250,
            cpu_some_avg10_bps: 0,
            memory_full_avg10_bps: 0,
        };
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
        Backend::Vm => {
            let mut settings = config.vm.clone();
            // Reservation equals configured guest address space; dynamic sizing
            // avoids reserving the 2 GiB default for every small agent.
            ensure!(
                resources.memory_bytes.is_multiple_of(1024 * 1024),
                "VM memory budget must be a whole MiB"
            );
            settings.memory_mib = u32::try_from(resources.memory_bytes / (1024 * 1024))?;
            settings.cpus = u16::try_from(resources.cpu_millis.div_ceil(1000))?;
            settings.ram_backing = None; // every Attempt owns a new backing
            settings.rootfs_immutable = true;
            Arc::new(VmExecutor::new(settings)?)
        }
    })
}

fn runtime(
    args: &Args,
    config: &WorkerProfile,
    assignment: &Assignment,
    storage: &Path,
) -> anyhow::Result<PVisor> {
    let executor = executor(args, config, assignment.spec.resources)?;
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
    if !config.lower_layers.is_empty() {
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

async fn execute(
    runtime: anyhow::Result<PVisor>,
    assignment: Assignment,
    mut stop: watch::Receiver<bool>,
    mut lease_clock: watch::Receiver<Instant>,
    mut commands: mpsc::Receiver<ControlCommand>,
    acknowledgements: mpsc::Sender<ControlAcknowledgement>,
    storage: PathBuf,
) -> Completion {
    let result: anyhow::Result<pvisor_core::RunResult> = async {
        let runtime = runtime?;
        let mut spec = assignment.spec.run;
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
                    let operation = async {
                        controls.clone().wait_ready().await?;
                        controls.control(action.operation()).await
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
        Ok(result)
    }
    .await;
    let completion = match result {
        Ok(result) => Completion {
            key: assignment.lease.key,
            result: Some(result),
            error: None,
        },
        Err(error) => Completion {
            key: assignment.lease.key,
            result: None,
            error: Some(format!("{error:#}")),
        },
    };
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
fn persist(path: &Path, value: &impl serde::Serialize) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.sync_all()?;
    std::fs::File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // VM/rootless child launchers re-enter this binary before parsing the worker CLI.
    if pvisor::run_krun_internal_if_requested()? || pvisor::sandbox::run_internal_if_requested()? {
        return Ok(());
    }
    let args = Args::parse();
    ensure!(
        args.poll_ms > 0 && args.poll_ms <= 10_000,
        "poll interval must be 1..10000 ms"
    );
    std::fs::create_dir_all(&args.state)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(args.state.join("owner.lock"))?;
    lock.try_lock_exclusive()
        .context("worker state already owned")?;
    let config: WorkerProfile = match &args.config {
        Some(path) => toml::from_str(&std::fs::read_to_string(path)?)?,
        None => WorkerProfile::default(),
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
    let registration = WorkerRegistration {
        version: CLUSTER_VERSION,
        id: args.id.clone(),
        incarnation: uuid::Uuid::new_v4().to_string(),
        capacity,
        execution: vec![class],
        labels: args.label.iter().cloned().collect(),
        cache_keys: args.cache_key.clone(),
        vm_control_protocol: matches!(args.backend, Backend::Vm).then_some(CLUSTER_VERSION),
        vm_control_actions: if matches!(args.backend, Backend::Vm) {
            let mut actions = vec![ControlAction::Pause, ControlAction::Resume];
            let uses_pool = config.vm.memory_pool.is_some()
                || (cfg!(all(target_os = "macos", target_arch = "aarch64"))
                    && std::env::var_os("PVISOR_EXPERIMENTAL_MEMORY_POOL").is_some());
            if !uses_pool {
                actions.push(ControlAction::Offload);
            }
            actions
        } else {
            Vec::new()
        },
    };
    let client = Client::new(&args.url, args.token.clone())?;
    client.register(&registration).await?;
    let (finished_tx, mut finished_rx) = mpsc::channel::<Completion>(capacity.slots as usize);
    let (acknowledgements_tx, mut acknowledgements_rx) =
        mpsc::channel::<ControlAcknowledgement>(capacity.slots as usize);
    let mut active = BTreeMap::<String, Active>::new();
    let mut tick = tokio::time::interval(Duration::from_millis(args.poll_ms));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut watchdog = tokio::time::interval(Duration::from_millis(50));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let shutdown = async {
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    };
    tokio::pin!(shutdown);
    let mut stopping = false;
    let mut last_probe_error = None;
    eprintln!("pVisor worker {} registered ({:?})", args.id, args.backend);
    loop {
        tokio::select! {
            _ = &mut shutdown, if !stopping => { stopping = true; for entry in active.values() { entry.stop.send_replace(true); } },
            Some(completion) = finished_rx.recv() => {
                if let Some(entry) = active.get_mut(&completion.key.task_id) { entry.completion = Some(completion); }
            },
            Some(acknowledgement) = acknowledgements_rx.recv() => {
                if let Some(entry) = active.get_mut(&acknowledgement.command.key.task_id)
                    && entry.key == acknowledgement.command.key
                    && entry.control_revision == acknowledgement.command.revision {
                    entry.resources = acknowledgement.outcome.reservation(entry.full_resources, entry.resources);
                    entry.acknowledgement = Some(acknowledgement);
                }
            },
            _ = watchdog.tick() => {
                for entry in active.values() { if Instant::now() >= entry.deadline { entry.stop.send_replace(true); } }
            },
            _ = tick.tick() => {
                let used = active.values().try_fold(Resources::default(), |r, entry| r.checked_add(entry.resources)).context("worker reservation overflow")?;
                let report = node_report(&config.admission, capacity, used, &samples.borrow())?;
                if report.error != last_probe_error {
                    if let Some(error) = &report.error { eprintln!("node admission blocked: {error}"); }
                    last_probe_error = report.error.clone();
                }
                let request = PollRequest { worker_id: registration.id.clone(), incarnation: registration.incarnation.clone(),
                    active: active.values().filter(|a| a.rejection.is_none()).map(|a| a.key.clone()).collect(), available: report.available,
                    max_assignments: if stopping { 0 } else { capacity.slots.min(64) }, admission: Some(report) };
                let began = Instant::now();
                // The lease watchdog must remain live while HTTP waits. This
                // branch awaits only via a nested select that observes expiry.
                let poll = client.poll(&request);
                tokio::pin!(poll);
                let response = loop {
                    tokio::select! {
                        response = &mut poll => break response,
                        _ = watchdog.tick() => { for entry in active.values() { if Instant::now() >= entry.deadline { entry.stop.send_replace(true); } } },
                        _ = &mut shutdown, if !stopping => { stopping = true; for entry in active.values() { entry.stop.send_replace(true); } },
                    }
                };
                match response {
                    Ok(response) => {
                        ensure!(response.version == CLUSTER_VERSION, "unsupported controller protocol");
                        let duration = Duration::from_millis(response.lease_duration_ms);
                        ensure!(args.poll_ms * 3 < response.lease_duration_ms, "poll interval must be below one third of lease duration");
                        for key in response.renewed {
                            if let Some(entry) = active.get_mut(&key.task_id) { entry.deadline = began + duration; entry.lease_clock.send_replace(entry.deadline); }
                        }
                        for key in response.stop { if let Some(entry) = active.get(&key.task_id) { entry.stop.send_replace(true); } }
                        for command in response.controls {
                            if let Some(entry) = active.get_mut(&command.key.task_id) && entry.key == command.key {
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
                                let report = node_report(&config.admission, capacity, used, &samples.borrow())?;
                                if !stopping && Instant::now() < began + duration && resume_allowed(&report, used) {
                                    let _ = active.get(&command.key.task_id).unwrap().commands.try_send(command);
                                }
                            }
                        }
                        let used = active.values().try_fold(Resources::default(), |r, entry| r.checked_add(entry.resources)).context("worker reservation overflow")?;
                        let final_report = node_report(&config.admission, capacity, used, &samples.borrow())?;
                        let mut available = final_report.available;
                        for assignment in response.assignments {
                            let id = assignment.spec.id.clone();
                            if active.contains_key(&id) { continue; }
                            let resources = assignment.spec.resources;
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
                                let runtime = runtime(&args, &config, &assignment, &storage);
                                let tx = finished_tx.clone();
                                let ack_tx = acknowledgements_tx.clone();
                                tokio::spawn(async move { let completion = execute(runtime, assignment, stop_rx, lease_rx, commands_rx, ack_tx, storage).await; let _ = tx.send(completion).await; });
                                None
                            };
                            active.insert(id, Active { key, resources, full_resources: resources, deadline: began + duration, lease_clock: lease_tx, stop: stop_tx, completion: None, commands: commands_tx, acknowledgement: None, control_revision: 0, rejection });
                        }
                    }
                    Err(error) => eprintln!("worker poll failed: {error:#}"),
                }
                // Declines are only created before runtime.run and are omitted
                // from the active-key acknowledgement. They safely requeue the
                // same task with a new generation; uncertain executions never do.
                let rejections: Vec<_> = active.values().filter_map(|a| a.rejection.clone()).collect();
                for rejection in rejections {
                    let id = rejection.key.task_id.clone();
                    let delivery = client.decline(&rejection);
                    tokio::pin!(delivery);
                    let response = loop {
                        tokio::select! {
                            response = &mut delivery => break response,
                            _ = watchdog.tick() => { for entry in active.values() { if Instant::now() >= entry.deadline { entry.stop.send_replace(true); } } },
                            _ = &mut shutdown, if !stopping => { stopping = true; for entry in active.values() { entry.stop.send_replace(true); } },
                        }
                    };
                    match response {
                        Ok(_) => { active.remove(&id); },
                        Err(error) if error.downcast_ref::<reqwest::Error>().and_then(|e| e.status()) == Some(reqwest::StatusCode::CONFLICT) => {
                            if let Some(entry) = active.get_mut(&id) {
                                // Cancellation may have won the decline race.
                                // This completion confirms no native run began.
                                entry.rejection = None;
                                entry.completion = Some(Completion { key: rejection.key.clone(), result: None, error: Some("node admission rejected before execution".into()) });
                                persist(&args.state.join("tasks").join(format!("{}-{}", id, entry.key.generation)).join("completion.json"), entry.completion.as_ref().unwrap())?;
                            }
                        },
                        Err(error) => {
                            eprintln!("admission rejection delivery failed for {id}: {error:#}");
                            if active.get(&id).is_some_and(|entry| Instant::now() >= entry.deadline) { active.remove(&id); }
                        },
                    }
                }
                let acknowledgements: Vec<_> = active.values().filter_map(|a| a.acknowledgement.clone()).collect();
                for acknowledgement in acknowledgements {
                    let id = acknowledgement.command.key.task_id.clone();
                    let delivery = client.acknowledge_control(&acknowledgement);
                    tokio::pin!(delivery);
                    let response = loop {
                        tokio::select! {
                            response = &mut delivery => break response,
                            _ = watchdog.tick() => { for entry in active.values() { if Instant::now() >= entry.deadline { entry.stop.send_replace(true); } } },
                            _ = &mut shutdown, if !stopping => { stopping = true; for entry in active.values() { entry.stop.send_replace(true); } },
                        }
                    };
                    match response {
                        Ok(_) => { if let Some(entry) = active.get_mut(&id) { entry.acknowledgement = None; } },
                        Err(error) if error.downcast_ref::<reqwest::Error>().and_then(|e| e.status()) == Some(reqwest::StatusCode::CONFLICT) => {
                            if let Some(entry) = active.get_mut(&id) { entry.acknowledgement = None; entry.stop.send_replace(true); }
                        },
                        Err(error) => eprintln!("control acknowledgement delivery failed for {id}: {error:#}"),
                    }
                }
                // Deliver completed evidence independently of task execution.
                // Retain reservations until the controller acknowledges it.
                let completions: Vec<_> = active.values().filter_map(|a| a.completion.clone()).collect();
                for completion in completions {
                    let id = completion.key.task_id.clone();
                    let deadline = active[&id].deadline;
                    let delivery = client.complete(&completion);
                    tokio::pin!(delivery);
                    let response = loop {
                        tokio::select! {
                            response = &mut delivery => break response,
                            _ = watchdog.tick() => { for entry in active.values() { if Instant::now() >= entry.deadline { entry.stop.send_replace(true); } } },
                            _ = &mut shutdown, if !stopping => { stopping = true; for entry in active.values() { entry.stop.send_replace(true); } },
                        }
                    };
                    match response {
                        Ok(_) => { active.remove(&id); },
                        Err(error) if error.downcast_ref::<reqwest::Error>().and_then(|e| e.status()) == Some(reqwest::StatusCode::CONFLICT) => {
                            eprintln!("controller fenced completion for {id}; retained local evidence"); active.remove(&id);
                        },
                        Err(error) => { eprintln!("completion delivery failed for {id}: {error:#}"); if Instant::now() >= deadline { active.remove(&id); } },
                    }
                }
            }
        }
        if stopping && active.is_empty() {
            break;
        }
    }
    Ok(())
}
