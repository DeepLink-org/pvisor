use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;
use clap::Args;

use pvisor::{ReadOnlyOverlayMount, RunRecord, mount_overlay_record_read_only, resolve_run};

const DEFAULT_STORAGE: &str = ".pvisor/capture";

#[derive(Debug, Args)]
pub(super) struct StatusCommandArgs {
    #[command(flatten)]
    pub args: StatusArgs,
    #[command(flatten)]
    pub vm: super::live_vm::VmOptions,
}

#[derive(Debug, Clone, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusArgs {
    /// Session id, stage directory, upper directory, or workspace path.
    pub selector: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
    #[arg(long)]
    pub json: bool,
    /// Lifecycle only: do not enumerate stage files, checkpoints or apply batches.
    #[arg(long, requires = "json", conflicts_with_all = ["review", "diff"])]
    #[serde(default)]
    pub lifecycle: bool,
    /// Show the Session's Run Bundle, safety evidence, and staged changes.
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
    /// Live Session id, stage directory, or workspace path.
    pub selector: PathBuf,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectArgs {
    /// Session id, stage directory, upper directory, or workspace path.
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
    /// Session id, stage directory, workspace path, or container name/id.
    pub selector: PathBuf,
    /// Select a container's exact staged bind guest path (never a Session selector).
    #[arg(long, value_name = "GUEST_PATH")]
    #[serde(default)]
    pub mount: Option<PathBuf>,
    #[arg(long, short = 'o', default_value = DEFAULT_STORAGE)]
    pub output_dir: PathBuf,
}

#[derive(Debug, Clone, Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyArgs {
    /// Session id, stage directory, workspace selector, or container name/id; internal stage paths are refused.
    #[arg(value_name = "SOURCE")]
    pub selector: PathBuf,
    /// Select a container's exact staged bind guest path (never a Session selector).
    #[arg(long, value_name = "GUEST_PATH")]
    #[serde(default)]
    pub mount: Option<PathBuf>,
    /// Apply recorded system-root changes as root instead of the project scope.
    /// Requires a root-created stage in root-controlled storage; sudo apply of
    /// another user's stage is unsupported by same-UID Session locks. This is not an
    /// atomic system upgrade: filtering package files separately from package
    /// metadata can leave the system/package database inconsistent.
    #[arg(long, help_heading = "Scope")]
    #[serde(default)]
    pub rootfs: bool,
    /// Apply this relative path and its descendants. Repeatable.
    #[arg(
        long = "path",
        value_name = "RELATIVE_PATH",
        help_heading = "Selection"
    )]
    pub paths: Vec<PathBuf>,
    /// Include staged paths matching this glob. Repeatable.
    #[arg(long, value_name = "GLOB", help_heading = "Selection")]
    pub include: Vec<String>,
    /// Exclude staged paths matching this glob. Repeatable.
    #[arg(long, value_name = "GLOB", help_heading = "Selection")]
    pub exclude: Vec<String>,
    /// Select every change in scope; exclusions remain effective.
    #[arg(long, conflicts_with_all = ["paths", "include"], help_heading = "Selection")]
    pub all: bool,
    /// Choose paths with an interactive terminal picker.
    #[arg(long, short = 'i', conflicts_with_all = ["dry_run", "json"], help_heading = "Selection")]
    #[serde(default)]
    pub interactive: bool,
    /// Preview without writing the target or publishing an apply ledger.
    #[arg(long, help_heading = "Preview")]
    #[serde(default)]
    pub dry_run: bool,
    /// Show bounded text diffs for the preview; binary content is skipped.
    #[arg(
        long,
        requires = "dry_run",
        conflicts_with = "json",
        help_heading = "Preview"
    )]
    #[serde(default)]
    pub diff: bool,
    /// Emit one JSON object; requires --all, --path or --include.
    #[arg(long, conflicts_with = "interactive", help_heading = "Output")]
    #[serde(default)]
    pub json: bool,
    /// Session lookup storage.
    #[arg(long, value_name = "DIR", default_value = DEFAULT_STORAGE, help_heading = "Storage")]
    pub store: PathBuf,
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
            mount: None,
        });
    }
    if args.lifecycle {
        let response = pvisor::session_service::SessionService::lifecycle_status(
            &super::host::service_context(),
            pvisor::session_service::StatusRequest {
                session: pvisor::session_service::SessionSelection {
                    selector: args.selector,
                    storage: args.output_dir,
                },
            },
        )?;
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    let response = pvisor::session_service::SessionService::status(
        &super::host::service_context(),
        pvisor::session_service::StatusRequest {
            session: pvisor::session_service::SessionSelection {
                selector: args.selector,
                storage: args.output_dir,
            },
        },
    )?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&status_json(&response))?);
        return Ok(());
    }
    let record = &response.record;
    println!("session: {}", record.run_id);
    println!("session: {}", record.session_id);
    println!("state: {}", response.state.as_str());
    if let Some(execution) = &response.live_execution {
        println!("execution: {:?}", execution.state);
    }
    println!(
        "pid: {}{}",
        record.pid,
        if response.live {
            " (live)"
        } else {
            " (offline)"
        }
    );
    println!("agent: {}", record.agent);
    println!(
        "command: {}{}",
        shell_join(&record.command),
        if record.command_truncated {
            " [excerpt]"
        } else {
            ""
        }
    );
    println!("stage: {}", record.stage.display());
    println!(
        "checkpoints: {} workspace",
        response.workspace_checkpoint_count
    );
    if let Some(blocker) = &response.execution_blocker {
        println!("execution checkpoint: unsupported ({blocker})");
    } else {
        println!("execution checkpoint: supported");
    }
    if let Some(session) = &response.execution {
        println!(
            "execution: {} ({} checkpoints)",
            session.state, session.checkpoint_count
        );
        if let Some(head) = &session.suspended_head {
            println!("suspended head: {head}");
        }
    }
    if response.apply_batch_count > 0 {
        println!("apply batches: {}", response.apply_batch_count);
    }
    println!("net: {}", record.network);
    println!(
        "overlaynet: {}",
        record.overlaynet_listen.as_deref().unwrap_or("disabled")
    );
    if let Some(observed) = &response.filesystem_observation {
        println!(
            "file accesses observed: {} operations, {} denied across {} paths ({} omitted)",
            observed.hits, observed.denied, observed.observed_paths, observed.overflow_hits
        );
        for sample in &observed.denied_samples {
            println!(
                "  denied {}: {}{}",
                sample.denied,
                sample.target,
                if sample.truncated { " [excerpt]" } else { "" }
            );
        }
    }
    if let Some(observed) = &response.network_observation {
        println!(
            "network accesses observed: {} policy allowed, {} denied, {} transport failures across {} destinations ({} omitted)",
            observed.policy_allowed,
            observed.denied,
            observed.transport_failures,
            observed.observed_targets,
            observed.overflow_hits
        );
        for sample in &observed.denied_samples {
            println!(
                "  denied {}: {}{}",
                sample.denied,
                sample.target,
                if sample.truncated { " [excerpt]" } else { "" }
            );
        }
    }
    if let Some(progress) = &response.execution_progress {
        println!(
            "controls: {} queued, {} in flight",
            progress.queued, progress.in_flight
        );
        if progress.history_gap || progress.outcome_unknown {
            println!("execution evidence: reconciliation required");
        }
    }
    println!(
        "gateway: {}",
        record.gateway_listen.as_deref().unwrap_or("disabled")
    );
    if let Some(overlay) = &record.overlay {
        let fs = response
            .filesystem
            .as_ref()
            .context("OverlayFS status missing")?;
        println!("fs: {:?} (read-only inspect available)", overlay.state);
        println!("target: {}", overlay.target.display());
        println!("upper: {}", overlay.upper.display());
        println!(
            "changes: {} paths, {} whiteouts",
            fs.changed_files, fs.whiteouts
        );
    } else {
        println!("fs: host view (no OverlayFS workspace)");
    }
    Ok(())
}

fn status_json(response: &pvisor::session_service::StatusResponse) -> serde_json::Value {
    let record = &response.record;
    serde_json::json!({
        "schema_version": response.schema_version,
        "state": response.state,
        "run": record, "live": response.live, "live_execution": response.live_execution,
        "execution_progress": response.execution_progress,
        "checkpoint_capability": {"workspace": record.overlay.is_some(), "workspace_capture_requires": "confirmed_stopped", "execution": response.execution_blocker.is_none(), "execution_blocker": response.execution_blocker},
        "execution": response.execution, "workspace_checkpoint_count": response.workspace_checkpoint_count,
        "workspace_generation": record.overlay.as_ref().map(|o| o.generation),
        "apply_batch_count": response.apply_batch_count,
        "observations": {"filesystem": response.filesystem_observation, "network": response.network_observation},
        "filesystem": response.filesystem,
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
        if pvisor::session_execution::terminate_suspended(&record)? {
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version":1,"operation":"kill","session_id":record.run_id,"state":"terminated","suspended_head_released":true})
                );
            } else {
                println!("terminated suspended Session {}", record.run_id);
            }
            return Ok(());
        }
        if record.state.is_stopped() {
            let _session = super::host::lock_selected_session(&record)?;
            let (current, _lease) = record.lock_current()?;
            super::host_service::check_record(&current)?;
            current.require_stopped()?;
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version":1,"operation":"kill",
                "session_id":current.run_id,"state":current.state,"already_stopped":true})
                );
            } else {
                println!("Session {} is already stopped", current.run_id);
            }
            return Ok(());
        }
        anyhow::bail!("Session has no valid recorded owner; only its durable state is available");
    }
    let endpoint = endpoint.unwrap();
    endpoint.exchange_checked(
        &record.run_id,
        record
            .execution_id
            .as_deref()
            .context("Session has no Execution identity")?,
        pvisor::host_transport::InstanceCommand::Terminate {},
        Some(&super::host_service::check_cancelled),
    )?;
    if args.json {
        println!(
            "{}",
            serde_json::json!({"schema_version":1,"operation":"kill",
            "session_id":record.run_id,"state":"stopping","termination_requested":true,"cooperative":true})
        );
    } else {
        println!(
            "requested termination of Session {} (PID {})",
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
    // Declared before the mount so early-return cleanup retains the leases.
    let mut _leases = None;
    let mount = if let Some(endpoint) = endpoint {
        let execution = selected
            .execution_id
            .clone()
            .context("live Session has no Execution identity")?;
        let response = endpoint.exchange_checked(
            &selected.run_id,
            &execution,
            pvisor::host_transport::InstanceCommand::Inspect {
                checkpoint: args.checkpoint.clone(),
            },
            Some(&super::host_service::check_cancelled),
        )?;
        let cleanup = InspectCleanup(Some(InspectCleanupKind::Remote {
            endpoint,
            session: selected.run_id.clone(),
            execution,
            id: response["id"]
                .as_str()
                .context("inspect response missing id")?
                .to_owned(),
        }));
        InspectMount {
            mountpoint: serde_json::from_value(response["mountpoint"].clone())?,
            cleanup,
        }
    } else {
        let session = super::host::lock_selected_session(&selected)?;
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
        _leases = Some((session, lease));
        let overlay = record
            .overlay
            .as_ref()
            .context("this Session has no OverlayFS workspace to inspect")?;
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
        std::fs::create_dir_all(
            inspect_root
                .parent()
                .context("inspect root has no parent")?,
        )?;
        std::fs::create_dir(&inspect_root)?;
        let inspect_root = InspectRoot(inspect_root);
        let session =
            mount_overlay_record_read_only(overlay, &lowers, &inspect_root.0.join("merged"))?;
        InspectMount {
            mountpoint: session.mountpoint().to_owned(),
            cleanup: InspectCleanup(Some(InspectCleanupKind::Local {
                inspect_root,
                session,
            })),
        }
    };
    let overlay = record
        .overlay
        .as_ref()
        .context("this Session has no OverlayFS workspace to inspect")?;

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
        super::host_process::OwnedTree::new(child.id() as i32, std::process::id() as i32, true)?;
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

struct InspectMount {
    mountpoint: PathBuf,
    cleanup: InspectCleanup,
}

impl InspectMount {
    fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    fn close(mut self) -> anyhow::Result<()> {
        self.cleanup.close()
    }
}

// Installed before subsequent fallible setup. Explicit close and unwinding use
// the same one-shot cleanup; Drop must not replace the command's primary error.
struct InspectCleanup(Option<InspectCleanupKind>);

enum InspectCleanupKind {
    Remote {
        endpoint: pvisor::host_transport::LiveControlRecord,
        session: String,
        execution: String,
        id: String,
    },
    Local {
        inspect_root: InspectRoot,
        session: ReadOnlyOverlayMount,
    },
}

impl InspectCleanup {
    fn close(&mut self) -> anyhow::Result<()> {
        match self.0.take() {
            Some(InspectCleanupKind::Remote {
                endpoint,
                session,
                execution,
                id,
            }) => endpoint
                .exchange(
                    &session,
                    &execution,
                    pvisor::host_transport::InstanceCommand::UnmountInspect { id },
                )
                .map(|_| ()),
            Some(InspectCleanupKind::Local {
                inspect_root,
                session,
            }) => {
                session.unmount()?;
                drop(inspect_root);
                Ok(())
            }
            None => Ok(()),
        }
    }
}

impl Drop for InspectCleanup {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

// Only the unique invocation directory is ours. Never recursively remove a
// mount or shared inspect/stage contents, including after failed construction.
struct InspectRoot(PathBuf);

impl Drop for InspectRoot {
    fn drop(&mut self) {
        // Failed mount construction can leave its empty mountpoint behind.
        // remove_dir cannot traverse contents or remove a still-mounted entry.
        let _ = std::fs::remove_dir(self.0.join("merged"));
        let _ = std::fs::remove_dir(&self.0);
    }
}

pub fn drop_overlay(mut args: SelectArgs) -> anyhow::Result<()> {
    use pvisor::session_service::{DropRequest, SessionService};
    args.selector = super::stage_source::resolve(
        Some(&args.selector),
        &args.output_dir,
        args.mount.as_deref(),
    )?
    .expect("explicit drop source");
    args.mount = None;
    let response = SessionService::drop(
        &super::host::service_context(),
        DropRequest {
            session: pvisor::session_service::SessionSelection {
                selector: Some(args.selector),
                storage: args.output_dir,
            },
        },
    )?;
    render_drop(response);
    Ok(())
}
fn render_drop(response: pvisor::session_service::MutationResponse) {
    use pvisor::session_service::MutationOutcome;
    match response.outcome {
        MutationOutcome::AlreadyDropped => println!(
            "remaining staged changes already dropped for {}",
            response.session_id
        ),
        MutationOutcome::Dropped => {
            println!(
                "dropped remaining staged changes for {}",
                response.session_id
            )
        }
    }
}

pub(super) fn selected(selector: Option<&Path>, output_dir: &Path) -> anyhow::Result<RunRecord> {
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
    fn inspect_root_cleanup_handles_partial_construction_without_removing_contents() {
        let root = tempfile::tempdir().unwrap();
        let invocation = root.path().join("invocation");
        std::fs::create_dir_all(invocation.join("merged")).unwrap();
        drop(InspectRoot(invocation.clone()));
        assert!(!invocation.exists());
        assert!(root.path().is_dir());

        std::fs::create_dir_all(invocation.join("merged")).unwrap();
        let retained = invocation.join("merged/retain");
        std::fs::write(&retained, b"do not recursively remove").unwrap();
        drop(InspectRoot(invocation));
        assert_eq!(
            std::fs::read(retained).unwrap(),
            b"do not recursively remove"
        );
    }

    #[derive(Debug, clap::Parser)]
    struct ApplyParser {
        #[command(flatten)]
        args: ApplyArgs,
    }

    #[test]
    fn rootfs_apply_is_explicit_and_target_override_is_removed() {
        use clap::Parser;
        let ordinary = ApplyParser::try_parse_from(["apply", "stage", "--all"]).unwrap();
        assert!(!ordinary.args.rootfs);
        let rootfs = ApplyParser::try_parse_from(["apply", "stage", "--all", "--rootfs"]).unwrap();
        assert!(rootfs.args.rootfs);
        for target in ["/", "/tmp/workspace"] {
            let error = ApplyParser::try_parse_from([
                "apply", "stage", "--all", "--rootfs", "--target", target,
            ])
            .unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        }
    }

    #[test]
    fn rootfs_help_explains_upgrade_and_ownership_limits() {
        use clap::CommandFactory;
        let help = ApplyParser::command().render_long_help().to_string();
        for warning in [
            "not an atomic system upgrade",
            "package metadata",
            "same-UID Session locks",
        ] {
            assert!(help.contains(warning), "{help}");
        }
    }

    #[test]
    fn shell_join_quotes_only_when_needed() {
        assert_eq!(
            shell_join(&["rg".into(), "hello world".into()]),
            "rg \"hello world\""
        );
    }
}
