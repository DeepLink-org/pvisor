//! Workspace branches and native execution restoration for CLI Jobs.
#[cfg(feature = "gateway")]
use super::resolve_proxy;
use super::{
    announce_control_socket, execute_config, paths_overlap, report_terminal, resolve_workspace,
    select_run_storage, wait_cli_job,
};
#[cfg(feature = "gateway")]
use crate::GatewayDriverConfig;
use crate::cli::trajectory::JournalRecording;
#[cfg(test)]
use crate::config::RunExecutorKind;
use crate::config::{GatewayMode, OverlayFsCommit, OverlayFsSettings, OverlayNetPolicy, RunConfig};
#[cfg(test)]
use crate::runtime::job_execution::JobState;
use crate::runtime::job_service::paths::fork_stage_candidate;
use crate::runtime::{RunLineage, RunRecord, default_run_home, resolve_run};
use crate::{NetworkDriverConfig, PVisor, RunBundle, restore_logical_checkpoint};

use clap::Args;
use pvisor_core::RunState;
use pvisor_overlaynet::{NetworkConfig, NetworkMode};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, Clone, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
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

impl ForkArgs {
    pub(in crate::cli) fn restores_execution(&self) -> bool {
        self.state == crate::cli::checkpoint::Kind::Execution
    }
    pub(in crate::cli) fn selection(&self) -> (&Path, &Path) {
        (&self.source, &self.output_dir)
    }
    pub(in crate::cli) fn pin_selection(&mut self, path: PathBuf) {
        self.source = path;
    }
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
    crate::cli::host_service::check_record(&source)?;
    if args.state == crate::cli::checkpoint::Kind::Execution {
        return fork_execution(args, source).await;
    }
    let source_job = crate::cli::host::lock_selected_job(&source)?;
    // Hold ownership from selection through copy and durable source retention.
    // The runner starts only after releasing the source's lease.
    let (source, source_lease) = source.lock_current()?;
    crate::cli::host_service::check_record(&source)?;
    // Fail before checkpoint/stage mutations when historical policy cannot be reconstructed.
    let (mut config, required_sandbox) =
        crate::runtime::job_service::policy::workspace_config(&source)?;
    let parent_isolation = source
        .executor
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("workspace fork lacks parent executor boundary evidence"))?
        .isolation;
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
    drop(source_lease);
    drop(source_job);
    execute_config(
        config,
        run_id,
        required_sandbox,
        Some(RunLineage {
            parent_run_id: source.run_id,
            checkpoint_id: checkpoint.checkpoint_id,
        }),
        crate::runtime::job_service::policy::PolicySource::Inherited(parent_isolation),
    )
    .await
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
    use crate::runtime::job_service::{
        JobSelection, ResumeRequest, ResumeResponse, RuntimeJobService,
    };
    let response = RuntimeJobService::resume(
        &crate::cli::host::service_context(),
        ResumeRequest {
            job: JobSelection {
                selector: Some(source.stage_dir()),
                storage: source.storage,
            },
            request_id,
            eager_ram,
        },
        execute_restored,
    )
    .await?;
    match response {
        ResumeResponse::AlreadyAdmitted { stage } => {
            run_log!("resume request already admitted: {}", stage.display());
            Ok(0)
        }
        ResumeResponse::Finished { exit_code } => Ok(exit_code),
    }
}

async fn fork_execution(args: ForkArgs, source: RunRecord) -> anyhow::Result<i32> {
    use crate::runtime::job_service::{
        ExecutionForkRequest, ExecutionForkResponse, JobSelection, RuntimeJobService,
    };
    let response = RuntimeJobService::fork_execution(
        &crate::cli::host::service_context(),
        ExecutionForkRequest {
            job: JobSelection {
                selector: Some(source.stage_dir()),
                storage: source.storage,
            },
            checkpoint: args.checkpoint,
            stage: args.stage,
            name: args.name,
            ram_storage: args.ram_storage.map(Into::into),
            eager_ram: args.eager_ram,
            request_id: args.request_id,
        },
        execute_restored,
    )
    .await?;
    match response {
        ExecutionForkResponse::AlreadyAdmitted { job_id, stage } => {
            run_log!(
                "fork request already admitted: Job {} at {}",
                job_id,
                stage.display()
            );
            Ok(0)
        }
        ExecutionForkResponse::Finished { exit_code } => Ok(exit_code),
    }
}

async fn execute_restored(
    attempt: crate::runtime::job_service::RestoredAttempt,
) -> anyhow::Result<i32> {
    let stage = attempt.stage.clone();
    let mut recording = None;
    let managed = attempt
        .start_with(|config, stage, executor, overlay| {
            let event_sink: Arc<dyn crate::EventSink> = if config.gateway.mode
                == GatewayMode::Capture
                || config.record.destination.is_some()
            {
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
                .executors(vec![report_terminal(Arc::new(executor))])
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
            #[cfg(unix)]
            crate::cli::terminal::announce_stage(stage);
            Ok(builder.build())
        })
        .await?;
    announce_control_socket(managed.handle());
    let result = wait_cli_job(managed, Some(&stage)).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::job_execution::{
        ForkOptions, ForkRequest, JOB_SCHEMA_VERSION, Job, ResumeRequest,
    };
    fn fixture(root: &Path) -> (RunRecord, Job) {
        let record: RunRecord = serde_json::from_value(serde_json::json!({
            "schema_version":1,"run_id":"job-fence","attempt_id":"attempt-old","session_id":"job-fence",
            "agent":"probe","pid":0,"command":["probe"],"state":"hibernated",
            "started_at_unix_ms":1,"finished_at_unix_ms":2,"storage":root,"network":{},"gateway_listen":null,"overlay":null
        })).unwrap();
        record.write().unwrap();
        let mut config = RunConfig::default();
        config.run.executor = RunExecutorKind::Vm;
        config.overlaynet.mode = crate::OverlayNetMode::Off;
        let job = Job {
            version: JOB_SCHEMA_VERSION,
            run_id: record.run_id.clone(),
            root: root.into(),
            active_stage: root.into(),
            previous_stage: root.into(),
            active_attempt: "attempt-current".into(),
            config,
            spec: pvisor_core::RunSpec::process("job-fence", "probe", "probe"),
            state: JobState::Suspended,
            head: None,
            checkpoints: Default::default(),
            requests: Default::default(),
            resumes: Default::default(),
            forks: Default::default(),
            stores: [root.join("execution-snapshots")].into(),
        };
        (record, job)
    }
    fn assert_conflict(error: anyhow::Error) {
        assert_eq!(
            error
                .downcast_ref::<pvisor_core::host_protocol::AgentCtlHostError>()
                .unwrap()
                .code,
            pvisor_core::host_protocol::AgentCtlHostErrorCode::Conflict
        );
    }
    #[tokio::test]
    async fn cached_resume_and_fork_replays_are_fenced_under_job_lock() {
        let root = tempfile::tempdir().unwrap();
        let (source, mut job) = fixture(root.path());
        job.resumes.insert(
            "replay".into(),
            ResumeRequest {
                stage: root.path().into(),
                eager_ram: false,
            },
        );
        job.forks.insert(
            "replay".into(),
            ForkRequest {
                options: ForkOptions {
                    checkpoint: None,
                    stage: None,
                    name: None,
                    ram_storage: None,
                    eager_ram: false,
                },
                stage: root.path().join("unconfirmed-child"),
                job_id: "child".into(),
                checkpoint_id: "checkpoint".into(),
            },
        );
        job.write().unwrap();
        let before = std::fs::read_dir(root.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_conflict(
            resume_execution(source.clone(), Some("replay".into()), false)
                .await
                .unwrap_err(),
        );
        let args = ForkArgs {
            source: root.path().into(),
            state: crate::cli::checkpoint::Kind::Execution,
            checkpoint: None,
            stage: None,
            name: None,
            ram_storage: None,
            eager_ram: false,
            request_id: Some("replay".into()),
            output_dir: root.path().into(),
            command: vec![],
        };
        assert_conflict(fork_execution(args, source).await.unwrap_err());
        assert_eq!(job.current().unwrap().active_attempt, "attempt-current");
        // The operation lease may be created, but no branch or restore is staged.
        assert!(!root.path().join("unconfirmed-child").exists());
        assert!(before.iter().all(|name| root.path().join(name).exists()));
    }
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
