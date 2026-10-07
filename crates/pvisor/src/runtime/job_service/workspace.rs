use super::{
    JobSelection, RuntimeJobService, ServiceContext, check_selected_record, lock_selected_job,
    validate_request_id,
};
use crate::runtime::RunRecord;
use crate::runtime::checkpoint::{
    checkpoint_branch_refs, create_stopped_checkpoint_locked, list_checkpoints, resolve_checkpoint,
};
use anyhow::Context;

pub struct WorkspaceCreateRequest {
    pub job: JobSelection,
    pub request_id: Option<String>,
}
#[derive(Debug, serde::Serialize)]
pub struct WorkspaceCreateResponse {
    pub checkpoint: crate::LogicalCheckpoint,
    pub reused: bool,
}
pub struct WorkspaceDeleteRequest {
    pub job: JobSelection,
    pub checkpoint_id: String,
}
pub struct WorkspaceGcRequest {
    pub job: JobSelection,
}
impl RuntimeJobService {
    pub fn create_workspace_checkpoint(
        context: &ServiceContext<'_>,
        request: WorkspaceCreateRequest,
    ) -> anyhow::Result<WorkspaceCreateResponse> {
        let selected = Self::resolve(context, &request.job)?;
        let (checkpoint, reused) =
            create_fenced_workspace_request(context, &selected, request.request_id.as_deref())?;
        Ok(WorkspaceCreateResponse { checkpoint, reused })
    }
    pub fn delete_workspace_checkpoint(
        context: &ServiceContext<'_>,
        request: WorkspaceDeleteRequest,
    ) -> anyhow::Result<String> {
        let selected = Self::resolve(context, &request.job)?;
        let _job = lock_selected_job(context, &selected)?;
        Self::delete_selected_workspace_checkpoint(context, &selected, &request.checkpoint_id)
    }
    pub fn collect_workspace_transactions(
        context: &ServiceContext<'_>,
        request: WorkspaceGcRequest,
    ) -> anyhow::Result<usize> {
        let selected = Self::resolve(context, &request.job)?;
        let _job = lock_selected_job(context, &selected)?;
        collect_fenced_workspace_transactions(context, &selected)
    }
    pub(crate) fn create_selected_workspace_checkpoint(
        context: &ServiceContext<'_>,
        selected: &RunRecord,
        request_id: Option<&str>,
    ) -> anyhow::Result<(crate::LogicalCheckpoint, bool)> {
        create_fenced_workspace_request(context, selected, request_id)
    }
    pub(crate) fn delete_selected_workspace_checkpoint(
        context: &ServiceContext<'_>,
        selected: &RunRecord,
        id: &str,
    ) -> anyhow::Result<String> {
        delete_workspace_checkpoint(context, selected, id)
    }
    // The mixed workspace/execution CLI operation already owns the Job lease.
    pub(crate) fn collect_selected_workspace_transactions(
        context: &ServiceContext<'_>,
        selected: &RunRecord,
    ) -> anyhow::Result<usize> {
        collect_fenced_workspace_transactions(context, selected)
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceReceipt {
    schema_version: u32,
    job_id: String,
    checkpoint_id: String,
}

fn create_fenced_workspace_request(
    context: &ServiceContext<'_>,
    selected: &RunRecord,
    request_id: Option<&str>,
) -> anyhow::Result<(crate::LogicalCheckpoint, bool)> {
    let _job = lock_selected_job(context, selected)?;
    let (record, _lease) = selected.lock_current()?;
    check_selected_record(selected, &record)?;
    context.check(&record)?;
    record.require_stopped()?;
    let Some(key) = request_id else {
        return Ok((create_stopped_checkpoint_locked(&record, None)?, false));
    };
    validate_request_id(key)?;
    use sha2::Digest;
    let id = format!(
        "request-{}",
        crate::util::encode_hex(&sha2::Sha256::digest(key.as_bytes()))
    );
    let requests = record
        .stage_dir()
        .join(crate::CHECKPOINTS_DIR)
        .join(".requests");
    let receipt = requests.join(format!("{id}.json"));
    if receipt.try_exists()? {
        let prior: WorkspaceReceipt = serde_json::from_slice(&std::fs::read(&receipt)?)?;
        anyhow::ensure!(
            prior.schema_version == 1 && prior.job_id == record.run_id && prior.checkpoint_id == id,
            "workspace request receipt identity mismatch"
        );
        let cp = resolve_checkpoint(&record, &id).map_err(|e| anyhow::anyhow!(
            "request already committed; its checkpoint is no longer available; use a new request id: {e}"))?;
        return Ok((cp, true));
    }
    let prior = list_checkpoints(&record)?
        .into_iter()
        .find(|cp| cp.checkpoint_id == id);
    let reused = prior.is_some();
    let checkpoint = match prior {
        Some(cp) => cp,
        None => create_stopped_checkpoint_locked(&record, Some(&id))?,
    };
    crate::util::create_dir_all_durable(&requests)?;
    crate::util::write_private_json(
        &receipt,
        &WorkspaceReceipt {
            schema_version: 1,
            job_id: record.run_id,
            checkpoint_id: id,
        },
    )?;
    Ok((checkpoint, reused))
}

// Caller holds the Job lease; the stage lease protects the manifest and transaction.
fn delete_workspace_checkpoint(
    context: &ServiceContext<'_>,
    selected: &RunRecord,
    id: &str,
) -> anyhow::Result<String> {
    let (current, _lease) = selected.lock_current()?;
    check_selected_record(selected, &current)?;
    context.check(&current)?;
    current.require_stopped()?;
    let checkpoint = resolve_checkpoint(&current, id)?;
    anyhow::ensure!(
        checkpoint_branch_refs(&checkpoint)? == 0,
        "CHECKPOINT_REFERENCED: checkpoint {} is retained by a child Job",
        checkpoint.checkpoint_id
    );
    let manifest = checkpoint.manifest_path();
    let root = manifest.parent().context("checkpoint manifest parent")?;
    let parent = root.parent().context("checkpoint parent")?;
    let tombstone = parent.join(format!(".deleted-{}", uuid::Uuid::new_v4().simple()));
    std::fs::rename(root, &tombstone)?;
    crate::util::sync_directory(parent)?;
    std::fs::remove_dir_all(tombstone)?;
    crate::util::sync_directory(parent)?;
    Ok(checkpoint.checkpoint_id)
}

fn collect_fenced_workspace_transactions(
    context: &ServiceContext<'_>,
    selected: &RunRecord,
) -> anyhow::Result<usize> {
    let (current, _lease) = selected.lock_current()?;
    check_selected_record(selected, &current)?;
    context.check(&current)?;
    current.require_stopped()?;
    let root = current.stage_dir().join(crate::CHECKPOINTS_DIR);
    if !root.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if (name.starts_with(".pending-") || name.starts_with(".deleted-"))
            && entry.file_type()?.is_dir()
        {
            if let Ok(cp) = crate::LogicalCheckpoint::read(&entry.path()) {
                anyhow::ensure!(
                    cp.checkpoint_id != name,
                    "refuse to collect a legacy committed checkpoint named {name}"
                );
            }
            std::fs::remove_dir_all(entry.path())?;
            removed += 1;
        }
    }
    crate::util::sync_directory(&root)?;
    Ok(removed)
}
