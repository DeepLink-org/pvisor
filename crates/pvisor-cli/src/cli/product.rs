//! CLI review presentation and bounded filesystem diff rendering.

use anyhow::Context;
use clap::Args;
use pvisor::RunRecord;
use pvisor::{ChangeEntryType, ChangeKind, RunBundle};
use pvisor_core::overlay::ChangeEntry;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_STORAGE: &str = ".pvisor/capture";
const DEFAULT_DIFF_BYTES: usize = 256 * 1024;
const DEFAULT_DIFF_FILE_BYTES: u64 = 1024 * 1024;
const REVIEW_PATH_LIMIT: usize = 200;

#[derive(Debug, Clone, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewArgs {
    /// Job id, project workspace, run.json, a path inside the Job filesystem, or container name/id.
    pub selector: Option<PathBuf>,
    /// Select a container's exact staged bind guest path (never a Job selector).
    #[arg(
        long,
        value_name = "GUEST_PATH",
        requires = "selector",
        conflicts_with = "checkpoint"
    )]
    #[serde(default)]
    pub mount: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
    /// Review the staged files saved in this workspace checkpoint.
    #[arg(long)]
    pub checkpoint: Option<String>,
    /// Emit the complete versioned Run Bundle.
    #[arg(long)]
    pub json: bool,
    /// Show bounded project-only unified diffs; --json retains all environment changes.
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
    let selector = super::stage_source::resolve(
        args.selector.as_deref(),
        &args.output_dir,
        args.mount.as_deref(),
    )?;
    let response = pvisor::job_service::RuntimeJobService::review(
        &super::host::service_context(),
        pvisor::job_service::ReviewRequest {
            job: pvisor::job_service::JobSelection {
                selector,
                storage: args.output_dir,
            },
            checkpoint: args.checkpoint,
        },
    );
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            let Some(unknown) = error.downcast_ref::<pvisor::job_service::ReviewUnavailable>()
            else {
                return Err(error);
            };
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "state": "unknown",
                        "code": "COMPLETION_UNCOMMITTED",
                        "job_id": unknown.job_id,
                        "attempt_id": unknown.attempt_id,
                        "reason": unknown.reason,
                        "stage": unknown.stage,
                        "stage_retained": true,
                        "workspace_mutation_allowed": false,
                        "recovery": {"command": "status", "selector": unknown.stage, "json": true}
                    }))?
                );
            } else {
                println!("pVisor review — {}", unknown.job_id);
                println!("outcome: UNKNOWN");
                println!("reason: {}", unknown.reason);
                println!("stage retained: {}", unknown.stage.display());
                println!(
                    "recovery: run `pvisor status <selector> --json` with this stage or Job ID to reconcile available receipts"
                );
                println!(
                    "A missing receipt cannot establish success. Workspace mutation remains blocked."
                );
            }
            return Ok(());
        }
    };
    let (record, bundle, context) = response.view();
    let context = serde_json::to_value(context)?;
    let handoff = Handoff::new(record, bundle);
    if args.json {
        let mut result = serde_json::to_value(bundle)?;
        result["handoff"] = serde_json::to_value(&handoff)?;
        result["review_context"] = context;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    println!("pVisor review — {}", bundle.run.run_id);
    println!(
        "outcome: {:?} (exit {:?})",
        bundle.run.state, bundle.run.exit_code
    );
    if handoff.partial_results {
        println!(
            "warning: partial results may remain; failure/cancellation does not establish task completion."
        );
    }
    println!("file view: {}", context);
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

    print_project_results(&handoff);
    if args.diff {
        print_diffs(
            record,
            bundle,
            &handoff.project_changes,
            args.max_diff_bytes,
            args.max_diff_file_bytes,
        )?;
    }

    print_environment_changes(&handoff);
    println!("\nSafety boundary and observations");
    for limitation in &handoff.limitations {
        println!("  limitation: {limitation}");
    }
    println!(
        "  Classification is advisory, not a safety certification; project caches, generated files and build outputs remain project changes."
    );
    println!(
        "  Missing observations do not prove absence. Reads may expose host files/credentials; external API/database/message effects are not undone by drop."
    );
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

    if let Some(filesystem) = &bundle.filesystem {
        println!(
            "\nRetained filesystem: {:?}; target: {}",
            filesystem.state,
            filesystem.target.display()
        );
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
        if handoff.non_root_export_available {
            println!(
                "  pvisor export {} --output NEWDIR --path src",
                record.stage_dir().display()
            );
            println!(
                "  Review the export manifest before accepting artifacts; deletions are informational, not applied. Export revalidates eligibility and may refuse unsupported selections."
            );
        } else {
            println!(
                "  Project handoff unavailable; inspect the retained evidence and limitations before selecting a next step."
            );
        }
        if handoff.project_prefix.as_deref() == Some(Path::new(""))
            || record.project_apply_scope.is_some()
        {
            println!(
                "  pvisor apply {} --path <project-relative-path> --dry-run  # preview; omit --dry-run to accept",
                record.stage_dir().display()
            );
            println!(
                "  pvisor apply {}  # interactive project picker; unselected environment changes stay staged",
                record.stage_dir().display()
            );
        }
        println!(
            "  Selection does not include dependency closure; stop concurrent writers before accepting changes. Rootfs system publication is a separate advanced operation."
        );
        println!("  pvisor drop {}", record.stage_dir().display());
    }
    Ok(())
}

// This is a presentation-only partition of the existing manifest. It must not
// resolve symlinks (in particular, workspace aliases into an upper directory).
#[derive(serde::Serialize)]
struct Handoff<'a> {
    schema_version: u32,
    project_root: Option<PathBuf>,
    project_prefix: Option<PathBuf>,
    classification_available: bool,
    project_changes: Vec<&'a ChangeEntry>,
    environment_changes: Vec<&'a ChangeEntry>,
    unclassified_changes: Vec<&'a ChangeEntry>,
    unsupported_project_changes: Vec<&'a ChangeEntry>,
    counts: HandoffCounts,
    non_root_export_available: bool,
    partial_results: bool,
    limitations: Vec<&'static str>,
}

#[derive(serde::Serialize)]
struct HandoffCounts {
    // None means unavailable, not an empty changeset.
    project: Option<usize>,
    environment: Option<usize>,
    unsupported_project: Option<usize>,
    unclassified: usize,
}

fn lexical_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|component| {
            matches!(
                component,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
}

impl<'a> Handoff<'a> {
    fn new(record: &RunRecord, bundle: &'a RunBundle) -> Self {
        let mut result = Self {
            schema_version: 1,
            project_root: record.workspace.clone(),
            project_prefix: None,
            classification_available: false,
            project_changes: Vec::new(),
            environment_changes: Vec::new(),
            unclassified_changes: Vec::new(),
            unsupported_project_changes: Vec::new(),
            counts: HandoffCounts {
                project: None,
                environment: None,
                unsupported_project: None,
                unclassified: 0,
            },
            non_root_export_available: false,
            partial_results: matches!(
                bundle.run.state,
                pvisor_core::RunState::Failed | pvisor_core::RunState::Cancelled
            ) || bundle.run.exit_code.is_some_and(|code| code != 0),
            limitations: vec![
                "Advisory lexical classification only, not a safety certification or export authorization. Project caches, generated files and build outputs remain project changes.",
                "Change paths remain relative to filesystem.target; remove project_prefix for project-relative export selections. No paths are canonicalized by this summary.",
                "Export revalidates platform, stage lifecycle, source paths, metadata and selection dependencies; availability here is provisional, not a promise that export will succeed.",
                "Export is an artifact inventory, not a merged snapshot or apply: deletions/directories are manifest-only; metadata and unselected dependencies are not reproduced.",
                "Selective acceptance may conflict with concurrent project edits or omit dependencies. External API/database/message effects are not undone by drop; missing observations do not prove absence.",
            ],
        };
        let Some(filesystem) = &bundle.filesystem else {
            result.limitations.push("No retained filesystem changeset; project changes are unavailable, not known empty.");
            return result;
        };
        let project = record
            .workspace
            .as_deref()
            .filter(|path| lexical_absolute(path) && *path != Path::new("/"));
        let controls = [record.stage_dir(), filesystem.upper.clone()];
        let project =
            project.filter(|path| !controls.iter().any(|control| path.starts_with(control)));
        if let Some(project) = project {
            if filesystem.target == Path::new("/") {
                result.project_prefix = project.strip_prefix("/").ok().map(Path::to_path_buf);
            } else if filesystem.target == project {
                result.project_prefix = Some(PathBuf::new());
            }
        }
        let manifest_available = !(filesystem.changes.is_empty()
            && (filesystem.changed_files > 0 || filesystem.whiteouts > 0));
        result.classification_available = result.project_prefix.is_some() && manifest_available;
        if !result.classification_available {
            result.limitations.push("Project classification unavailable: requires a recorded non-root lexical absolute workspace matching the stage target or a root-stage prefix, and a complete manifest. Counts are unknown, not zero.");
            result.unclassified_changes.extend(&filesystem.changes);
        } else {
            let prefix = result.project_prefix.as_deref().expect("checked prefix");
            for change in &filesystem.changes {
                let path = change.relative_path();
                if path.as_os_str().is_empty() || safe_change_path(&path).is_err() {
                    result.unclassified_changes.push(change);
                    continue;
                }
                let absolute = filesystem.target.join(&path);
                if path.starts_with(prefix)
                    && !controls.iter().any(|control| absolute.starts_with(control))
                {
                    result.project_changes.push(change);
                } else {
                    result.environment_changes.push(change);
                }
            }
            let opaque_ancestor = filesystem.changes.iter().any(|change| {
                change.kind == ChangeKind::Opaque && prefix.starts_with(change.relative_path())
            });
            for change in &result.project_changes {
                let unsupported = opaque_ancestor
                    || matches!(change.kind, ChangeKind::Opaque | ChangeKind::TypeChanged)
                    || matches!(
                        change.old_type,
                        Some(ChangeEntryType::Symlink | ChangeEntryType::Other)
                    )
                    || matches!(
                        change.new_type,
                        Some(ChangeEntryType::Symlink | ChangeEntryType::Other)
                    )
                    || change.relative_path().to_str().is_none()
                    || (change.kind != ChangeKind::Deleted && change.new_type.is_none());
                if unsupported {
                    result.unsupported_project_changes.push(*change);
                }
            }
            if opaque_ancestor {
                result.limitations.push("Project scope has an opaque ancestor; project export is currently unsupported.");
            }
            result.counts.project = Some(result.project_changes.len());
            result.counts.environment = Some(result.environment_changes.len());
            result.counts.unsupported_project = Some(result.unsupported_project_changes.len());
            result.non_root_export_available = cfg!(target_os = "linux")
                && !result.project_changes.is_empty()
                && filesystem.state == pvisor_core::overlay::OverlayState::Staged
                && record.overlay.is_some()
                && !opaque_ancestor;
        }
        result.counts.unclassified = result.unclassified_changes.len();
        if !result.non_root_export_available {
            result.limitations.push("Non-root project export unavailable for this scope, platform or stage state; retain evidence for inspection.");
        }
        result
    }
}

fn print_project_results(handoff: &Handoff<'_>) {
    println!("\nProject results");
    if handoff.classification_available {
        println!(
            "  project root: {}",
            handoff.project_root.as_deref().unwrap().display()
        );
        println!(
            "  {} project changes; {} currently unsupported for export",
            handoff.project_changes.len(),
            handoff.unsupported_project_changes.len()
        );
        for change in handoff.project_changes.iter().take(REVIEW_PATH_LIMIT) {
            let relative = change.relative_path();
            let path = relative
                .strip_prefix(handoff.project_prefix.as_deref().unwrap())
                .unwrap_or(&relative);
            let unsupported = handoff.unsupported_project_changes.contains(change);
            println!(
                "  {:?} {}{}",
                change.kind,
                path.display(),
                if unsupported {
                    " [export unsupported]"
                } else {
                    ""
                }
            );
        }
        if handoff.project_changes.len() > REVIEW_PATH_LIMIT {
            println!("  … more project paths; use --json for the complete manifest");
        }
    } else {
        println!("  unavailable (not known empty); see limitations and review --json");
    }
}

fn print_environment_changes(handoff: &Handoff<'_>) {
    println!("\nExperimental environment changes");
    if handoff.classification_available {
        println!(
            "  {} paths outside the recorded project scope; not included in project diff",
            handoff.environment_changes.len()
        );
    } else {
        println!(
            "  classification unavailable; {} retained manifest entries unclassified",
            handoff.unclassified_changes.len()
        );
    }
    println!(
        "  Use pvisor review SELECTOR --json for all filesystem.changes, including environment changes."
    );
    println!(
        "  non-root project export available (provisional): {}",
        handoff.non_root_export_available
    );
}

fn print_diffs(
    record: &RunRecord,
    bundle: &RunBundle,
    changes: &[&ChangeEntry],
    max_total_bytes: usize,
    max_file_bytes: u64,
) -> anyhow::Result<()> {
    if bundle.filesystem.is_none() || changes.is_empty() {
        return Ok(());
    }
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
    println!("\nProject diff (environment changes: use review --json)");
    for change in changes {
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
