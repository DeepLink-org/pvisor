//! Job-scoped workspace and execution checkpoint management.
use crate::runtime::checkpoint::{checkpoint_branch_refs, list_checkpoints, resolve_checkpoint};
use crate::runtime::job_execution::{self, Job};
use crate::runtime::{RunRecord, resolve_run};
use anyhow::Context;
use clap::{Args, Subcommand, ValueEnum};
use pvisor_core::operation::SnapshotRamStorage;

use std::path::PathBuf;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    #[default]
    Workspace,
    Execution,
}

#[derive(Debug, Clone, Copy, ValueEnum, serde::Serialize, serde::Deserialize)]
pub(super) enum RamStorage {
    Raw,
    Compressed,
}
impl From<RamStorage> for SnapshotRamStorage {
    fn from(value: RamStorage) -> Self {
        match value {
            RamStorage::Raw => Self::Raw,
            RamStorage::Compressed => Self::Compressed,
        }
    }
}

#[derive(Debug, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Selection {
    /// Explicit Job id, stage path, run.json, or last.
    pub job: PathBuf,
    #[arg(long, short = 'o', default_value = ".pvisor/capture")]
    pub output_dir: PathBuf,
}
impl Selection {
    pub fn resolve(&self) -> anyhow::Result<RunRecord> {
        let record = resolve_run(Some(&self.job), &self.output_dir)?;
        super::host_service::check_record(&record)?;
        Ok(record)
    }
}

#[derive(Debug, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckpointArgs {
    #[command(subcommand)]
    command: CheckpointCommand,
}

#[derive(Debug, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SuspendArgs {
    #[command(flatten)]
    selection: Selection,
    #[arg(long, value_enum)]
    ram_storage: Option<RamStorage>,
    #[arg(long)]
    request_id: Option<String>,
    #[arg(long)]
    json: bool,
    /// Maximum time to wait (for example 30s or 2m); timeout never kills the VM.
    #[arg(long, default_value = "120s")]
    timeout: super::run::DurationMs,
}

#[derive(Debug, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeArgs {
    #[command(flatten)]
    selection: Selection,
    #[arg(long)]
    pub tui: bool,
    /// Read all captured RAM before starting the successor; default is lazy.
    #[arg(long)]
    eager_ram: bool,
    #[arg(long)]
    request_id: Option<String>,
}

impl SuspendArgs {
    pub(super) fn selection(&self) -> &Selection {
        &self.selection
    }
    pub(super) fn selection_mut(&mut self) -> &mut Selection {
        &mut self.selection
    }
    pub(super) fn job_selector(&self) -> &std::path::Path {
        &self.selection.job
    }
}
impl ResumeArgs {
    pub(super) fn selection(&self) -> &Selection {
        &self.selection
    }
    pub(super) fn selection_mut(&mut self) -> &mut Selection {
        &mut self.selection
    }
    pub(super) fn job_selector(&self) -> &std::path::Path {
        &self.selection.job
    }
}

pub(super) async fn suspend(args: SuspendArgs) -> anyhow::Result<()> {
    let record = args.selection.resolve()?;
    let request_id = args
        .request_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let response = crate::runtime::job_service::RuntimeJobService::capture_selected_execution(
        &super::host::service_context(),
        &record,
        true,
        args.ram_storage
            .map(Into::into)
            .unwrap_or(SnapshotRamStorage::Compressed),
        Some(request_id.clone()),
        std::time::Duration::from_millis(args.timeout.0),
    )
    .await?;
    let checkpoint = response.checkpoint;
    emit(
        args.json,
        serde_json::json!({"schema_version":1,"operation":"suspend","job_id":record.run_id,"state":"suspended","request_id":request_id,"checkpoint_id":checkpoint.snapshot_id,"kind":"execution","checkpoint":checkpoint}),
    )
}

pub(super) async fn resume(args: ResumeArgs) -> anyhow::Result<i32> {
    let record = args.selection.resolve()?;
    check_execution(&record)?;
    super::run::resume_execution(record, args.request_id, args.eager_ram).await
}
#[derive(Debug, Subcommand, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
enum CheckpointCommand {
    /// Save staged files, or capture a live VM's execution and continue it.
    Create {
        #[command(flatten)]
        selection: Selection,
        #[arg(long, value_enum, default_value = "workspace")]
        kind: Kind,
        /// RAM encoding for execution capture only.
        #[arg(long, value_enum)]
        ram_storage: Option<RamStorage>,
        /// Durable idempotency key for workspace creation.
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value = "120s")]
        timeout: super::run::DurationMs,
    },
    /// List immutable savepoints belonging to a Job.
    List {
        #[command(flatten)]
        selection: Selection,
        #[arg(long, value_enum)]
        kind: Option<Kind>,
        #[arg(long)]
        json: bool,
    },
    /// Show a savepoint and its retained branch references.
    Show {
        #[command(flatten)]
        selection: Selection,
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete an unreferenced savepoint of a stopped Job.
    Delete {
        #[command(flatten)]
        selection: Selection,
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Collect abandoned workspace/native store transactions; retain published checkpoints.
    Gc {
        #[command(flatten)]
        selection: Selection,
        #[arg(long)]
        json: bool,
        /// Omit to collect both stores; execution collection can run on a suspended Job.
        #[arg(long, value_enum)]
        kind: Option<Kind>,
    },
    /// Verify a sealed native execution checkpoint's compatibility and payload.
    Verify {
        #[command(flatten)]
        selection: Selection,
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Import a rootfs into the Job's immutable base store.
    ImportBase {
        #[command(flatten)]
        selection: Selection,
        rootfs: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Verify an imported immutable rootfs generation.
    VerifyBase {
        #[command(flatten)]
        selection: Selection,
        id: String,
        #[arg(long)]
        json: bool,
    },
}

impl CheckpointArgs {
    pub(super) fn selection(&self) -> &Selection {
        match &self.command {
            CheckpointCommand::Create { selection, .. }
            | CheckpointCommand::List { selection, .. }
            | CheckpointCommand::Show { selection, .. }
            | CheckpointCommand::Delete { selection, .. }
            | CheckpointCommand::Gc { selection, .. }
            | CheckpointCommand::Verify { selection, .. }
            | CheckpointCommand::ImportBase { selection, .. }
            | CheckpointCommand::VerifyBase { selection, .. } => selection,
        }
    }
    pub(super) fn selection_mut(&mut self) -> &mut Selection {
        match &mut self.command {
            CheckpointCommand::Create { selection, .. }
            | CheckpointCommand::List { selection, .. }
            | CheckpointCommand::Show { selection, .. }
            | CheckpointCommand::Delete { selection, .. }
            | CheckpointCommand::Gc { selection, .. }
            | CheckpointCommand::Verify { selection, .. }
            | CheckpointCommand::ImportBase { selection, .. }
            | CheckpointCommand::VerifyBase { selection, .. } => selection,
        }
    }
}

/// Check the Job configuration before admitting native execution checkpoints.
/// Platform support alone does not guarantee that this Job can be restored.
pub(crate) fn execution_blocker(record: &RunRecord) -> Option<String> {
    crate::runtime::job_service::execution_blocker(record)
}
pub(super) fn check_execution(record: &RunRecord) -> anyhow::Result<()> {
    crate::runtime::job_service::require_execution(record)
}

pub(super) async fn run(args: CheckpointArgs) -> anyhow::Result<()> {
    match args.command {
        CheckpointCommand::Create {
            selection,
            kind,
            ram_storage,
            request_id,
            json,
            timeout,
        } => {
            anyhow::ensure!(
                kind == Kind::Execution || ram_storage.is_none(),
                "--ram-storage is only valid for execution capture"
            );
            let selected = selection.resolve()?;
            if kind == Kind::Execution {
                let request_id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let response =
                    crate::runtime::job_service::RuntimeJobService::capture_selected_execution(
                        &super::host::service_context(),
                        &selected,
                        false,
                        ram_storage
                            .map(Into::into)
                            .unwrap_or(SnapshotRamStorage::Compressed),
                        Some(request_id.clone()),
                        std::time::Duration::from_millis(timeout.0),
                    )
                    .await?;
                let checkpoint = response.checkpoint;
                return emit(
                    json,
                    serde_json::json!({"schema_version":1,"operation":"checkpoint.create","job_id":selected.run_id,"request_id":request_id,"checkpoint_id":checkpoint.snapshot_id,"kind":"execution","checkpoint":checkpoint}),
                );
            }
            let (checkpoint, reused) = crate::runtime::job_service::RuntimeJobService::create_selected_workspace_checkpoint(
                &super::host::service_context(),
                &selected,
                request_id.as_deref(),
            )?;
            emit(
                json,
                serde_json::json!({
                    "schema_version": 1, "operation": "checkpoint.create", "job_id": selected.run_id,
                    "request_id": request_id, "checkpoint_id": checkpoint.checkpoint_id,
                    "kind": "workspace", "reused": reused, "checkpoint": checkpoint,
                }),
            )
        }
        CheckpointCommand::List {
            selection,
            kind,
            json,
        } => {
            let record = selection.resolve()?;
            let mut checkpoints: Vec<serde_json::Value> = if kind == Some(Kind::Execution) {
                Vec::new()
            } else {
                list_checkpoints(&record)?
                    .into_iter()
                    .map(serde_json::to_value)
                    .collect::<Result<_, _>>()?
            };
            if kind != Some(Kind::Workspace)
                && let Some(job) = Job::read(&record)?
            {
                checkpoints.extend(job.checkpoints.values().map(|capture| serde_json::json!({"checkpoint_id":capture.checkpoint.snapshot_id,"kind":"execution","checkpoint":capture.checkpoint,"branch_references":capture.branches})));
            }
            emit(
                json,
                serde_json::json!({"schema_version":1,"operation":"checkpoint.list",
                    "job_id":record.run_id,"kind_filter":kind,"execution_blocker":execution_blocker(&record),"checkpoints":checkpoints}),
            )
        }
        CheckpointCommand::Show {
            selection,
            id,
            json,
        } => {
            let record = selection.resolve()?;
            if let Some(job) = Job::read(&record)?
                && let Some(capture) = job.checkpoints.get(&id)
            {
                return emit(
                    json,
                    serde_json::json!({"schema_version":1,"operation":"checkpoint.show","job_id":record.run_id,"kind":"execution","checkpoint":capture.checkpoint,"branch_references":capture.branches,"suspended_head":job.head.as_deref()==Some(&id)}),
                );
            }
            // Keep the source manifest alive while computing reference information.
            let (record, _lease) = record.lock_current()?;
            let checkpoint = resolve_checkpoint(&record, &id)?;
            let refs = checkpoint_branch_refs(&checkpoint)?;
            emit(
                json,
                serde_json::json!({"schema_version":1,"operation":"checkpoint.show",
                "job_id":record.run_id,"kind":"workspace","branch_references":refs,
                "checkpoint":checkpoint}),
            )
        }
        CheckpointCommand::Delete {
            selection,
            id,
            json,
        } => {
            let record = selection.resolve()?;
            let template = Job::read(&record)?;
            let _job_lease = template.as_ref().map(Job::lock).transpose()?;
            let job = template.map(|job| job.current()).transpose()?;
            if let Some(job) = &job {
                job.validate_record_target(&record)?;
                let current = RunRecord::read(&job.active_stage)?;
                super::host::check_selected_record(&record, &current)?;
                super::host_service::check_record(&current)?;
            }
            if let Some(mut job) = job
                && job.checkpoints.contains_key(&id)
            {
                let capture = job.checkpoints.get(&id).context("checkpoint disappeared")?;
                anyhow::ensure!(
                    job.head.as_deref() != Some(&id) && capture.branches.is_empty(),
                    "CHECKPOINT_REFERENCED: retained by suspended head or child Jobs"
                );
                // Store-level reader leases also protect active native restores.
                let store = checkpoint_store(&job, &capture.checkpoint.store)?;
                store.delete(&id)?;
                job.checkpoints.remove(&id);
                // Keep admitted request ids fenced after deletion.
                for request in job.requests.values_mut() {
                    if request.checkpoint.as_deref() == Some(&id) {
                        request.checkpoint = None;
                        request.error = Some("checkpoint deleted".into());
                    }
                }
                job.write()?;
                return emit(
                    json,
                    serde_json::json!({"schema_version":1,"operation":"checkpoint.delete","job_id":record.run_id,"kind":"execution","checkpoint_id":id,"deleted":true}),
                );
            }
            let id = crate::runtime::job_service::RuntimeJobService::delete_selected_workspace_checkpoint(
                &super::host::service_context(),
                &record,
                &id,
            )?;
            emit(
                json,
                serde_json::json!({"schema_version":1,"operation":"checkpoint.delete",
                "job_id":record.run_id,"checkpoint_id":id,"deleted":true}),
            )
        }
        CheckpointCommand::Gc {
            selection,
            json,
            kind,
        } => {
            let record = selection.resolve()?;
            let template = Job::read(&record)?;
            let _job_lease = template.as_ref().map(Job::lock).transpose()?;
            let job = template.map(|job| job.current()).transpose()?;
            if let Some(job) = &job {
                job.validate_record_target(&record)?;
                let current = RunRecord::read(&job.active_stage)?;
                super::host::check_selected_record(&record, &current)?;
                super::host_service::check_record(&current)?;
            }
            let scope = if job.is_some() {
                "job_checkpoint_transactions"
            } else {
                "job_workspace_transactions"
            };
            let removed = if kind == Some(Kind::Execution) {
                0
            } else {
                crate::runtime::job_service::RuntimeJobService::collect_selected_workspace_transactions(
                    &super::host::service_context(),
                    &record,
                )?
            };
            let mut execution_removed = 0;
            if kind != Some(Kind::Workspace)
                && let Some(job) = job
            {
                let mut stores = job.stores.clone();
                stores.insert(job.active_stage.join("execution-snapshots"));
                stores.extend(
                    job.checkpoints
                        .values()
                        .map(|capture| capture.checkpoint.store.clone()),
                );
                for path in stores {
                    execution_removed += checkpoint_store(&job, &path)?.collect_abandoned()?;
                }
            }
            emit(
                json,
                serde_json::json!({"schema_version":1,"operation":"checkpoint.gc",
                "job_id":record.run_id,"scope":scope,"kind_filter":kind,
                "root":record.stage_dir().join(crate::CHECKPOINTS_DIR),"removed_transactions":removed,
                "execution_removed_transactions":execution_removed,"published_checkpoints_deleted":0}),
            )
        }
        CheckpointCommand::Verify {
            selection,
            id,
            json,
        } => {
            let record = selection.resolve()?;
            let template = job_execution::job(&record)?;
            let _lease = template.lock()?;
            let job = template.current()?;
            job.validate_record_target(&record)?;
            super::host_service::check_record(&RunRecord::read(&job.active_stage)?)?;
            let checkpoint = job.checkpoint(&id)?;
            let compatibility = crate::VmExecutor::checkpoint_compatibility(&job.config.vm)?;
            let object = checkpoint_store(&job, &checkpoint.store)?.open(&id, &compatibility)?;
            emit(
                json,
                serde_json::json!({"operation":"checkpoint.verify","job_id":job.run_id,"checkpoint_id":id,"verified":true,"manifest":object.manifest()}),
            )
        }
        CheckpointCommand::ImportBase {
            selection,
            rootfs,
            json,
        } => {
            let record = selection.resolve()?;
            let (job, _lease) = super::host::lock_selected_job(&record)?
                .context("this Job has no native execution handoff")?;
            let rootfs = rootfs.canonicalize()?;
            super::host_service::reject_guest_exposure([rootfs.clone()])?;
            let base = checkpoint_store(&job, &job.primary_store()?)?.import_base(&rootfs)?;
            emit(
                json,
                serde_json::json!({"operation":"checkpoint.import_base","job_id":job.run_id,"base":base.reference(),"rootfs":base.root()}),
            )
        }
        CheckpointCommand::VerifyBase {
            selection,
            id,
            json,
        } => {
            let record = selection.resolve()?;
            let job = job_execution::job(&record)?;
            let base = checkpoint_store(&job, &job.primary_store()?)?
                .open_base(&crate::environment_snapshot::BaseReference { id })?;
            emit(
                json,
                serde_json::json!({"operation":"checkpoint.verify_base","job_id":job.run_id,"base":base.reference(),"verified":true}),
            )
        }
    }
}

fn checkpoint_store(
    job: &Job,
    path: &std::path::Path,
) -> anyhow::Result<crate::environment_snapshot::SnapshotStore> {
    match job.config.vm.snapshot_filesystem_pool.as_deref() {
        Some(pool) => crate::environment_snapshot::SnapshotStore::with_filesystem_pool(path, pool),
        None => crate::environment_snapshot::SnapshotStore::new(path),
    }
}

fn emit(json: bool, mut value: serde_json::Value) -> anyhow::Result<()> {
    value["schema_version"] = serde_json::json!(1);
    if json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!(
            "{}: Job {}",
            value["operation"].as_str().unwrap_or("checkpoint"),
            value["job_id"].as_str().unwrap_or("-")
        );
        if let Some(checkpoints) = value["checkpoints"].as_array() {
            for cp in checkpoints {
                println!(
                    "  {} ({})",
                    cp["checkpoint_id"].as_str().unwrap_or("-"),
                    cp["kind"].as_str().unwrap_or("workspace")
                );
            }
        } else {
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
    }
    Ok(())
}
