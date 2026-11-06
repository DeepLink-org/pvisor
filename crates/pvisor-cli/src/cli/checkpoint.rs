//! Session-scoped workspace and execution checkpoint management.
use anyhow::Context;
use clap::{Args, Subcommand, ValueEnum};
use pvisor::Session;
use pvisor::checkpoint::{checkpoint_branch_refs, list_checkpoints, resolve_checkpoint};
use pvisor::session_execution::{self};
use pvisor::{RunRecord, resolve_run};
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
    /// Explicit Session id, stage path, run.json, or last.
    pub session: PathBuf,
    #[arg(long, short = 'o', default_value = ".pvisor/capture")]
    pub output_dir: PathBuf,
}
impl Selection {
    pub fn resolve(&self) -> anyhow::Result<RunRecord> {
        let record = resolve_run(Some(&self.session), &self.output_dir)?;
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

#[derive(Debug, Args)]
pub(super) struct SuspendCommandArgs {
    #[command(flatten)]
    pub args: SuspendArgs,
    #[command(flatten)]
    pub vm: super::live_vm::SuspendVmOptions,
}

#[derive(Debug, Args)]
pub(super) struct ResumeCommandArgs {
    #[command(flatten)]
    pub args: ResumeArgs,
    #[command(flatten)]
    pub vm: super::live_vm::ResumeVmOptions,
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
    pub(super) fn session_selector(&self) -> &std::path::Path {
        &self.selection.session
    }
}
impl ResumeArgs {
    pub(super) fn has_restore_options(&self) -> bool {
        self.request_id.is_some() || self.eager_ram
    }
    pub(super) fn selection(&self) -> &Selection {
        &self.selection
    }
    pub(super) fn selection_mut(&mut self) -> &mut Selection {
        &mut self.selection
    }
    pub(super) fn session_selector(&self) -> &std::path::Path {
        &self.selection.session
    }
}

async fn capture(
    record: &RunRecord,
    suspend: bool,
    ram_storage: SnapshotRamStorage,
    request_id: String,
    timeout_ms: u64,
) -> anyhow::Result<pvisor::session_service::CaptureResponse> {
    let Some(endpoint) = record
        .live_control
        .as_ref()
        .filter(|endpoint| endpoint.is_available())
    else {
        // A suspended Session has no live owner. The runtime validates its durable
        // head and request receipt under the execution-operation lease; a
        // running Session with a missing endpoint still fails closed there.
        return pvisor::session_service::SessionService::capture_selected_execution(
            &super::host::service_context(),
            record,
            suspend,
            ram_storage,
            Some(request_id),
            std::time::Duration::from_millis(timeout_ms),
        )
        .await;
    };
    Ok(serde_json::from_value(
        endpoint.exchange_checked(
            &record.run_id,
            record
                .execution_id
                .as_deref()
                .context("Session has no Execution identity")?,
            pvisor::host_transport::InstanceCommand::Capture {
                suspend,
                ram_storage,
                request_id,
                timeout_ms,
            },
            Some(&super::host_service::check_cancelled),
        )?,
    )?)
}

pub(super) async fn suspend(args: SuspendArgs) -> anyhow::Result<()> {
    let record = args.selection.resolve()?;
    let request_id = args
        .request_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let response = capture(
        &record,
        true,
        args.ram_storage
            .map(Into::into)
            .unwrap_or(SnapshotRamStorage::Compressed),
        request_id.clone(),
        args.timeout.0,
    )
    .await?;
    let checkpoint = response.checkpoint;
    emit(
        args.json,
        serde_json::json!({"schema_version":1,"operation":"suspend","session_id":record.run_id,"state":"suspended","request_id":request_id,"checkpoint_id":checkpoint.snapshot_id,"kind":"execution","checkpoint":checkpoint}),
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
    /// List immutable savepoints belonging to a Session.
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
    /// Delete an unreferenced savepoint; workspace savepoints require a stopped Session.
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
        /// Omit to collect both stores; execution collection can run on a suspended Session.
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
    /// Import a rootfs into the Session's immutable base store.
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
    pub(super) fn preflight(&self, record: &RunRecord) -> anyhow::Result<()> {
        // Execution deletion has its own Session/Execution and retained-reference
        // fences below. This classification is not mutation authorization:
        // workspace fallback must still acquire stopped-Execution ownership.
        let execution_delete = match &self.command {
            CheckpointCommand::Delete { id, .. } => {
                Session::read(record)?.is_some_and(|session| session.checkpoints().contains_key(id))
            }
            _ => false,
        };
        if matches!(
            &self.command,
            CheckpointCommand::Create {
                kind: Kind::Workspace,
                ..
            } | CheckpointCommand::Delete { .. }
                | CheckpointCommand::Gc { .. }
                | CheckpointCommand::ImportBase { .. }
        ) && record
            .live_control
            .as_ref()
            .is_some_and(|endpoint| endpoint.is_available())
            && !execution_delete
        {
            anyhow::bail!(
                "Session owner still holds stage; persistent checkpoint mutation refused"
            );
        }
        if matches!(
            &self.command,
            CheckpointCommand::Create {
                kind: Kind::Execution,
                ..
            }
        ) {
            check_execution(record)?;
        }
        Ok(())
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

/// Check the Session configuration before admitting native execution checkpoints.
/// Platform support alone does not guarantee that this Session can be restored.
pub(crate) fn execution_blocker(record: &RunRecord) -> Option<String> {
    pvisor::session_service::execution_blocker(record)
}
pub(super) fn check_execution(record: &RunRecord) -> anyhow::Result<()> {
    pvisor::session_service::require_execution(record)
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
                let response = capture(
                    &selected,
                    false,
                    ram_storage
                        .map(Into::into)
                        .unwrap_or(SnapshotRamStorage::Compressed),
                    request_id.clone(),
                    timeout.0,
                )
                .await?;
                let checkpoint = response.checkpoint;
                return emit(
                    json,
                    serde_json::json!({"schema_version":1,"operation":"checkpoint.create","session_id":selected.run_id,"request_id":request_id,"checkpoint_id":checkpoint.snapshot_id,"kind":"execution","checkpoint":checkpoint}),
                );
            }
            let (checkpoint, reused) =
                pvisor::session_service::SessionService::create_selected_workspace_checkpoint(
                    &super::host::service_context(),
                    &selected,
                    request_id.as_deref(),
                )?;
            emit(
                json,
                serde_json::json!({
                    "schema_version": 1, "operation": "checkpoint.create", "session_id": selected.run_id,
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
                && let Some(session) = Session::read(&record)?
            {
                checkpoints.extend(session.checkpoints().values().map(|capture| serde_json::json!({"checkpoint_id":capture.checkpoint.snapshot_id,"kind":"execution","checkpoint":capture.checkpoint,"branch_references":capture.branches})));
            }
            emit(
                json,
                serde_json::json!({"schema_version":1,"operation":"checkpoint.list",
                    "session_id":record.run_id,"kind_filter":kind,"execution_blocker":execution_blocker(&record),"checkpoints":checkpoints}),
            )
        }
        CheckpointCommand::Show {
            selection,
            id,
            json,
        } => {
            let record = selection.resolve()?;
            if let Some(session) = Session::read(&record)?
                && let Some(capture) = session.checkpoints().get(&id)
            {
                return emit(
                    json,
                    serde_json::json!({"schema_version":1,"operation":"checkpoint.show","session_id":record.run_id,"kind":"execution","checkpoint":capture.checkpoint,"branch_references":capture.branches,"suspended_head":session.head()==Some(&id)}),
                );
            }
            // Keep the source manifest alive while computing reference information.
            let (record, _lease) = record.lock_current()?;
            let checkpoint = resolve_checkpoint(&record, &id)?;
            let refs = checkpoint_branch_refs(&checkpoint)?;
            emit(
                json,
                serde_json::json!({"schema_version":1,"operation":"checkpoint.show",
                "session_id":record.run_id,"kind":"workspace","branch_references":refs,
                "checkpoint":checkpoint}),
            )
        }
        CheckpointCommand::Delete {
            selection,
            id,
            json,
        } => {
            let record = selection.resolve()?;
            if pvisor::session_service::SessionService::delete_selected_execution_checkpoint(
                &super::host::service_context(),
                &record,
                &id,
            )? {
                return emit(
                    json,
                    serde_json::json!({"schema_version":1,"operation":"checkpoint.delete","session_id":record.run_id,"kind":"execution","checkpoint_id":id,"deleted":true}),
                );
            }
            // Native lookup releases its lease on a miss; reacquire and revalidate
            // before the workspace fallback, which expects this fence.
            let _session = super::host::lock_selected_session(&record)?;
            let id = pvisor::session_service::SessionService::delete_selected_workspace_checkpoint(
                &super::host::service_context(),
                &record,
                &id,
            )?;
            emit(
                json,
                serde_json::json!({"schema_version":1,"operation":"checkpoint.delete",
                "session_id":record.run_id,"checkpoint_id":id,"deleted":true}),
            )
        }
        CheckpointCommand::Gc {
            selection,
            json,
            kind,
        } => {
            let record = selection.resolve()?;
            let template = Session::read(&record)?;
            let _session_lease = template.as_ref().map(Session::lock).transpose()?;
            let session = template.map(|session| session.current()).transpose()?;
            if let Some(session) = &session {
                session.validate_record_target(&record)?;
                let current = RunRecord::read(session.active_stage())?;
                super::host::check_selected_record(&record, &current)?;
                super::host_service::check_record(&current)?;
            }
            let scope = if session.is_some() {
                "session_checkpoint_transactions"
            } else {
                "session_workspace_transactions"
            };
            let removed = if kind == Some(Kind::Execution) {
                0
            } else {
                pvisor::session_service::SessionService::collect_selected_workspace_transactions(
                    &super::host::service_context(),
                    &record,
                )?
            };
            let mut execution_removed = 0;
            if kind != Some(Kind::Workspace)
                && let Some(session) = session
            {
                let mut stores = session.stores().clone();
                stores.insert(session.active_stage().join("execution-snapshots"));
                stores.extend(
                    session
                        .checkpoints()
                        .values()
                        .map(|capture| capture.checkpoint.store.clone()),
                );
                for path in stores {
                    execution_removed += checkpoint_store(&session, &path)?.collect_abandoned()?;
                }
            }
            emit(
                json,
                serde_json::json!({"schema_version":1,"operation":"checkpoint.gc",
                "session_id":record.run_id,"scope":scope,"kind_filter":kind,
                "root":record.stage_dir().join(pvisor::CHECKPOINTS_DIR),"removed_transactions":removed,
                "execution_removed_transactions":execution_removed,"published_checkpoints_deleted":0}),
            )
        }
        CheckpointCommand::Verify {
            selection,
            id,
            json,
        } => {
            let record = selection.resolve()?;
            let template = session_execution::session(&record)?;
            let _lease = template.lock()?;
            let session = template.current()?;
            session.validate_record_target(&record)?;
            super::host_service::check_record(&RunRecord::read(session.active_stage())?)?;
            let checkpoint = session.checkpoint(&id)?;
            let compatibility = pvisor::VmExecutor::checkpoint_compatibility(&session.config().vm)?;
            let object =
                checkpoint_store(&session, &checkpoint.store)?.open(&id, &compatibility)?;
            emit(
                json,
                serde_json::json!({"operation":"checkpoint.verify","session_id":session.run_id(),"checkpoint_id":id,"verified":true,"manifest":object.manifest()}),
            )
        }
        CheckpointCommand::ImportBase {
            selection,
            rootfs,
            json,
        } => {
            let record = selection.resolve()?;
            let (session, _lease) = super::host::lock_selected_session(&record)?
                .context("this Session has no native execution handoff")?;
            let rootfs = rootfs.canonicalize()?;
            super::host_service::reject_guest_exposure([rootfs.clone()])?;
            let base =
                checkpoint_store(&session, &session.primary_store()?)?.import_base(&rootfs)?;
            emit(
                json,
                serde_json::json!({"operation":"checkpoint.import_base","session_id":session.run_id(),"base":base.reference(),"rootfs":base.root()}),
            )
        }
        CheckpointCommand::VerifyBase {
            selection,
            id,
            json,
        } => {
            let record = selection.resolve()?;
            let session = session_execution::session(&record)?;
            let base = checkpoint_store(&session, &session.primary_store()?)?
                .open_base(&pvisor::environment_snapshot::BaseReference { id })?;
            emit(
                json,
                serde_json::json!({"operation":"checkpoint.verify_base","session_id":session.run_id(),"base":base.reference(),"verified":true}),
            )
        }
    }
}

fn checkpoint_store(
    session: &Session,
    path: &std::path::Path,
) -> anyhow::Result<pvisor::environment_snapshot::SnapshotStore> {
    match session.config().vm.snapshot_filesystem_pool.as_deref() {
        Some(pool) => pvisor::environment_snapshot::SnapshotStore::with_filesystem_pool(path, pool),
        None => pvisor::environment_snapshot::SnapshotStore::new(path),
    }
}

fn emit(json: bool, mut value: serde_json::Value) -> anyhow::Result<()> {
    value["schema_version"] = serde_json::json!(1);
    if json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!(
            "{}: Session {}",
            value["operation"].as_str().unwrap_or("checkpoint"),
            value["session_id"].as_str().unwrap_or("-")
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
