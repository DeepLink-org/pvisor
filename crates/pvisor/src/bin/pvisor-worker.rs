//! Per-host cluster executor. Task bodies never inherit controller credentials.
use anyhow::{Context, ensure};
use clap::{Parser, ValueEnum};
use fs2::FileExt;
use pvisor::{ContainerExecutor, PVisor, ProcessExecutor, RunExecutor, VmExecutor};
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
    /// Host-owned TOML profile supplies rootfs, toolkit layers, network and cache settings.
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
    deadline: Instant,
    lease_clock: watch::Sender<Instant>,
    stop: watch::Sender<bool>,
    completion: Option<Completion>,
}

fn executor(
    args: &Args,
    config: &pvisor::RunConfig,
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
            Arc::new(VmExecutor::new(settings)?)
        }
    })
}

fn runtime(
    args: &Args,
    config: &pvisor::RunConfig,
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
    if let Some(overlay) = &config.overlayfs {
        ensure!(
            overlay.commit == pvisor::OverlayFsCommit::Manual,
            "cluster overlays must preserve staged outputs"
        );
        ensure!(
            !overlay.mount.is_empty(),
            "cluster overlayfs profile needs explicit mounts"
        );
        // Core filesystem rules remain in RunSpec. Host-owned lower layers are
        // shared; writable upper and merged mount are private to each lease.
        builder = builder.overlay(pvisor::OverlayHint {
            lower_dirs: overlay.mount.iter().map(|m| m.source.clone()).collect(),
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
    storage: PathBuf,
) -> Completion {
    let result: anyhow::Result<pvisor_core::RunResult> = async {
        let runtime = runtime?;
        let mut spec = assignment.spec.run;
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
        if *stop.borrow() || Instant::now() >= *lease_clock.borrow() {
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
        tokio::select! {
            result = &mut wait => Ok(result?),
            _ = stop.changed() => { cancellation.cancel(); Ok(wait.await?) },
            _ = expired => { cancellation.cancel(); Ok(wait.await?) }
        }
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
fn persist(path: &Path, value: &impl serde::Serialize) -> anyhow::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
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
    let config = match &args.config {
        Some(path) => pvisor::RunConfig::from_file(path)?,
        None => pvisor::RunConfig::default(),
    };
    ensure!(
        config.gateway.mode != pvisor::GatewayMode::Capture,
        "cluster worker Gateway capture profile is not yet connected"
    );
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
    };
    let client = Client::new(&args.url, args.token.clone())?;
    client.register(&registration).await?;
    let (finished_tx, mut finished_rx) = mpsc::channel::<Completion>(capacity.slots as usize);
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
    eprintln!("pVisor worker {} registered ({:?})", args.id, args.backend);
    loop {
        tokio::select! {
            _ = &mut shutdown, if !stopping => { stopping = true; for entry in active.values() { entry.stop.send_replace(true); } },
            Some(completion) = finished_rx.recv() => {
                if let Some(entry) = active.get_mut(&completion.key.task_id) { entry.completion = Some(completion); }
            },
            _ = watchdog.tick() => {
                for entry in active.values() { if Instant::now() >= entry.deadline { entry.stop.send_replace(true); } }
            },
            _ = tick.tick() => {
                let used = active.values().try_fold(Resources::default(), |r, entry| r.checked_add(entry.resources)).context("worker reservation overflow")?;
                let request = PollRequest { worker_id: registration.id.clone(), incarnation: registration.incarnation.clone(),
                    active: active.values().map(|a| a.key.clone()).collect(), available: capacity.checked_sub(used).context("worker over capacity")?,
                    max_assignments: if stopping { 0 } else { capacity.slots.min(64) } };
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
                        let duration = Duration::from_millis(response.lease_duration_ms);
                        ensure!(args.poll_ms * 3 < response.lease_duration_ms, "poll interval must be below one third of lease duration");
                        for key in response.renewed {
                            if let Some(entry) = active.get_mut(&key.task_id) { entry.deadline = began + duration; entry.lease_clock.send_replace(entry.deadline); }
                        }
                        for key in response.stop { if let Some(entry) = active.get(&key.task_id) { entry.stop.send_replace(true); } }
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
                            let runtime = runtime(&args, &config, &assignment, &storage);
                            let tx = finished_tx.clone();
                            tokio::spawn(async move { let completion = execute(runtime, assignment, stop_rx, lease_rx, storage).await; let _ = tx.send(completion).await; });
                            active.insert(id, Active { key, resources, deadline: began + duration, lease_clock: lease_tx, stop: stop_tx, completion: None });
                        }
                    }
                    Err(error) => eprintln!("worker poll failed: {error:#}"),
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
