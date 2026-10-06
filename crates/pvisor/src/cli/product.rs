//! Product-facing review and logical checkpoint commands.

use crate::runtime::{RunRecord, resolve_run};
use crate::{ChangeEntryType, ChangeKind, RunBundle};
use anyhow::Context;
use clap::Args;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_STORAGE: &str = ".pvisor/capture";
const DEFAULT_DIFF_BYTES: usize = 256 * 1024;
const DEFAULT_DIFF_FILE_BYTES: u64 = 1024 * 1024;
const REVIEW_PATH_LIMIT: usize = 200;

#[derive(Debug, Clone, Args, serde::Serialize, serde::Deserialize)]
pub struct ReviewArgs {
    /// Job id, project workspace, run.json, or a path inside the Job filesystem.
    pub selector: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
    /// Review the staged files saved in this workspace checkpoint.
    #[arg(long)]
    pub checkpoint: Option<String>,
    /// Emit the complete versioned Run Bundle.
    #[arg(long)]
    pub json: bool,
    /// Show bounded unified text diffs after the classified change list.
    #[arg(long, conflicts_with = "json")]
    pub diff: bool,
    /// Maximum total bytes emitted by --diff.
    #[arg(long, default_value_t = DEFAULT_DIFF_BYTES, requires = "diff")]
    pub max_diff_bytes: usize,
    /// Skip content diff for any file larger than this many bytes.
    #[arg(long, default_value_t = DEFAULT_DIFF_FILE_BYTES, requires = "diff")]
    pub max_diff_file_bytes: u64,
}

pub fn review(args: ReviewArgs) -> anyhow::Result<()> {
    let selected = selected(args.selector.as_deref(), &args.output_dir)?;
    let (mut record, _lease) = selected.lock_current()?;
    let bundle_stage = record.stage_dir();
    let checkpoint_id = if let Some(id) = &args.checkpoint {
        let checkpoint = crate::runtime::checkpoint::resolve_checkpoint(&record, id)?;
        record = crate::runtime::checkpoint::workspace_view(&record, &checkpoint)?;
        Some(checkpoint.checkpoint_id)
    } else {
        record.require_stopped()?;
        None
    };
    let mut bundle = RunBundle::read(&bundle_stage).with_context(|| {
        format!(
            "Job {} has no readable Run Bundle; re-run it with this pVisor version",
            record.run_id
        )
    })?;
    // The Bundle's execution evidence is historical. Read the selected file
    // view again so a preceding apply/drop cannot leave review showing old files.
    if let Some(overlay) = &record.overlay {
        let status = crate::runtime::overlay_status(overlay)?;
        let filesystem = bundle
            .filesystem
            .as_mut()
            .context("Job Bundle has no workspace evidence")?;
        filesystem.state = overlay.state;
        filesystem.target = overlay.target.clone();
        filesystem.upper = overlay.upper.path().to_path_buf();
        filesystem.changed_files = status.changed_files;
        filesystem.whiteouts = status.whiteouts;
        filesystem.sample_paths = status.sample_paths;
        filesystem.changes = crate::runtime::overlay_changes(overlay, &record.overlay_lowers)?;
    }
    let context = serde_json::json!({"job_id":record.run_id,
        "attempt_id":record.attempt_id,"checkpoint_id":checkpoint_id,
        "workspace_generation":record.overlay.as_ref().map(|overlay| overlay.generation),
        "file_view":"staged_upper_with_external_lowers","execution_evidence":"historical_bundle"});
    if args.json {
        let mut result = serde_json::to_value(&bundle)?;
        result["review_context"] = context;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    println!("pVisor review — {}", bundle.run.run_id);
    println!("file view: {}", context);
    println!(
        "outcome: {:?} (exit {:?})",
        bundle.run.state, bundle.run.exit_code
    );
    println!("agent: {}", bundle.run.agent);
    println!("duration: {} ms", bundle.run.duration_ms);
    if bundle.run.parent_run_id.is_some() || bundle.run.task_id.is_some() {
        println!(
            "orchestration: parent={} task={}",
            bundle.run.parent_run_id.as_deref().unwrap_or("-"),
            bundle.run.task_id.as_deref().unwrap_or("-")
        );
    }
    for (key, value) in &bundle.orchestration {
        println!("  {key}: {value}");
    }
    if let Some(lineage) = &bundle.lineage {
        println!(
            "lineage: {} @ {}",
            lineage.parent_run_id, lineage.checkpoint_id
        );
    }

    println!("\nSafety boundary");
    println!(
        "  filesystem: {}",
        if bundle.safety.filesystem_non_bypassable {
            "kernel-enforced read/write roots with staged workspace"
        } else if bundle.safety.filesystem_write_non_bypassable {
            "kernel-enforced staged writes; reads remain ambient"
        } else if bundle.safety.filesystem_changes_staged {
            "changes staged for review"
        } else {
            "no staged change set"
        }
    );
    println!(
        "  network: {}",
        if bundle.safety.network_non_bypassable {
            "non-bypassable enforcement"
        } else if bundle.network.interception.is_some() {
            "cooperative proxy coverage"
        } else {
            "host network"
        }
    );
    println!(
        "  process: {}",
        match bundle
            .run
            .executor
            .as_ref()
            .map(|executor| executor.isolation)
        {
            Some(pvisor_core::IsolationKind::RootlessProcess)
                if bundle.safety.filesystem_non_bypassable =>
                "rootless user namespace + Landlock",
            Some(pvisor_core::IsolationKind::RootlessProcess) => {
                "rootless user namespace + network namespace"
            }
            Some(pvisor_core::IsolationKind::SandboxedProcess)
                if bundle.safety.filesystem_write_non_bypassable =>
                "macOS Seatbelt filesystem policy",
            Some(pvisor_core::IsolationKind::SandboxedProcess) => {
                "macOS Seatbelt network policy"
            }
            Some(pvisor_core::IsolationKind::Container) => {
                "OCI container with injected pVisor"
            }
            Some(pvisor_core::IsolationKind::VirtualMachine) => {
                "pvisor-vm guest (KVM on Linux, HVF on macOS) over the pVisor root OverlayFS"
            }
            _ => "host process (not a host-isolation boundary)",
        }
    );
    for warning in &bundle.safety.warnings {
        println!("  warning: {warning}");
    }
    if let Some(metrics) = &bundle.network.intercepted {
        println!(
            "  observed: {} proxy requests; {} allowed, {} denied, {} failures; {} VM TCP flows",
            metrics.requests_seen,
            metrics.policy_allowed,
            metrics.policy_denied + metrics.tcp_flows_denied,
            metrics.failures + metrics.tcp_connect_failures,
            metrics.tcp_flows_opened
        );
        println!("\nNetwork access observations (only traffic reaching OverlayNet)");
        let mut targets = metrics.targets.iter().collect::<Vec<_>>();
        targets.sort_by(|left, right| {
            (right.1.denied, right.1.failed, right.1.allowed)
                .cmp(&(left.1.denied, left.1.failed, left.1.allowed))
                .then_with(|| left.0.cmp(right.0))
        });
        for (target, counts) in targets.iter().take(REVIEW_PATH_LIMIT) {
            println!(
                "  {target}: policy-allowed={} denied={} failed={}",
                counts.allowed, counts.denied, counts.failed
            );
        }
        if metrics.target_overflow > 0 || targets.len() > REVIEW_PATH_LIMIT {
            println!(
                "  {} destinations hidden by display limit; {} events omitted by capture limit; use --json for retained details",
                targets.len().saturating_sub(REVIEW_PATH_LIMIT),
                metrics.target_overflow
            );
        }
    }

    if let Some(observed) = bundle
        .run_observation
        .as_ref()
        .and_then(|observation| observation.filesystem.as_ref())
    {
        println!("\nFile access observations (only operations reaching OverlayFS)");
        let mut paths = observed.paths.iter().collect::<Vec<_>>();
        paths.sort_by(|left, right| {
            let priority =
                |operations: &BTreeMap<_, pvisor_core::operation::PathOperationCounters>| {
                    operations.values().fold((0u64, 0u64, 0u64), |sum, counts| {
                        (
                            sum.0 + counts.denied,
                            sum.1 + counts.effects,
                            sum.2 + counts.hits,
                        )
                    })
                };
            priority(right.1)
                .cmp(&priority(left.1))
                .then_with(|| left.0.cmp(right.0))
        });
        for (path, operations) in paths.iter().take(REVIEW_PATH_LIMIT) {
            for (operation, counts) in operations.iter() {
                println!(
                    "  {} {}: hits={} succeeded={} denied={} failed={} effects={} uncertain_effects={}",
                    path,
                    operation,
                    counts.hits,
                    counts.succeeded,
                    counts.denied,
                    counts.failed,
                    counts.effects,
                    counts.uncertain_effects
                );
            }
        }
        if observed.paths.len() > REVIEW_PATH_LIMIT || observed.overflow_hits > 0 {
            println!(
                "  {} more paths; {} observations omitted by the path limit; use --json for details",
                observed.paths.len().saturating_sub(REVIEW_PATH_LIMIT),
                observed.overflow_hits
            );
        }
    }

    println!("\nChanges");
    if let Some(filesystem) = &bundle.filesystem {
        println!(
            "  {} changed paths, {} deletions/whiteouts",
            filesystem.changed_files, filesystem.whiteouts
        );
        println!("  target: {}", filesystem.target.display());
        let mut counts = BTreeMap::new();
        for change in &filesystem.changes {
            *counts.entry(change.kind).or_insert(0usize) += 1;
        }
        if !counts.is_empty() {
            println!(
                "  classified: {} added, {} modified, {} deleted, {} type-changed, {} opaque",
                counts.get(&ChangeKind::Added).copied().unwrap_or(0),
                counts.get(&ChangeKind::Modified).copied().unwrap_or(0),
                counts.get(&ChangeKind::Deleted).copied().unwrap_or(0),
                counts.get(&ChangeKind::TypeChanged).copied().unwrap_or(0),
                counts.get(&ChangeKind::Opaque).copied().unwrap_or(0),
            );
        }
        for change in filesystem.changes.iter().take(REVIEW_PATH_LIMIT) {
            let code = match change.kind {
                ChangeKind::Added => "A",
                ChangeKind::Modified => "M",
                ChangeKind::Deleted => "D",
                ChangeKind::TypeChanged => "T",
                ChangeKind::Opaque => "O",
            };
            let mode = change
                .mode
                .map(|mode| format!(" mode={mode:04o}"))
                .unwrap_or_default();
            println!("  {code} {}{mode}", change.path);
        }
        if filesystem.changes.len() > REVIEW_PATH_LIMIT {
            println!(
                "  … {} more paths; use --json for the complete manifest",
                filesystem.changes.len() - REVIEW_PATH_LIMIT
            );
        } else if filesystem.changes.is_empty() {
            for path in &filesystem.sample_paths {
                println!("  - {path}");
            }
        }
    } else {
        println!("  host filesystem; no transactional change set");
    }

    println!("\nObserved Agent state");
    println!("  AgentCtl clients: {}", bundle.agentctl.clients.len());
    println!("\nEnvironment and resources");
    println!(
        "  host environment inherited: {}",
        bundle.environment.inherits_host
    );
    println!(
        "  projected env keys: {}",
        if bundle.environment.projected_keys.is_empty() {
            "-".into()
        } else {
            bundle.environment.projected_keys.join(", ")
        }
    );
    println!(
        "  runtime-injected env keys: {}",
        if bundle.environment.runtime_injected_keys.is_empty() {
            "-".into()
        } else {
            bundle.environment.runtime_injected_keys.join(", ")
        }
    );
    println!(
        "  requested limits: {}",
        serde_json::to_string(&bundle.resources.requested)?
    );
    println!(
        "  effective limits: {}",
        serde_json::to_string(&bundle.resources.effective)?
    );
    if !bundle.resources.mechanisms.is_empty() {
        println!("  mechanisms: {}", bundle.resources.mechanisms.join(", "));
    }
    for limitation in &bundle.resources.limitations {
        println!("  limitation: {limitation}");
    }
    if let Some(failure) = &bundle.run.failure {
        println!("\nFailure\n  {:?}: {}", failure.kind, failure.message);
    }
    println!(
        "\nBundle: {}",
        RunBundle::path(&record.stage_dir()).display()
    );
    if bundle.filesystem.is_some() {
        println!("Next:");
        println!("  pvisor inspect {}", record.stage_dir().display());
        println!("  pvisor fork {} -- <agent>", record.stage_dir().display());
        println!("  pvisor apply {}", record.stage_dir().display());
        println!("  pvisor drop {}", record.stage_dir().display());
    }
    if args.diff {
        print_diffs(
            &record,
            &bundle,
            args.max_diff_bytes,
            args.max_diff_file_bytes,
        )?;
    }
    Ok(())
}

fn print_diffs(
    record: &RunRecord,
    bundle: &RunBundle,
    max_total_bytes: usize,
    max_file_bytes: u64,
) -> anyhow::Result<()> {
    let Some(filesystem) = &bundle.filesystem else {
        return Ok(());
    };
    let overlay = record
        .overlay
        .as_ref()
        .context("Run Bundle has a changeset but Run overlay metadata is missing")?;
    let lowers = if record.overlay_lowers.is_empty() {
        vec![overlay.target.clone()]
    } else {
        record.overlay_lowers.clone()
    };
    let mut remaining = max_total_bytes;
    println!("\nDiff");
    for change in &filesystem.changes {
        if remaining == 0 {
            println!("  … diff output truncated at {max_total_bytes} bytes");
            break;
        }
        let relative = safe_change_path(&change.relative_path())?;
        let old = lowers
            .iter()
            .map(|lower| lower.join(&relative))
            .find(|path| fs::symlink_metadata(path).is_ok());
        let new = overlay.upper.path().join(&relative);
        if change.kind == ChangeKind::Opaque {
            println!("opaque directory: {}", change.path);
            continue;
        }
        if change.old_type == Some(ChangeEntryType::Symlink)
            || change.new_type == Some(ChangeEntryType::Symlink)
        {
            println!(
                "symlink {}: {} -> {}",
                change.path,
                old.as_deref()
                    .and_then(read_link_label)
                    .unwrap_or_else(|| "-".into()),
                read_link_label(&new).unwrap_or_else(|| "-".into())
            );
            continue;
        }
        let old_file = old.as_deref().filter(|path| path.is_file());
        let new_file = new.is_file().then_some(new.as_path());
        if old_file.is_none() && new_file.is_none() {
            continue;
        }
        if [old_file, new_file]
            .into_iter()
            .flatten()
            .any(|path| fs::metadata(path).is_ok_and(|metadata| metadata.len() > max_file_bytes))
        {
            println!("binary/large {} (content diff skipped)", change.path);
            continue;
        }
        if [old_file, new_file]
            .into_iter()
            .flatten()
            .any(is_binary_file)
        {
            println!("binary {} (content diff skipped)", change.path);
            continue;
        }
        let old_arg = old_file.unwrap_or_else(|| Path::new("/dev/null"));
        let new_arg = new_file.unwrap_or_else(|| Path::new("/dev/null"));
        let output = Command::new("diff")
            .args(["-u", "--label"])
            .arg(format!("a/{}", change.path))
            .arg("--label")
            .arg(format!("b/{}", change.path))
            .arg("--")
            .arg(old_arg)
            .arg(new_arg)
            .output()
            .with_context(|| format!("render diff for {}", change.path))?;
        anyhow::ensure!(
            matches!(output.status.code(), Some(0 | 1)),
            "diff failed for {}: {}",
            change.path,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let keep = remaining.min(output.stdout.len());
        print!("{}", String::from_utf8_lossy(&output.stdout[..keep]));
        remaining -= keep;
    }
    Ok(())
}

fn safe_change_path(path: &Path) -> anyhow::Result<PathBuf> {
    use std::path::Component;
    anyhow::ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir)),
        "unsafe change path in Run Bundle: {}",
        path.display()
    );
    Ok(path.to_path_buf())
}

fn read_link_label(path: &Path) -> Option<String> {
    fs::read_link(path)
        .ok()
        .map(|target| target.display().to_string())
}

fn is_binary_file(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = fs::File::open(path) else {
        return true;
    };
    let mut prefix = [0_u8; 8192];
    let read = file.read(&mut prefix).unwrap_or(0);
    prefix[..read].contains(&0)
}

fn selected(selector: Option<&Path>, output_dir: &Path) -> anyhow::Result<RunRecord> {
    let storage = output_dir
        .canonicalize()
        .unwrap_or_else(|_| output_dir.to_path_buf());
    resolve_run(selector, &storage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_paths_cannot_escape_the_overlay_roots() {
        assert!(safe_change_path(Path::new("src/lib.rs")).is_ok());
        assert!(safe_change_path(Path::new("../host-secret")).is_err());
        assert!(safe_change_path(Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn binary_probe_detects_nul_bytes() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), b"text\0binary").unwrap();
        assert!(is_binary_file(temp.path()));
        fs::write(temp.path(), b"plain text\n").unwrap();
        assert!(!is_binary_file(temp.path()));
    }
}
