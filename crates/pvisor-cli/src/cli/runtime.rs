use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;
use clap::Args;

use pvisor::{
    ApplySelection, ReadOnlyOverlayMount, RunRecord, mount_overlay_record_read_only, resolve_run,
};

const DEFAULT_STORAGE: &str = ".pvisor/capture";

#[derive(Debug, Args)]
pub(super) struct StatusCommandArgs {
    #[command(flatten)]
    pub args: StatusArgs,
    #[command(flatten)]
    pub vm: super::host::VmOptions,
}

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
    let response = pvisor::job_service::RuntimeJobService::status(
        &super::host::service_context(),
        pvisor::job_service::StatusRequest {
            job: pvisor::job_service::JobSelection {
                selector: args.selector,
                storage: args.output_dir,
            },
        },
    )?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&status_json(&response))?);
        return Ok(());
    }
    let pvisor::job_service::StatusResponse {
        record,
        live,
        live_attempt,
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
    } else if record.state == pvisor::RunRecordState::Running {
        "stale"
    } else {
        record.state.as_str()
    };
    println!("state: {state}");
    if let Some(attempt) = live_attempt {
        println!("attempt: {:?}", attempt.state);
    }
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

fn status_json(response: &pvisor::job_service::StatusResponse) -> serde_json::Value {
    let record = &response.record;
    serde_json::json!({
        "run": record, "live": response.live, "live_attempt": response.live_attempt,
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
    let endpoint = record
        .live_control
        .as_ref()
        .filter(|endpoint| endpoint.is_available());
    if endpoint.is_none() {
        if pvisor::job_execution::terminate_suspended(&record)? {
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
        anyhow::bail!("Job has no valid recorded owner; only its durable state is available");
    }
    let endpoint = endpoint.unwrap();
    endpoint.exchange_checked(
        &record.run_id,
        record
            .attempt_id
            .as_deref()
            .context("Job has no Attempt identity")?,
        pvisor::host_transport::InstanceCommand::Terminate {},
        Some(&super::host_service::check_cancelled),
    )?;
    if args.json {
        println!(
            "{}",
            serde_json::json!({"schema_version":1,"operation":"kill",
            "job_id":record.run_id,"state":"stopping","termination_requested":true,"cooperative":true})
        );
    } else {
        println!(
            "requested termination of Job {} (PID {})",
            record.run_id, record.pid
        );
    }
    Ok(())
}

pub fn inspect(args: InspectArgs) -> anyhow::Result<i32> {
    let selected = selected(args.selector.as_deref(), &args.output_dir)?;
    let endpoint = selected
        .live_control
        .as_ref()
        .filter(|endpoint| endpoint.is_available())
        .cloned();
    let mut record = selected.clone();
    let mut leases = None;
    let mount = if let Some(endpoint) = endpoint {
        let attempt = selected
            .attempt_id
            .clone()
            .context("live Job has no Attempt identity")?;
        let response = endpoint.exchange_checked(
            &selected.run_id,
            &attempt,
            pvisor::host_transport::InstanceCommand::Inspect {
                checkpoint: args.checkpoint.clone(),
            },
            Some(&super::host_service::check_cancelled),
        )?;
        InspectMount::Remote {
            endpoint,
            job: selected.run_id.clone(),
            attempt,
            id: response["id"]
                .as_str()
                .context("inspect response missing id")?
                .to_owned(),
            mountpoint: serde_json::from_value(response["mountpoint"].clone())?,
        }
    } else {
        let job = super::host::lock_selected_job(&selected)?;
        let (current, lease) = selected.lock_current()?;
        super::host::check_selected_record(&selected, &current)?;
        record = current;
        super::host_service::check_record(&record)?;
        if let Some(id) = &args.checkpoint {
            let checkpoint = pvisor::checkpoint::resolve_checkpoint(&record, id)?;
            record = pvisor::checkpoint::workspace_view(&record, &checkpoint)?;
        } else {
            record.require_stopped()?;
        }
        leases = Some((job, lease));
        let overlay = record
            .overlay
            .as_ref()
            .context("this Job has no OverlayFS workspace to inspect")?;
        let lowers = if record.overlay_lowers.is_empty() {
            vec![overlay.target.clone()]
        } else {
            record.overlay_lowers.clone()
        };
        let inspect_root = overlay.stage_dir.join("inspect").join(format!(
            "{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let session =
            mount_overlay_record_read_only(overlay, &lowers, &inspect_root.join("merged"))?;
        InspectMount::Local {
            inspect_root,
            session,
        }
    };
    let _leases = leases;
    let overlay = record
        .overlay
        .as_ref()
        .context("this Job has no OverlayFS workspace to inspect")?;

    let command = if args.command.is_empty() {
        vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())]
    } else {
        args.command
    };
    let (program, command_args) = command.split_first().context("missing inspect command")?;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    super::host_service::check_cancelled()?;
    super::host_process::become_subreaper()?;
    let mut child = Command::new(program)
        .args(command_args)
        .current_dir(mount.mountpoint())
        .env("PVISOR_INSPECT", "1")
        .env("PVISOR_RUN_ID", &record.run_id)
        .env("PVISOR_OVERLAY_STAGE", &overlay.stage_dir)
        .process_group(0)
        .spawn()
        .with_context(|| format!("execute inspect command `{program}`"))?;
    let mut tree =
        super::host_process::OwnedTree::new(child.id() as i32, std::process::id() as i32, true, 0)?;
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
            && super::host_service::request_cancellation().is_some_and(|token| token.is_cancelled())
        {
            tree.signal_root(libc::SIGTERM)?;
            deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(1));
        }
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            tree.cleanup(true)?;
            break child.wait()?;
        }
        tree.wait_root(std::time::Duration::from_millis(10))?;
    };
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
        endpoint: pvisor::host_transport::LiveControlRecord,
        job: String,
        attempt: String,
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
            Self::Remote {
                endpoint,
                job,
                attempt,
                id,
                ..
            } => endpoint
                .exchange(
                    &job,
                    &attempt,
                    pvisor::host_transport::InstanceCommand::UnmountInspect { id },
                )
                .map(|_| ()),
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
    use pvisor::job_service::{ApplyRequest, RuntimeJobService};
    let response = RuntimeJobService::apply(
        &super::host::service_context(),
        ApplyRequest {
            job: pvisor::job_service::JobSelection {
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
    use pvisor::job_service::{DropRequest, RuntimeJobService};
    let response = RuntimeJobService::drop(
        &super::host::service_context(),
        DropRequest {
            job: pvisor::job_service::JobSelection {
                selector: Some(args.selector),
                storage: args.output_dir,
            },
        },
    )?;
    render_mutation(response);
    Ok(())
}
fn render_mutation(response: pvisor::job_service::MutationResponse) {
    use pvisor::job_service::MutationOutcome;
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
}
