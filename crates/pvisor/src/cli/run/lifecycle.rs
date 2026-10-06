//! Workspace branches and native execution restoration for CLI Jobs.
#[cfg(feature = "gateway")]
use super::resolve_proxy;
use super::{
    announce_control_socket, apply_safe_defaults, delegated_shutdown_signal, execute_config,
    paths_overlap, resolve_workspace, select_run_storage,
};
#[cfg(feature = "gateway")]
use crate::GatewayDriverConfig;
use crate::cli::trajectory::JournalRecording;
use crate::config::{
    GatewayMode, OverlayFsCommit, OverlayFsSettings, OverlayNetPolicy, RunConfig, RunExecutorKind,
};
use crate::runtime::job_execution::JobState;
use crate::runtime::{RunLineage, RunRecord, default_run_home, resolve_run};
use crate::{
    NetworkDriverConfig, OverlayHint, PVisor, RunBundle, VmExecutor, restore_logical_checkpoint,
};
use anyhow::Context;
use clap::Args;
use pvisor_core::{RunInvocation, RunState};
use pvisor_overlaynet::{NetworkConfig, NetworkMode};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, Clone, Args, serde::Serialize, serde::Deserialize)]
pub struct ForkArgs {
    /// Source Job id, workspace, run.json, or path inside the source Job.
    source: PathBuf,
    /// Workspace starts a new command; execution continues saved CPU/RAM state.
    #[arg(long, value_enum, default_value = "workspace")]
    state: crate::cli::checkpoint::Kind,
    /// Existing logical checkpoint id; when omitted, snapshot the stopped source Job now.
    #[arg(long, value_name = "ID")]
    checkpoint: Option<String>,
    /// Empty or new directory for the child Job's independent stage.
    #[arg(long)]
    stage: Option<PathBuf>,
    /// Human-readable child Job name.
    #[arg(long)]
    name: Option<String>,
    /// Encoding for a newly captured execution checkpoint only.
    #[arg(long, value_enum)]
    ram_storage: Option<crate::cli::checkpoint::RamStorage>,
    /// Read all captured RAM before starting an execution branch; default is lazy.
    #[arg(long)]
    eager_ram: bool,
    /// Durable idempotency key for an execution branch.
    #[arg(long)]
    request_id: Option<String>,
    #[arg(long, short = 'o', default_value = ".pvisor/capture")]
    output_dir: PathBuf,
    /// Agent command; defaults to the source Job command.
    #[arg(last = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

pub(in crate::cli) async fn fork(args: ForkArgs) -> anyhow::Result<i32> {
    anyhow::ensure!(
        args.state == crate::cli::checkpoint::Kind::Execution
            || args.request_id.is_none() && !args.eager_ram,
        "--request-id is only valid for execution fork"
    );
    anyhow::ensure!(
        args.state == crate::cli::checkpoint::Kind::Execution || args.ram_storage.is_none(),
        "--ram-storage is only valid for execution capture"
    );
    anyhow::ensure!(
        args.state == crate::cli::checkpoint::Kind::Workspace || args.command.is_empty(),
        "execution fork cannot replace the saved command"
    );
    anyhow::ensure!(
        args.checkpoint.is_none() || args.ram_storage.is_none(),
        "--ram-storage cannot change an existing checkpoint"
    );
    let storage = args
        .output_dir
        .canonicalize()
        .unwrap_or(args.output_dir.clone());
    let source = resolve_run(Some(&args.source), &storage)?;
    if args.state == crate::cli::checkpoint::Kind::Execution {
        return fork_execution(args, source).await;
    }
    // Hold ownership from selection through copy and durable source retention.
    // The runner starts only after releasing the source's lease.
    let (source, source_lease) = source.lock_current()?;
    let checkpoint = match args.checkpoint.as_deref() {
        Some(id) => crate::runtime::checkpoint::resolve_checkpoint(&source, id)?,
        None => crate::runtime::checkpoint::create_stopped_checkpoint_locked(&source, None)?,
    };
    anyhow::ensure!(
        checkpoint.run_id == source.run_id,
        "checkpoint {} belongs to Job {}, not {}",
        checkpoint.checkpoint_id,
        checkpoint.run_id,
        source.run_id
    );
    let fork_workspace = source
        .workspace
        .clone()
        .unwrap_or_else(|| checkpoint.target.clone());
    let fork_workspace = resolve_workspace(&fork_workspace)?;
    let run_id = format!("run-{}", uuid::Uuid::new_v4());
    let mut config = RunConfig::default();
    if source
        .executor
        .as_ref()
        .is_some_and(|executor| executor.isolation == pvisor_core::IsolationKind::VirtualMachine)
    {
        config.run.executor = RunExecutorKind::Vm;
        config.vm.rootfs = Some(checkpoint.target.clone());
        config.vm.rootfs_immutable = checkpoint.protect_target;
    }
    config.run.workspace = Some(fork_workspace.clone());
    let (agent, command) = fork_command(&source.agent, &source.command, args.command);
    config.run.agent = agent;
    if let Some(name) = args.name {
        config.run.agent = name;
    }
    config.run.command = command;
    config.overlayfs = Some(OverlayFsSettings {
        durability: pvisor_overlay_core::stage::policy(&checkpoint.preimages_snapshot)?,
        access_policy: checkpoint.access_policy.clone(),
        base: Some(checkpoint.target.clone()),
        target: None,
        merged_dir: None,
        compose: checkpoint
            .lower_dirs
            .iter()
            .filter(|lower| *lower != &checkpoint.target)
            .cloned()
            .collect(),
        stage: args.stage,
        stage_size_bytes: None,
        mount: Vec::new(),
        access: Vec::new(),
        commit: OverlayFsCommit::Manual,
    });
    let stage = match config
        .overlayfs
        .as_ref()
        .and_then(|overlay| overlay.stage.as_ref())
    {
        Some(path) => fork_stage_candidate(path)?,
        None => select_run_storage(&config, &fork_workspace, &run_id)?,
    };
    anyhow::ensure!(
        !stage.exists() || std::fs::read_dir(&stage)?.next().is_none(),
        "child stage must be empty or absent: {}",
        stage.display()
    );
    anyhow::ensure!(
        !paths_overlap(&stage, &source.stage_dir())
            && checkpoint
                .lower_dirs
                .iter()
                .all(|lower| !lower.starts_with(&stage)),
        "child stage must not overlap the source Job or contain its filesystem layers"
    );
    crate::util::create_dir_all_durable(&stage)?;
    let upper = stage.join("upper");
    // Exclusive pin creation arbitrates competing forks targeting one empty
    // directory. A loser must never remove the winner's files during cleanup.
    crate::runtime::checkpoint::pin_checkpoint(&checkpoint, &stage)?;
    if let Err(error) = restore_logical_checkpoint(&checkpoint, &upper, &stage.join("preimages")) {
        let _ = std::fs::remove_dir_all(&stage);
        return Err(error);
    }
    config.overlayfs.as_mut().expect("configured above").stage = Some(stage);
    apply_safe_defaults(&mut config)?;
    drop(source_lease);
    execute_config(
        config,
        run_id,
        false,
        Some(RunLineage {
            parent_run_id: source.run_id,
            checkpoint_id: checkpoint.checkpoint_id,
        }),
    )
    .await
}

/// Resolve existing symlink ancestors before validating a new branch path,
/// without creating directories inside the source Job on rejected requests.
fn fork_stage_candidate(path: &Path) -> anyhow::Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    fn resolve(path: &Path) -> anyhow::Result<PathBuf> {
        if path.try_exists()? {
            return Ok(path.canonicalize()?);
        }
        let name = path.file_name().context("invalid child stage path")?;
        let parent = path.parent().context("child stage has no parent")?;
        Ok(resolve(parent)?.join(name))
    }
    resolve(&path)
}

pub(super) fn fork_command(
    source_agent: &str,
    source_command: &[String],
    command: Vec<String>,
) -> (String, Vec<String>) {
    if command.is_empty() {
        return (source_agent.to_owned(), source_command.to_vec());
    }
    let agent = Path::new(&command[0])
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(source_agent)
        .to_owned();
    (agent, command)
}

pub(in crate::cli) async fn resume_execution(
    source: RunRecord,
    request_id: Option<String>,
    eager_ram: bool,
) -> anyhow::Result<i32> {
    use crate::runtime::job_execution;
    let template = job_execution::job(&source)?;
    let lease = template.lock()?;
    let mut job = template.current()?;
    let request_id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    anyhow::ensure!(
        !request_id.trim().is_empty() && request_id.len() <= 256,
        "invalid request id"
    );
    if let Some(request) = job.resumes.get(&request_id) {
        let stage = &request.stage;
        anyhow::ensure!(
            request.eager_ram == eager_ram,
            "REQUEST_ID_CONFLICT: resume RAM policy changed"
        );
        anyhow::ensure!(
            stage.join("run.json").is_file() && job.state != JobState::Restoring,
            "EXECUTION_UNKNOWN: resume request admitted without confirmed successor startup at {}",
            stage.display()
        );
        run_log!("resume request already admitted: {}", stage.display());
        return Ok(0);
    }
    anyhow::ensure!(
        job.state == JobState::Suspended,
        "JOB_BUSY: resume requires a confirmed suspended head (state={})",
        job.state
    );
    let checkpoint = job.checkpoint(job.head.as_deref().context("missing suspended head")?)?;
    let source_lease = source.lock_current()?.1;
    let stage = job
        .root
        .join("attempts")
        .join(uuid::Uuid::new_v4().to_string());
    let (mut executor, mut overlay) =
        VmExecutor::restore(job.config.vm.clone(), checkpoint.clone(), &stage)?;
    if eager_ram {
        executor.materialize_restore_ram(&stage)?;
    }
    preserve_apply_target(&source, &mut overlay);
    let previous = job.clone();
    job.previous_stage = job.active_stage.clone();
    job.active_stage = stage.clone();
    job.state = JobState::Restoring;
    job.resumes.insert(
        request_id,
        job_execution::ResumeRequest {
            stage: stage.clone(),
            eager_ram,
        },
    );
    job.link_stage(&stage)?;
    job.write()?;
    drop(lease);
    // Keep the previous Attempt's lease until the successor has completed.
    // The new Attempt acquires its own lease through normal runtime admission.
    let result = execute_restored(job.clone(), stage, executor, overlay, checkpoint).await;
    drop(source_lease);
    if result.is_err() {
        let _lease = job.lock()?;
        let current = job.current()?;
        if current.state == JobState::Restoring {
            // No RunHandle was accepted, hence the suspended head is retryable.
            previous.write()?;
        }
    }
    result
}

fn preserve_apply_target(source: &RunRecord, overlay: &mut OverlayHint) {
    if let (Some(original), Some(saved)) = (&source.overlay, &mut overlay.execution_snapshot) {
        saved.target = original.target.clone();
        if saved.baseline_lower.is_none() {
            saved.baseline_lower = overlay.lower_dirs.last().cloned();
        }
        overlay.protect_target = original.protect_target;
    }
}

async fn fork_execution(args: ForkArgs, source: RunRecord) -> anyhow::Result<i32> {
    use crate::runtime::job_execution;
    use pvisor_core::operation::SnapshotRamStorage;
    crate::cli::checkpoint::check_execution(&source)?;
    let request_id = args
        .request_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    anyhow::ensure!(
        !request_id.trim().is_empty() && request_id.len() <= 256,
        "invalid request id"
    );
    let options = job_execution::ForkOptions {
        checkpoint: args.checkpoint.clone(),
        stage: args.stage.clone(),
        name: args.name.clone(),
        ram_storage: args.ram_storage.map(SnapshotRamStorage::from),
        eager_ram: args.eager_ram,
    };
    let selected = job_execution::job(&source)?;
    if let Some(previous) = selected.forks.get(&request_id) {
        anyhow::ensure!(
            previous.options == options,
            "REQUEST_ID_CONFLICT: execution fork options changed"
        );
        let stage = &previous.stage;
        let child=RunRecord::read(stage).context("EXECUTION_UNKNOWN: branch admitted without a confirmed Attempt; inspect retained branch stage")?;
        anyhow::ensure!(
            child.run_id == previous.job_id,
            "branch Job binding mismatch"
        );
        run_log!(
            "fork request already admitted: Job {} at {}",
            child.run_id,
            stage.display()
        );
        return Ok(0);
    }
    let checkpoint = match args.checkpoint.as_deref() {
        Some(id) => job_execution::job(&source)?.checkpoint(id)?,
        None => {
            let job = job_execution::job(&source)?;
            if job.state == JobState::Suspended {
                job.checkpoint(job.head.as_deref().context("missing suspended head")?)?
            } else {
                job_execution::capture(
                    &source,
                    false,
                    args.ram_storage
                        .map(Into::into)
                        .unwrap_or(SnapshotRamStorage::Compressed),
                    Some({
                        use sha2::Digest;
                        format!(
                            "fork-{}",
                            crate::util::encode_hex(&sha2::Sha256::digest(request_id.as_bytes()))
                        )
                    }),
                    std::time::Duration::from_secs(120),
                )
                .await?
            }
        }
    };
    let template = job_execution::job(&source)?;
    let _lease = template.lock()?;
    let mut parent = template.current()?;
    if let Some(previous) = parent.forks.get(&request_id) {
        anyhow::ensure!(
            previous.options == options,
            "REQUEST_ID_CONFLICT: execution fork options changed"
        );
        anyhow::bail!(
            "EXECUTION_UNKNOWN: branch already admitted at {}; inspect status or retry the same request",
            previous.stage.display()
        );
    }
    parent.checkpoint(&checkpoint.snapshot_id)?;
    let run_id = format!("run-{}", uuid::Uuid::new_v4());
    let stage = fork_stage_candidate(
        &args
            .stage
            .unwrap_or_else(|| default_run_home().join(&run_id)),
    )?;
    anyhow::ensure!(
        !paths_overlap(&stage, &parent.root) && !paths_overlap(&stage, &checkpoint.store),
        "execution fork stage overlaps its source Job"
    );
    anyhow::ensure!(
        !stage.exists() || std::fs::read_dir(&stage)?.next().is_none(),
        "execution fork requires an empty stage"
    );
    let (mut executor, mut overlay) =
        VmExecutor::restore(parent.config.vm.clone(), checkpoint.clone(), &stage)?;
    if args.eager_ram {
        executor.materialize_restore_ram(&stage)?;
    }
    preserve_apply_target(&source, &mut overlay);
    let mut spec = parent.spec.clone();
    spec.metadata.remove(job_execution::STORE_KEY);
    spec.run_id = run_id.clone().into();
    spec.parent_run_id = Some(parent.run_id.clone().into());
    spec.metadata.insert(
        "pvisor.lineage".into(),
        serde_json::json!({"parent_run_id":parent.run_id,"checkpoint_id":checkpoint.snapshot_id}),
    );
    if let Some(name) = args.name {
        spec.metadata.insert(
            "pvisor.orchestration.job_name".into(),
            serde_json::json!(name),
        );
    }
    let child = job_execution::Job {
        version: job_execution::JOB_SCHEMA_VERSION,
        run_id: run_id.clone(),
        root: stage.clone(),
        active_stage: stage.clone(),
        previous_stage: stage.clone(),
        active_attempt: String::new(),
        config: parent.config.clone(),
        spec,
        state: JobState::Restoring,
        head: None,
        checkpoints: Default::default(),
        requests: Default::default(),
        resumes: Default::default(),
        forks: Default::default(),
        stores: [stage.join("execution-snapshots")].into(),
    };
    child.write()?;
    child.link_stage(&stage)?;
    // Pin before launching; a crash cannot leave a child with a deletable source.
    parent
        .checkpoints
        .get_mut(&checkpoint.snapshot_id)
        .context("checkpoint disappeared")?
        .branches
        .insert(run_id, stage.clone());
    parent.forks.insert(
        request_id,
        job_execution::ForkRequest {
            options,
            stage: stage.clone(),
            job_id: child.run_id.clone(),
            checkpoint_id: checkpoint.snapshot_id.clone(),
        },
    );
    parent.write()?;
    drop(_lease);
    execute_restored(child, stage, executor, overlay, checkpoint).await
}

async fn execute_restored(
    job: crate::runtime::job_execution::Job,
    stage: PathBuf,
    executor: VmExecutor,
    overlay: OverlayHint,
    checkpoint: pvisor_core::operation::ExecutionCheckpoint,
) -> anyhow::Result<i32> {
    let config = &job.config;
    let saved_environment = executor
        .restored_guest_environment()
        .context("missing captured guest environment")?;
    let mut recording = None;
    let event_sink: Arc<dyn crate::EventSink> =
        if config.gateway.mode == GatewayMode::Capture || config.record.destination.is_some() {
            let destination = config
                .record
                .destination
                .clone()
                .unwrap_or_else(|| stage.join(".capture"));
            let writer = JournalRecording::open(&destination)?;
            let sink = Arc::new(writer.journal.clone());
            recording = Some(writer);
            sink
        } else {
            Arc::new(crate::trace::Journal::memory())
        };
    let network = NetworkDriverConfig::new(
        config.overlaynet.mode,
        NetworkConfig {
            capability: None,
            mode: match config.overlaynet.policy {
                OverlayNetPolicy::Public => NetworkMode::Public,
                OverlayNetPolicy::Deny => NetworkMode::NoNetwork,
                OverlayNetPolicy::Allowlist => NetworkMode::Allowlist,
            },
            allowed_hosts: config.overlaynet.allow.clone(),
            rules: config.overlaynet.rules.clone(),
            deny_rules: config.overlaynet.deny.clone(),
            limits: config.overlaynet.limits.clone(),
        },
    )
    .listen(&config.overlaynet.listen);
    #[allow(unused_mut)]
    let mut builder = PVisor::builder()
        .storage(&stage)
        .overlay(overlay)
        .executors(vec![Arc::new(executor)])
        .network(network)
        .event_sink(event_sink);
    let control_socket = crate::cli::host_service::vm_control_socket(None)?
        .or_else(|| config.vm.control_socket.clone());
    if let Some(path) = &control_socket {
        builder = builder.control_socket(path);
    }
    #[cfg(feature = "gateway")]
    if let Some(proxy) = resolve_proxy(config)? {
        builder = builder.gateway(
            GatewayDriverConfig::new(proxy)
                .output_dir(&stage)
                .gateway_enabled(config.gateway.mode == GatewayMode::Capture),
        );
    }
    let pvisor = builder.build();
    let mut spec = job.spec.clone();
    spec.metadata
        .remove(crate::runtime::job_execution::STORE_KEY);
    let RunInvocation::Process(process) = &mut spec.invocation;
    process.inherit_env = false;
    process.env = saved_environment;
    spec.metadata.insert("pvisor.environment".into(),serde_json::json!({"inherits_host":false,"projected_keys":process.env.keys().collect::<Vec<_>>()}));
    spec.metadata
        .insert("pvisor.stage".into(), serde_json::to_value(&stage)?);
    spec.metadata.insert(
        "pvisor.orchestration.execution_restore".into(),
        serde_json::to_value(&checkpoint)?,
    );
    #[cfg(unix)]
    crate::cli::terminal::announce_stage(&stage);
    let handle = pvisor.run(spec.clone()).await?;
    announce_control_socket(&handle);
    let record = RunRecord::read(&stage)?;
    let server = crate::runtime::job_execution::Server::start(
        &record,
        job.config.clone(),
        spec,
        handle.controls(),
    )?;
    let cancellation = handle.cancellation();
    let wait = handle.wait();
    tokio::pin!(wait);
    let result = tokio::select! {
        result = &mut wait => result?,
        _ = delegated_shutdown_signal() => { cancellation.cancel(); wait.await? }
    };
    server.finish(&result).await?;
    if let Some(writer) = recording {
        writer.finish()?;
    }
    run_log!("Run Bundle: {}", RunBundle::path(&stage).display());
    if let Some(failure) = &result.failure {
        run_log!("pVisor Job failed: {:?}: {}", failure.kind, failure.message);
    }
    Ok(match result.state {
        RunState::Completed => result.exit_code.unwrap_or(0),
        RunState::Hibernated => 0,
        RunState::Cancelled => 130,
        _ => result.exit_code.unwrap_or(1),
    })
}

// Stage directories can sit below a workspace that the guest sees. Keep the
// immutable capture store outside every guest backing root, without changing
// the stage, normal filesystem configuration, or guest I/O path.
pub(super) fn execution_store_location(
    config: &RunConfig,
    workspace: &Path,
    stage: &Path,
    run_id: &str,
) -> anyhow::Result<PathBuf> {
    let mut roots = vec![workspace.to_owned()];
    if let Some(root) = &config.vm.rootfs {
        roots.push(root.canonicalize()?);
    }
    if let Some(overlay) = &config.overlayfs {
        for root in &overlay.compose {
            roots.push(root.canonicalize()?);
        }
    }
    let mut candidates = vec![
        stage.join("execution-snapshots"),
        default_run_home().join("execution-snapshots").join(run_id),
        std::env::temp_dir()
            .join("pvisor-execution-snapshots")
            .join(run_id),
    ];
    if roots.iter().any(|root| stage.starts_with(root)) {
        // A nested stage cannot host its own backing copies. Prefer the nearest
        // outside ancestor, retaining the stage's filesystem where possible.
        // Directory link counts can differ between filesystems, so moving an
        // otherwise exact copy to the default run home can violate restoration.
        let mut ancestor = stage.parent();
        while let Some(parent) = ancestor {
            if roots.iter().all(|root| !parent.starts_with(root)) {
                if parent.parent().is_some() {
                    candidates.insert(0, parent.join("pvisor-execution-snapshots").join(run_id));
                }
                break;
            }
            ancestor = parent.parent();
        }
    }
    if let Some(pool) = &config.vm.snapshot_filesystem_pool {
        // Pooled snapshots need hard-linked references on the pool's volume.
        // Prefer a sibling store while keeping it outside guest-visible roots.
        let parent = pool
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        candidates.insert(0, parent.join("pvisor-execution-snapshots").join(run_id));
    }
    for candidate in candidates {
        let candidate = fork_stage_candidate(&candidate)?;
        if roots.iter().all(|root| !candidate.starts_with(root)) {
            return Ok(candidate);
        }
    }
    anyhow::bail!("no execution snapshot store outside guest backing roots")
}
