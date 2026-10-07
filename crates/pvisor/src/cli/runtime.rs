#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(target_os = "linux")]
use crate::runtime::LEASE_FILENAME;
use anyhow::Context;
use clap::Args;

use crate::runtime::{
    ApplySelection, ReadOnlyOverlayMount, RunRecord, control_mount_inspect, control_ping,
    control_unmount_inspect, is_live, mount_overlay_record_read_only, resolve_run,
};

const DEFAULT_STORAGE: &str = ".pvisor/capture";

#[derive(Debug, Clone, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Clone, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KillArgs {
    /// Live Job id, stage directory, or workspace path.
    pub selector: PathBuf,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectArgs {
    /// Job id, stage directory, upper directory, or workspace path.
    pub selector: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
    /// Inspect the saved workspace of an immutable checkpoint.
    #[arg(long)]
    pub checkpoint: Option<String>,
    /// Command to run in the read-only view; defaults to $SHELL or /bin/bash.
    #[arg(last = true, allow_hyphen_values = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectArgs {
    /// Job id, stage directory, upper directory, or workspace path.
    pub selector: PathBuf,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
}

#[derive(Debug, Clone, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyArgs {
    /// Job id, stage directory, upper directory, or workspace path.
    pub selector: PathBuf,
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
            checkpoint: None,
        });
    }
    let response = crate::runtime::job_service::RuntimeJobService::status(
        &super::host::service_context(),
        crate::runtime::job_service::StatusRequest {
            job: crate::runtime::job_service::JobSelection {
                selector: args.selector,
                storage: args.output_dir,
            },
        },
    )?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&status_json(&response))?);
        return Ok(());
    }
    let crate::runtime::job_service::StatusResponse {
        record,
        live,
        execution_blocker: blocker,
        checkpoints,
        apply_history,
        filesystem: fs,
        filesystem_observation: file_observed,
        network_observation: net_observed,
        execution,
    } = response;
    println!("job: {}", record.run_id);
    println!("session: {}", record.session_id);
    let state = if live {
        "running"
    } else if record.state == crate::RunRecordState::Running {
        "stale"
    } else {
        record.state.as_str()
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
    println!("checkpoints: {} workspace", checkpoints.len());
    if let Some(blocker) = blocker {
        println!("execution checkpoint: unsupported ({blocker})");
    } else {
        println!("execution checkpoint: supported");
    }
    if let Some(job) = execution {
        println!(
            "execution: {} ({} checkpoints)",
            job["state"].as_str().unwrap_or("unknown"),
            job["checkpoints"].as_object().map_or(0, |v| v.len())
        );
        if let Some(head) = job["suspended_head"].as_str() {
            println!("suspended head: {head}");
        }
    }
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
    if let Some(observed) = &file_observed {
        let (hits, denied) = observed
            .paths
            .values()
            .flat_map(|operations| operations.values())
            .fold((0u64, 0u64), |sum, counts| {
                (sum.0 + counts.hits, sum.1 + counts.denied)
            });
        println!(
            "file accesses observed: {hits} operations, {denied} denied across {} paths ({} omitted)",
            observed.paths.len(),
            observed.overflow_hits
        );
        for (path, operations) in observed
            .paths
            .iter()
            .filter(|(_, operations)| operations.values().any(|counts| counts.denied > 0))
            .take(5)
        {
            let denied: u64 = operations.values().map(|counts| counts.denied).sum();
            println!("  denied {denied}: {path}");
        }
    }
    if let Some(observed) = &net_observed {
        println!(
            "network accesses observed: {} policy allowed, {} denied, {} transport failures across {} destinations ({} omitted)",
            observed.policy_allowed,
            observed.policy_denied + observed.tcp_flows_denied,
            observed.tcp_connect_failures + observed.failures,
            observed.targets.len(),
            observed.target_overflow
        );
        for (target, counts) in observed
            .targets
            .iter()
            .filter(|(_, counts)| counts.denied > 0)
            .take(5)
        {
            println!("  denied {}: {target}", counts.denied);
        }
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

fn status_json(response: &crate::runtime::job_service::StatusResponse) -> serde_json::Value {
    let record = &response.record;
    serde_json::json!({
        "run": record, "live": response.live,
        "checkpoint_capability": {"workspace": record.overlay.is_some(), "workspace_capture_requires": "confirmed_stopped", "execution": response.execution_blocker.is_none(), "execution_blocker": response.execution_blocker},
        "execution": response.execution, "checkpoints": response.checkpoints,
        "workspace_generation": record.overlay.as_ref().map(|o| o.generation),
        "apply_history": response.apply_history,
        "observations": {"filesystem": response.filesystem_observation, "network": response.network_observation},
        "filesystem": response.filesystem.as_ref().map(|fs| serde_json::json!({"state": record.overlay.as_ref().map(|o| o.state), "changed_files": fs.changed_files, "whiteouts": fs.whiteouts, "sample_paths": fs.sample_paths})),
    })
}

pub fn kill(args: KillArgs) -> anyhow::Result<()> {
    let record = selected(Some(&args.selector), &args.output_dir)?;
    super::host_service::check_record(&record)?;
    if crate::runtime::job_execution::terminate_suspended(&record)? {
        if args.json {
            println!(
                "{}",
                serde_json::json!({"schema_version":1,"operation":"kill","job_id":record.run_id,"state":"terminated","suspended_head_released":true})
            );
        } else {
            println!("terminated suspended Job {}", record.run_id);
        }
        return Ok(());
    }
    if record.state.is_stopped() {
        let _job = super::host::lock_selected_job(&record)?;
        let (current, _lease) = record.lock_current()?;
        super::host_service::check_record(&current)?;
        current.require_stopped()?;
        if args.json {
            println!(
                "{}",
                serde_json::json!({"schema_version":1,"operation":"kill",
                "job_id":current.run_id,"state":current.state,"already_stopped":true})
            );
        } else {
            println!("Job {} is already stopped", current.run_id);
        }
        return Ok(());
    }
    anyhow::ensure!(
        record.executor.is_some(),
        "{} is a legacy environment record, not an executable Job",
        record.run_id
    );
    anyhow::ensure!(
        record.state == crate::RunRecordState::Running && is_live(&record.stage_dir())?,
        "Job {} is not live",
        record.run_id
    );
    let cooperative = super::host_cancel::request(&record)?;
    if !cooperative {
        #[cfg(target_os = "linux")]
        {
            // Pin before inspecting the lease, then reread the durable identity
            // under the native Job lock. Never signal a recycled numeric PID.
            let template = crate::runtime::job_execution::Job::read(&record)?;
            let _job_lease = template
                .as_ref()
                .map(crate::runtime::job_execution::Job::lock)
                .transpose()?;
            if let Some(template) = template {
                template.current()?.validate_record_target(&record)?;
            }
            let pid = libc::pid_t::try_from(record.pid).context("Job PID does not fit pid_t")?;
            anyhow::ensure!(
                pid > 1 && pid != std::process::id() as libc::pid_t,
                "invalid Job PID {pid}"
            );
            let process = super::host_process::StableProcess::open(pid)?;
            anyhow::ensure!(
                pid_holds_lease(pid, &record.stage_dir().join(LEASE_FILENAME))?,
                "Job {} PID {} no longer owns its exclusive storage lease",
                record.run_id,
                pid
            );
            let current = crate::RunRecord::read(&record.stage_dir())?;
            super::host::check_selected_record(&record, &current)?;
            super::host_service::check_record(&current)?;
            anyhow::ensure!(
                current.pid == record.pid
                    && current.state == crate::RunRecordState::Running
                    && is_live(&current.stage_dir())?
                    && process.is_alive(),
                "Job process ownership changed; termination refused"
            );
            process.signal(libc::SIGTERM)?;
        }
        #[cfg(not(target_os = "linux"))]
        return Err(pvisor_core::host_protocol::AgentCtlHostError::new(
            pvisor_core::host_protocol::AgentCtlHostErrorCode::Unsupported,
            "no cooperative host cancellation endpoint; stable legacy process identity cannot be proven on this platform",
        ).into());
    }
    if args.json {
        println!(
            "{}",
            serde_json::json!({"schema_version":1,"operation":"kill",
            "job_id":record.run_id,"state":"stopping","termination_requested":true,"cooperative":cooperative})
        );
    } else {
        println!(
            "requested termination of Job {} (PID {})",
            record.run_id, record.pid
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn pid_holds_lease(pid: libc::pid_t, lease: &Path) -> anyhow::Result<bool> {
    let expected = std::fs::metadata(lease)?;
    for entry in std::fs::read_dir(format!("/proc/{pid}/fd"))? {
        let entry = entry?;
        if let Ok(actual) = std::fs::metadata(entry.path())
            && actual.dev() == expected.dev()
            && actual.ino() == expected.ino()
        {
            let name = entry.file_name();
            let info =
                std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{}", name.to_string_lossy()))?;
            let owner = pid.to_string();
            if info
                .lines()
                .filter(|line| line.starts_with("lock:"))
                .any(|line| {
                    let fields: Vec<_> = line.split_whitespace().collect();
                    fields
                        .windows(4)
                        .any(|fields| fields == ["FLOCK", "ADVISORY", "WRITE", owner.as_str()])
                })
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub fn inspect(args: InspectArgs) -> anyhow::Result<i32> {
    let selected = selected(args.selector.as_deref(), &args.output_dir)?;
    let _job = super::host::lock_selected_job(&selected)?;
    let (mut record, _lease) = selected.lock_current()?;
    super::host_service::check_record(&record)?;
    if let Some(id) = &args.checkpoint {
        let checkpoint = crate::runtime::checkpoint::resolve_checkpoint(&record, id)?;
        record = crate::runtime::checkpoint::workspace_view(&record, &checkpoint)?;
    } else {
        record.require_stopped()?;
    }
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
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    super::host_service::check_cancelled()?;
    let mut child = Command::new(program)
        .args(command_args)
        .current_dir(mount.mountpoint())
        .env("PVISOR_INSPECT", "1")
        .env("PVISOR_RUN_ID", &record.run_id)
        .env("PVISOR_OVERLAY_STAGE", &overlay.stage_dir)
        .process_group(0)
        .spawn()
        .with_context(|| format!("execute inspect command `{program}`"))?;
    let mut tree = super::host_process::OwnedTree::new(
        child.id() as i32,
        std::process::id() as i32,
        false,
        0,
    )?;
    let foreground = super::host_process::TerminalOwner::give_to(child.id() as i32)?;
    let mut deadline = None;
    let status = loop {
        tree.refresh()?;
        if tree.root_exited() {
            // Keep the group leader unreaped through cleanup on platforms that
            // use PGID ownership instead of pidfds.
            tree.cleanup(true)?;
            break child.wait()?;
        }
        if deadline.is_none()
            && super::host_service::worker_cancellation().is_some_and(|token| token.is_cancelled())
        {
            tree.signal_root(libc::SIGTERM)?;
            deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(1));
        }
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            tree.cleanup(true)?;
            break child.wait()?;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    tree.cleanup(true)?;
    drop(foreground);
    if let Some(signal) = status.signal()
        && [libc::SIGINT, libc::SIGTERM, libc::SIGHUP].contains(&signal)
    {
        super::host_service::notify_cancel(signal);
    }
    // Even if this unmount hangs, the service and frontend have independently
    // armed, bounded whole-request tree cleanup before inspect was admitted.
    mount.close()?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
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
    use crate::runtime::job_service::{ApplyRequest, RuntimeJobService};
    let response = RuntimeJobService::apply(
        &super::host::service_context(),
        ApplyRequest {
            job: crate::runtime::job_service::JobSelection {
                selector: Some(args.selector),
                storage: args.output_dir,
            },
            target: args.target,
            selection: ApplySelection {
                paths: args.paths,
                includes: args.include,
                excludes: args.exclude,
            },
            all: args.all,
        },
    )?;
    render_mutation(response);
    Ok(())
}
pub fn drop_overlay(args: SelectArgs) -> anyhow::Result<()> {
    use crate::runtime::job_service::{DropRequest, RuntimeJobService};
    let response = RuntimeJobService::drop(
        &super::host::service_context(),
        DropRequest {
            job: crate::runtime::job_service::JobSelection {
                selector: Some(args.selector),
                storage: args.output_dir,
            },
        },
    )?;
    render_mutation(response);
    Ok(())
}
fn render_mutation(response: crate::runtime::job_service::MutationResponse) {
    use crate::runtime::job_service::MutationOutcome;
    match response.outcome {
        MutationOutcome::AlreadyApplied => println!(
            "already applied {} → {}",
            response.job_id,
            response.target.display()
        ),
        MutationOutcome::AlreadyDropped => println!(
            "remaining staged changes already dropped for {}",
            response.job_id
        ),
        MutationOutcome::Dropped => {
            println!("dropped remaining staged changes for {}", response.job_id)
        }
        MutationOutcome::Applied {
            applied,
            apply_id,
            remaining,
        } => println!(
            "applied {} changes from {} → {} (apply_id={}, remaining={})",
            applied,
            response.job_id,
            response.target.display(),
            apply_id,
            remaining
        ),
    }
}

fn selected(selector: Option<&Path>, output_dir: &Path) -> anyhow::Result<RunRecord> {
    let storage = output_dir
        .canonicalize()
        .unwrap_or_else(|_| output_dir.to_path_buf());
    let record = resolve_run(selector, &storage)?;
    super::host_service::check_record(&record)?;
    Ok(record)
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
        let lease = crate::runtime::RunLease::acquire(temporary.path()).unwrap();
        let path = temporary.path().join(LEASE_FILENAME);
        let pid = std::process::id() as libc::pid_t;
        assert!(pid_holds_lease(pid, &path).unwrap());
        drop(lease);
        let _merely_open = std::fs::File::open(&path).unwrap();
        assert!(
            !pid_holds_lease(pid, &path).unwrap(),
            "an open descriptor is not exclusive lease ownership"
        );
    }
}
