//! Job-scoped checkpoint management. This never launches the legacy snapshot runner.
use crate::runtime::checkpoint::{
    checkpoint_branch_refs, collect_workspace_transactions, create_workspace_request,
    delete_checkpoint, list_checkpoints, resolve_checkpoint,
};
use crate::runtime::{RunRecord, resolve_run};
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    #[default]
    Workspace,
    Execution,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(super) enum RamStorage {
    Raw,
    Compressed,
}

#[derive(Debug, Args)]
pub(super) struct Selection {
    /// Explicit Job id, stage path, run.json, or last.
    pub job: PathBuf,
    #[arg(long, short = 'o', default_value = ".pvisor/capture")]
    pub output_dir: PathBuf,
}
impl Selection {
    pub fn resolve(&self) -> anyhow::Result<RunRecord> {
        resolve_run(Some(&self.job), &self.output_dir)
    }
}

#[derive(Debug, Args)]
pub(super) struct CheckpointArgs {
    #[command(subcommand)]
    command: CheckpointCommand,
}

#[derive(Debug, Args)]
pub(super) struct SuspendArgs {
    #[command(flatten)]
    selection: Selection,
    #[arg(long, value_enum)]
    ram_storage: Option<RamStorage>,
    #[arg(long)]
    request_id: Option<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub(super) struct ResumeArgs {
    #[command(flatten)]
    selection: Selection,
    #[arg(long)]
    tui: bool,
    #[arg(long)]
    request_id: Option<String>,
}

pub(super) fn suspend(args: SuspendArgs) -> anyhow::Result<()> {
    let record = args.selection.resolve()?;
    let _ = (args.ram_storage, args.request_id, args.json);
    // Capability rejection happens before state transitions, copying or freezing.
    reject_execution(&record)
}

pub(super) fn resume(args: ResumeArgs) -> anyhow::Result<()> {
    let record = args.selection.resolve()?;
    let _ = (args.tui, args.request_id);
    reject_execution(&record)
}
#[derive(Debug, Subcommand)]
enum CheckpointCommand {
    /// Save a stopped Job's staged files and conflict preimages.
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
    /// Clean abandoned workspace checkpoint transactions (Job-local scope).
    Gc {
        #[command(flatten)]
        selection: Selection,
        #[arg(long)]
        json: bool,
    },
}

/// Static capability for the ordinary Job executor, distinct from the legacy
/// independently owned full-copy VM profile. Never infer support from OS alone.
pub(crate) fn execution_blocker(record: &RunRecord) -> &'static str {
    if record
        .executor
        .as_ref()
        .is_some_and(|identity| identity.isolation == pvisor_core::IsolationKind::VirtualMachine)
    {
        "ordinary VM Jobs use temporary overlay root layers; their executor has no owned-layer checkpoint binding, full machine capture/restore or Attempt handoff"
    } else {
        "this Job executor has no full CPU/RAM/device capture and restore contract"
    }
}
pub(super) fn reject_execution(record: &RunRecord) -> anyhow::Result<()> {
    anyhow::bail!(
        "CAPABILITY_UNSUPPORTED: Job {}: {}",
        record.run_id,
        execution_blocker(record)
    )
}

pub(super) fn run(args: CheckpointArgs) -> anyhow::Result<()> {
    match args.command {
        CheckpointCommand::Create {
            selection,
            kind,
            ram_storage,
            request_id,
            json,
        } => {
            anyhow::ensure!(
                kind == Kind::Execution || ram_storage.is_none(),
                "--ram-storage is only valid for execution capture"
            );
            let selected = selection.resolve()?;
            if kind == Kind::Execution {
                return reject_execution(&selected);
            }
            let (checkpoint, reused) = create_workspace_request(&selected, request_id.as_deref())?;
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
            let checkpoints = list_checkpoints(&record)?;
            let checkpoints = if kind == Some(Kind::Execution) {
                Vec::new()
            } else {
                checkpoints
            };
            emit(
                json,
                serde_json::json!({"schema_version":1,"operation":"checkpoint.list",
                    "job_id":record.run_id,"kind_filter":kind,"supported_kinds":["workspace"],"checkpoints":checkpoints}),
            )
        }
        CheckpointCommand::Show {
            selection,
            id,
            json,
        } => {
            let record = selection.resolve()?;
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
            let id = delete_checkpoint(&record, &id)?;
            emit(
                json,
                serde_json::json!({"schema_version":1,"operation":"checkpoint.delete",
                "job_id":record.run_id,"checkpoint_id":id,"deleted":true}),
            )
        }
        CheckpointCommand::Gc { selection, json } => {
            let record = selection.resolve()?;
            let removed = collect_workspace_transactions(&record)?;
            emit(
                json,
                serde_json::json!({"schema_version":1,"operation":"checkpoint.gc",
                "job_id":record.run_id,"scope":"job_workspace_transactions",
                "root":record.stage_dir().join(crate::CHECKPOINTS_DIR),"removed_transactions":removed,
                "shared_content_store":null}),
            )
        }
    }
}
fn emit(json: bool, value: serde_json::Value) -> anyhow::Result<()> {
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
                    "  {} (workspace)",
                    cp["checkpoint_id"].as_str().unwrap_or("-")
                );
            }
        } else {
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
    }
    Ok(())
}
