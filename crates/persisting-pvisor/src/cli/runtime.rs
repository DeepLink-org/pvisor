#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use clap::Args;

use crate::runtime::{
    ApplySelection, LEASE_FILENAME, OverlayState, ReadOnlyOverlayMount, RunLease, RunRecord,
    apply_overlay_selected, control_mount_inspect, control_overlay_status, control_ping,
    control_unmount_inspect, discard_overlay, is_live, load_apply_records,
    mount_overlay_record_read_only, overlay_status, resolve_run,
};

const DEFAULT_STORAGE: &str = ".persisting/capture";

#[derive(Debug, Clone, Args)]
pub struct StatusArgs {
    /// Job id, stage directory, upper directory, or workspace path.
    pub selector: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
    #[arg(long)]
    pub json: bool,
    /// Show the Job's Run Bundle, safety evidence, and staged changes.
    #[arg(long)]
    pub review: bool,
    /// Include bounded unified diffs in the review view.
    #[arg(long, conflicts_with = "json")]
    pub diff: bool,
    #[arg(long, default_value_t = 256 * 1024, requires = "diff")]
    pub max_diff_bytes: usize,
    #[arg(long, default_value_t = 1024 * 1024, requires = "diff")]
    pub max_diff_file_bytes: u64,
}

#[derive(Debug, Clone, Args)]
pub struct KillArgs {
    /// Live Job id, stage directory, or workspace path.
    pub selector: PathBuf,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct InspectArgs {
    /// Job id, stage directory, upper directory, or workspace path.
    pub selector: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
    /// Command to run in the read-only view; defaults to $SHELL or /bin/bash.
    #[arg(last = true, allow_hyphen_values = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct SelectArgs {
    /// Job id, stage directory, upper directory, or workspace path.
    pub selector: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct ApplyArgs {
    /// Job id, stage directory, upper directory, or workspace path.
    pub selector: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
    /// Apply staged changes here instead of the target recorded by the Job.
    #[arg(long, value_name = "PATH")]
    pub target: Option<PathBuf>,
    /// Apply this relative path and its descendants. Repeatable.
    #[arg(long = "path", value_name = "RELATIVE_PATH")]
    pub paths: Vec<PathBuf>,
    /// Include staged paths matching this glob. Repeatable.
    #[arg(long, value_name = "GLOB")]
    pub include: Vec<String>,
    /// Exclude staged paths matching this glob. Repeatable.
    #[arg(long, value_name = "GLOB")]
    pub exclude: Vec<String>,
    /// Explicitly apply every remaining staged change.
    #[arg(long)]
    pub all: bool,
}

pub fn status(args: StatusArgs) -> anyhow::Result<()> {
    if args.review || args.diff {
        return super::product::review(super::product::ReviewArgs {
            selector: args.selector,
            output_dir: args.output_dir,
            json: args.json,
            diff: args.diff,
            max_diff_bytes: args.max_diff_bytes,
            max_diff_file_bytes: args.max_diff_file_bytes,
        });
    }
    let record = selected(args.selector.as_deref(), &args.output_dir)?;
    let live = control_ping(&record.stage_dir()) || is_live(&record.stage_dir())?;
    let apply_history = load_apply_records(&record.stage_dir())?;
    let fs = record
        .overlay
        .as_ref()
        .map(|overlay| {
            if control_ping(&record.stage_dir()) {
                control_overlay_status(&record.stage_dir()).map(|status| FsSummary {
                    changed_files: status.changed_files,
                    whiteouts: status.whiteouts,
                    sample_paths: status.sample_paths,
                })
            } else {
                overlay_status(overlay)
                    .map(|status| FsSummary {
                        changed_files: status.changed_files,
                        whiteouts: status.whiteouts,
                        sample_paths: status.sample_paths,
                    })
                    .map_err(Into::into)
            }
        })
        .transpose()?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "run": record,
                "live": live,
                "apply_history": apply_history,
                "filesystem": fs.as_ref().map(|status| serde_json::json!({
                    "state": record.overlay.as_ref().map(|overlay| overlay.state),
                    "changed_files": status.changed_files,
                    "whiteouts": status.whiteouts,
                    "sample_paths": status.sample_paths,
                })),
            }))?
        );
        return Ok(());
    }

    println!("job: {}", record.run_id);
    println!("session: {}", record.session_id);
    let state = if live {
        "running"
    } else if record.state == "running" {
        "stale"
    } else {
        &record.state
    };
    println!("state: {state}");
    println!(
        "pid: {}{}",
        record.pid,
        if live { " (live)" } else { " (offline)" }
    );
    println!("agent: {}", record.agent);
    println!("command: {}", shell_join(&record.command));
    println!("stage: {}", record.stage_dir().display());
    if !apply_history.is_empty() {
        println!("apply batches: {}", apply_history.len());
    }
    println!("net: {}", serde_json::to_string(&record.network)?);
    println!(
        "overlaynet: {}",
        record.overlaynet_listen.as_deref().unwrap_or("disabled")
    );
    if let Some(interception) = &record.network_interception {
        println!(
            "network interception: {:?} ({:?}, enforcing={})",
            interception.driver,
            interception.strength,
            interception.is_enforcing()
        );
    }
    println!(
        "gateway: {}",
        record.gateway_listen.as_deref().unwrap_or("disabled")
    );
    if let Some(overlay) = &record.overlay {
        let fs = fs.context("OverlayFS status missing")?;
        println!("fs: {:?} (read-only inspect available)", overlay.state);
        println!("target: {}", overlay.target.display());
        println!("upper: {}", overlay.upper.path().display());
        println!(
            "changes: {} paths, {} whiteouts",
            fs.changed_files, fs.whiteouts
        );
    } else {
        println!("fs: host view (no OverlayFS workspace)");
    }
    Ok(())
}

pub fn kill(args: KillArgs) -> anyhow::Result<()> {
    let record = selected(Some(&args.selector), &args.output_dir)?;
    anyhow::ensure!(
        record.executor.is_some(),
        "{} is an environment, not a Job; use `pvisor env stop`",
        record.run_id
    );
    anyhow::ensure!(
        record.state == "running" && is_live(&record.stage_dir())?,
        "Job {} is not live",
        record.run_id
    );
    let pid = libc::pid_t::try_from(record.pid).context("Job PID does not fit pid_t")?;
    anyhow::ensure!(
        pid > 1 && pid != std::process::id() as libc::pid_t,
        "invalid Job PID {pid}"
    );
    #[cfg(target_os = "linux")]
    anyhow::ensure!(
        pid_holds_lease(pid, &record.stage_dir().join(LEASE_FILENAME))?,
        "Job {} PID {} no longer owns its storage lease",
        record.run_id,
        pid
    );
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("terminate Job {} (PID {pid})", record.run_id));
    }
    println!("requested termination of Job {} (PID {pid})", record.run_id);
    Ok(())
}

#[cfg(target_os = "linux")]
fn pid_holds_lease(pid: libc::pid_t, lease: &Path) -> anyhow::Result<bool> {
    let expected = std::fs::metadata(lease)?;
    for entry in std::fs::read_dir(format!("/proc/{pid}/fd"))? {
        if let Ok(actual) = std::fs::metadata(entry?.path())
            && actual.dev() == expected.dev()
            && actual.ino() == expected.ino()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

struct FsSummary {
    changed_files: usize,
    whiteouts: usize,
    sample_paths: Vec<String>,
}

pub fn inspect(args: InspectArgs) -> anyhow::Result<i32> {
    let record = selected(args.selector.as_deref(), &args.output_dir)?;
    let overlay = record
        .overlay
        .as_ref()
        .context("this Job has no OverlayFS workspace to inspect")?;
    let lowers = if record.overlay_lowers.is_empty() {
        vec![overlay.target.clone()]
    } else {
        record.overlay_lowers.clone()
    };
    let stage = record.stage_dir();
    let mount = if control_ping(&stage) {
        let (id, mountpoint) = control_mount_inspect(&stage)?;
        InspectMount::Remote {
            stage,
            id,
            mountpoint,
        }
    } else {
        let inspect_root = overlay.stage_dir.join("inspect").join(format!(
            "{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mountpoint = inspect_root.join("merged");
        let session = mount_overlay_record_read_only(overlay, &lowers, &mountpoint)
            .with_context(|| format!("mount read-only Job view at {}", mountpoint.display()))?;
        InspectMount::Local {
            inspect_root,
            session,
        }
    };

    let command = if args.command.is_empty() {
        vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())]
    } else {
        args.command
    };
    let (program, command_args) = command.split_first().context("missing inspect command")?;
    let status = Command::new(program)
        .args(command_args)
        .current_dir(mount.mountpoint())
        .env("PERSISTING_INSPECT", "1")
        .env("PERSISTING_RUN_ID", &record.run_id)
        .env("PERSISTING_OVERLAY_STAGE", &overlay.stage_dir)
        .status()
        .with_context(|| format!("execute inspect command `{program}`"));
    mount.close()?;
    let status = status?;
    Ok(status.code().unwrap_or(1))
}

enum InspectMount {
    Remote {
        stage: PathBuf,
        id: String,
        mountpoint: PathBuf,
    },
    Local {
        inspect_root: PathBuf,
        session: ReadOnlyOverlayMount,
    },
}

impl InspectMount {
    fn mountpoint(&self) -> &Path {
        match self {
            Self::Remote { mountpoint, .. } => mountpoint,
            Self::Local { session, .. } => session.mountpoint(),
        }
    }

    fn close(self) -> anyhow::Result<()> {
        match self {
            Self::Remote { stage, id, .. } => control_unmount_inspect(&stage, id),
            Self::Local {
                inspect_root,
                session,
            } => {
                session.unmount()?;
                let _ = std::fs::remove_dir(inspect_root);
                Ok(())
            }
        }
    }
}

pub fn apply(args: ApplyArgs) -> anyhow::Result<()> {
    if args.all && (!args.paths.is_empty() || !args.include.is_empty() || !args.exclude.is_empty())
    {
        bail!("--all cannot be combined with --path, --include, or --exclude");
    }
    let selection = ApplySelection {
        paths: args.paths,
        includes: args.include,
        excludes: args.exclude,
    };
    let select = SelectArgs {
        selector: args.selector,
        output_dir: args.output_dir,
    };
    mutate(select, true, args.target.as_deref(), Some(&selection))
}

pub fn drop_overlay(args: SelectArgs) -> anyhow::Result<()> {
    mutate(args, false, None, None)
}

fn mutate(
    args: SelectArgs,
    apply: bool,
    target: Option<&Path>,
    selection: Option<&ApplySelection>,
) -> anyhow::Result<()> {
    let mut record = selected(args.selector.as_deref(), &args.output_dir)?;
    if is_live(&record.stage_dir())? {
        bail!(
            "Job {} is still running; its upper cannot be {}",
            record.run_id,
            if apply { "applied" } else { "dropped" }
        );
    }
    let _lease = RunLease::acquire(&record.stage_dir())?;
    let mut overlay = record
        .overlay
        .take()
        .context("this Job has no OverlayFS workspace")?;
    // The final lower may be the Run-owned snapshot of the target. It is
    // still the base workspace, whereas any preceding lower is a composed
    // read-only layer whose changes cannot be applied to that workspace.
    let base_snapshot = record.storage.join(".overlay-lowers");
    if apply
        && (record.overlay_lowers.len() > 1
            || record.overlay_lowers.first().is_some_and(|lower| {
                lower != &overlay.target && !lower.starts_with(&base_snapshot)
            }))
    {
        bail!(
            "Job {} composes read-only layers above its base; apply is disabled until pVisor can materialize the complete merged diff",
            record.run_id
        );
    }
    match (apply, overlay.state) {
        (true, OverlayState::Applied) => {
            println!(
                "already applied {} → {}",
                record.run_id,
                overlay.target.display()
            );
            return Ok(());
        }
        (false, OverlayState::Discarded) => {
            println!(
                "remaining staged changes already dropped for {}",
                record.run_id
            );
            return Ok(());
        }
        (false, OverlayState::Applied) => {
            bail!(
                "Job {} was already applied; drop cannot undo changes written to {}",
                record.run_id,
                overlay.target.display()
            );
        }
        (true, OverlayState::Discarded) => {
            bail!(
                "Job {} was already dropped; apply cannot recover discarded changes",
                record.run_id
            );
        }
        _ => {}
    }
    if apply {
        if overlay.target == Path::new("/") {
            bail!(
                "Job {} is a full-root libkrun changeset; fork it or drop it instead of applying it to the host root",
                record.run_id
            );
        }
        if let Some(target) = target {
            let target = resolve_apply_target(target, &record.stage_dir())?;
            overlay.target = target.clone();
            if let Some(primary_lower) = record.overlay_lowers.first_mut() {
                *primary_lower = target;
            } else {
                record.overlay_lowers.push(target);
            }
        }
        let lower_dirs = if record.overlay_lowers.is_empty() {
            vec![overlay.target.clone()]
        } else {
            record.overlay_lowers.clone()
        };
        let outcome = apply_overlay_selected(
            &mut overlay,
            &lower_dirs,
            selection.expect("apply always supplies a selection"),
        )?;
        println!(
            "applied {} changes from {} → {} (apply_id={}, remaining={})",
            outcome.applied.len(),
            record.run_id,
            overlay.target.display(),
            outcome.apply_id,
            outcome.remaining.len()
        );
    } else {
        discard_overlay(&mut overlay)?;
        println!("dropped remaining staged changes for {}", record.run_id);
    }
    record.overlay = Some(overlay);
    record.write()?;
    Ok(())
}

fn resolve_apply_target(target: &Path, stage: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(target)
        .with_context(|| format!("create apply target {}", target.display()))?;
    let target = target
        .canonicalize()
        .with_context(|| format!("resolve apply target {}", target.display()))?;
    let stage = stage.canonicalize().unwrap_or_else(|_| stage.to_path_buf());
    if target.starts_with(&stage) || stage.starts_with(&target) {
        bail!(
            "apply target must not overlap the pVisor stage: target={}, stage={}",
            target.display(),
            stage.display()
        );
    }
    Ok(target)
}

fn selected(selector: Option<&Path>, output_dir: &Path) -> anyhow::Result<RunRecord> {
    let storage = output_dir
        .canonicalize()
        .unwrap_or_else(|_| output_dir.to_path_buf());
    resolve_run(selector, &storage)
}

fn shell_join(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| {
            if part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "-._/".contains(ch))
            {
                part.clone()
            } else {
                format!("{:?}", part)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_join_quotes_only_when_needed() {
        assert_eq!(
            shell_join(&["rg".into(), "hello world".into()]),
            "rg \"hello world\""
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_must_hold_the_selected_run_lease() {
        let temporary = tempfile::tempdir().unwrap();
        let lease = RunLease::acquire(temporary.path()).unwrap();
        let path = temporary.path().join(LEASE_FILENAME);
        let pid = std::process::id() as libc::pid_t;
        assert!(pid_holds_lease(pid, &path).unwrap());
        drop(lease);
        assert!(!pid_holds_lease(pid, &path).unwrap());
    }
}
