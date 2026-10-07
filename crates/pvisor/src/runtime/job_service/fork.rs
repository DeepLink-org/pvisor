use super::{
    JobSelection, RestoredAttempt, RuntimeJobService, ServiceContext, check_selected_record,
    validate_request_id,
};
use crate::VmExecutor;
use crate::runtime::job_execution::JobState;
use crate::runtime::{RunRecord, default_run_home};
use anyhow::Context;
use pvisor_core::operation::SnapshotRamStorage;
use std::{
    future::Future,
    path::{Path, PathBuf},
};

pub struct ExecutionForkRequest {
    pub job: JobSelection,
    pub checkpoint: Option<String>,
    pub stage: Option<PathBuf>,
    pub name: Option<String>,
    pub ram_storage: Option<SnapshotRamStorage>,
    pub eager_ram: bool,
    pub request_id: Option<String>,
}
#[derive(Debug, serde::Serialize)]
pub enum ExecutionForkResponse {
    AlreadyAdmitted { job_id: String, stage: PathBuf },
    Finished { exit_code: i32 },
}
fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}
impl RuntimeJobService {
    pub async fn fork_execution<F, Fut>(
        context: &ServiceContext<'_>,
        args: ExecutionForkRequest,
        launch: F,
    ) -> anyhow::Result<ExecutionForkResponse>
    where
        F: FnOnce(RestoredAttempt) -> Fut,
        Fut: Future<Output = anyhow::Result<i32>>,
    {
        let source = Self::resolve(context, &args.job)?;
        anyhow::ensure!(
            args.checkpoint.is_none() || args.ram_storage.is_none(),
            "RAM storage cannot change an existing checkpoint"
        );
        use crate::runtime::job_execution;
        super::require_execution(&source)?;
        let request_id = args
            .request_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        validate_request_id(&request_id)?;
        let options = job_execution::ForkOptions {
            checkpoint: args.checkpoint.clone(),
            stage: args.stage.clone(),
            name: args.name.clone(),
            ram_storage: args.ram_storage,
            eager_ram: args.eager_ram,
        };
        let template = job_execution::job(&source)?;
        let selection_lease = template.lock()?;
        let selected = template.current()?;
        selected.validate_record_target(&source)?;
        let current_record = RunRecord::read(&selected.active_stage)?;
        check_selected_record(&source, &current_record)?;
        context.check(&current_record)?;
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
            return Ok(ExecutionForkResponse::AlreadyAdmitted {
                job_id: child.run_id,
                stage: stage.clone(),
            });
        }
        let checkpoint = match args.checkpoint.as_deref() {
            Some(id) => selected.checkpoint(id)?,
            None if selected.state == JobState::Suspended => {
                selected.checkpoint(selected.head.as_deref().context("missing suspended head")?)?
            }
            None => {
                // Capture acquires the same Job lease internally and fences the
                // selected record there. Never await capture while holding it.
                drop(selection_lease);
                let captured = RuntimeJobService::capture_selected_execution(
                    context,
                    &source,
                    false,
                    args.ram_storage.unwrap_or(SnapshotRamStorage::Compressed),
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
                .checkpoint;
                return fork_execution_from_checkpoint(
                    context, args, source, request_id, options, captured, launch,
                )
                .await;
            }
        };
        drop(selection_lease);
        fork_execution_from_checkpoint(
            context, args, source, request_id, options, checkpoint, launch,
        )
        .await
    }
}
async fn fork_execution_from_checkpoint<F, Fut>(
    context: &ServiceContext<'_>,
    args: ExecutionForkRequest,
    source: RunRecord,
    request_id: String,
    options: crate::runtime::job_execution::ForkOptions,
    checkpoint: pvisor_core::operation::ExecutionCheckpoint,
    launch: F,
) -> anyhow::Result<ExecutionForkResponse>
where
    F: FnOnce(RestoredAttempt) -> Fut,
    Fut: Future<Output = anyhow::Result<i32>>,
{
    use crate::runtime::job_execution;
    let template = job_execution::job(&source)?;
    let _lease = template.lock()?;
    let mut parent = template.current()?;
    parent.validate_record_target(&source)?;
    let current_record = RunRecord::read(&parent.active_stage)?;
    check_selected_record(&source, &current_record)?;
    context.check(&current_record)?;
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
    super::preserve_apply_target(&source, &mut overlay);
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
    let completed_stage = stage.clone();
    let child_id = child.run_id.clone();
    let exit_code = launch(RestoredAttempt {
        config: child.config,
        spec: child.spec,
        stage,
        executor,
        overlay,
        checkpoint,
    })
    .await?;
    super::lifecycle::confirm_restored_completion(&completed_stage, &child_id)?;
    Ok(ExecutionForkResponse::Finished { exit_code })
}

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
