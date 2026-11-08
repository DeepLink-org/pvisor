//! Bounded read model for both recorded-owner and offline Session queries.
use super::{ServiceContext, SessionSelection, SessionService, execution_blocker};
use crate::runtime::{load_apply_records, overlay_status};
use anyhow::{Context, ensure};
use pvisor_core::operation::FilesystemObservation;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_STATUS_BYTES: usize = 256 * 1024;
const MAX_SAMPLES: usize = 5;
const MAX_SAMPLE_BYTES: usize = 512;

/// Select one Session using the same admission fences as mutation requests.
pub struct StatusRequest {
    /// Session identity or path and its lookup storage.
    pub session: SessionSelection,
}

/// Current workspace counts, with at most five bounded path samples.
#[derive(Debug, Serialize, Deserialize)]
pub struct FilesystemSummary {
    /// Changed-path count for the selected workspace view.
    pub changed_files: usize,
    /// Deletion-marker count for the selected workspace view.
    pub whiteouts: usize,
    /// At most five display samples, each at most 512 UTF-8 bytes.
    pub sample_paths: Vec<String>,
    /// Samples were omitted or shortened; counts still describe the full view.
    pub samples_truncated: bool,
}

/// Display-only workspace identity; never accepted as writeback authority.
#[derive(Serialize, Deserialize)]
pub struct StatusOverlay {
    /// Current state of the containing summary; only authoritative operations may mutate runtime state.
    pub state: crate::runtime::OverlayState,
    /// Recorded workspace generation; not mutation authorization.
    pub generation: u64,
    /// Display path or destination; may be shortened.
    pub target: PathBuf,
    /// Private upper path for inspection.
    pub upper: PathBuf,
}

/// A display projection, not a serialized RunRecord or an executable RunSpec.
#[derive(Serialize, Deserialize)]
pub struct StatusRun {
    /// Selected logical Session identity.
    pub run_id: String,
    /// Selected Execution identity, absent on legacy records.
    pub execution_id: Option<String>,
    /// Recorded session identity.
    pub session_id: String,
    /// Current state of the containing summary; only authoritative operations may mutate runtime state.
    pub state: crate::RunRecordState,
    /// Recorded owner PID; callers must not use it as signal authority.
    pub pid: u32,
    /// Display agent name, at most 256 UTF-8 bytes.
    pub agent: String,
    /// At most 32 display arguments, each at most 512 UTF-8 bytes.
    pub command: Vec<String>,
    /// The displayed command is an excerpt and must not be used to relaunch.
    pub command_truncated: bool,
    /// Recorded stage selector for subsequent admitted queries.
    pub stage: PathBuf,
    /// Recorded wall-clock start time in milliseconds.
    pub started_at_unix_ms: u64,
    /// Confirmed finish time when present; wall clocks may move backwards.
    pub finished_at_unix_ms: Option<u64>,
    /// Display Gateway address, absent when not configured.
    pub gateway_listen: Option<String>,
    /// Display proxy address, absent when not configured.
    pub overlaynet_listen: Option<String>,
    /// Network capability mode, without its potentially large rule set.
    pub network: String,
    /// Workspace display identity, absent without staging.
    pub overlay: Option<StatusOverlay>,
}

/// Owner-sampled Execution state without its unbounded admission plan.
#[derive(Serialize, Deserialize)]
pub struct StatusExecution {
    /// Selected Execution identity, absent on legacy records.
    pub execution_id: String,
    /// Current state of the containing summary; only authoritative operations may mutate runtime state.
    pub state: pvisor_core::RunState,
    /// Owner observation wall-clock time in milliseconds.
    pub updated_at_unix_ms: u64,
}
impl From<&pvisor_core::RunStatus> for StatusExecution {
    fn from(status: &pvisor_core::RunStatus) -> Self {
        Self {
            execution_id: status.execution.execution_id.to_string(),
            state: status.state,
            updated_at_unix_ms: status.updated_at_unix_ms,
        }
    }
}

/// Durable execution counts. Scoped request receipts remain in Session storage.
#[derive(Serialize, Deserialize)]
pub struct ExecutionSummary {
    /// Current state of the containing summary; only authoritative operations may mutate runtime state.
    pub state: crate::SessionState,
    /// Confirmed durable suspended head, when present.
    pub suspended_head: Option<String>,
    /// Execution currently selected by the durable Session ledger.
    pub active_execution: String,
    /// Number of recorded execution checkpoints.
    pub checkpoint_count: usize,
    /// Number of scoped capture requests retained in the ledger.
    pub capture_request_count: usize,
    /// Number of scoped restore requests retained in the ledger.
    pub resume_request_count: usize,
    /// Number of scoped fork requests retained in the ledger.
    pub fork_request_count: usize,
}

/// Source of counters; a durable observation is not a live measurement.
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSource {
    /// Sampled from the currently selected owner.
    Live,
    /// Loaded from retained observations; not a live measurement.
    Durable,
}

/// One diagnostic sample. Its display text may be shortened.
#[derive(Serialize, Deserialize)]
pub struct DeniedSample {
    /// Display path or destination; may be shortened.
    pub target: String,
    /// Denied-operation count reported by this observation.
    pub denied: u64,
    /// The displayed target was shortened on a UTF-8 boundary.
    pub truncated: bool,
}

/// Bounded counters over the recorded path table, not all guest filesystem I/O.
#[derive(Serialize, Deserialize)]
pub struct FilesystemActivity {
    /// Whether these counters were sampled live or loaded from retained records.
    pub source: ObservationSource,
    /// Operations counted in the bounded recorded path table.
    pub hits: u64,
    /// Denied-operation count reported by this observation.
    pub denied: u64,
    /// Number of paths in that table, not all guest accesses.
    pub observed_paths: usize,
    /// Operations or destinations omitted by the owning collector.
    pub overflow_hits: u64,
    /// At most five bounded denied path or destination samples.
    pub denied_samples: Vec<DeniedSample>,
}

/// Bounded driver counters; traffic bypassing interception is not observed.
#[derive(Serialize, Deserialize)]
pub struct NetworkActivity {
    /// Whether these counters were sampled live or loaded from retained records.
    pub source: ObservationSource,
    /// Operations allowed by the interception policy.
    pub policy_allowed: u64,
    /// Denied-operation count reported by this observation.
    pub denied: u64,
    /// Driver-reported connection and transport failures.
    pub transport_failures: u64,
    /// Number of destinations retained by the collector.
    pub observed_targets: usize,
    /// Operations or destinations omitted by the owning collector.
    pub overflow_hits: u64,
    /// At most five bounded denied path or destination samples.
    pub denied_samples: Vec<DeniedSample>,
}

/// An optional observation is unavailable when absent, not a zero counter.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Activity {
    /// Current workspace counts, absent without staging.
    pub filesystem: Option<FilesystemActivity>,
    /// Network capability mode, without its potentially large rule set.
    pub network: Option<NetworkActivity>,
}

/// Schema 2 is a bounded summary, deliberately excluding historical records.
/// Offline completion remains receipt-validated under the existing leases.
#[derive(Serialize, Deserialize)]
pub struct StatusResponse {
    /// Effective phase projected from one live snapshot or validated completion evidence.
    pub state: crate::StatusState,
    /// Summary contract version; currently 2.
    pub schema_version: u16,
    /// Display-only projection, never a full RunRecord or writeback authority.
    pub record: StatusRun,
    /// A valid recorded owner currently owns this Execution.
    pub live: bool,
    /// State sampled by that owner; absent for offline queries.
    pub live_execution: Option<StatusExecution>,
    /// Owner-sampled control progress; absent without a live observation.
    pub execution_progress: Option<crate::ExecutionProgress>,
    /// Bounded reason capture is unsupported, absent when admitted capabilities allow it.
    pub execution_blocker: Option<String>,
    /// Number of recorded logical workspace checkpoints.
    pub workspace_checkpoint_count: usize,
    /// Number of retained apply ledger batches.
    pub apply_batch_count: usize,
    /// Current workspace counts, absent without staging.
    pub filesystem: Option<FilesystemSummary>,
    /// Available filesystem counters; absence does not mean zero.
    pub filesystem_observation: Option<FilesystemActivity>,
    /// Available network counters; absence does not mean zero.
    pub network_observation: Option<NetworkActivity>,
    /// Durable native execution summary, absent for Sessions without that ledger.
    pub execution: Option<ExecutionSummary>,
}

pub(crate) fn excerpt(value: &str, limit: usize) -> (String, bool) {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), end < value.len())
}

pub(crate) fn filesystem_activity(
    value: &FilesystemObservation,
    source: ObservationSource,
) -> FilesystemActivity {
    let (hits, denied) = value.paths.values().flat_map(|ops| ops.values()).fold(
        (0u64, 0u64),
        |(hits, denied), counts| {
            (
                hits.saturating_add(counts.hits),
                denied.saturating_add(counts.denied),
            )
        },
    );
    let denied_samples = value
        .paths
        .iter()
        .filter_map(|(path, ops)| {
            let denied = ops
                .values()
                .fold(0u64, |sum, count| sum.saturating_add(count.denied));
            if denied == 0 {
                return None;
            }
            let (target, truncated) = excerpt(path, MAX_SAMPLE_BYTES);
            Some(DeniedSample {
                target,
                denied,
                truncated,
            })
        })
        .take(MAX_SAMPLES)
        .collect();
    FilesystemActivity {
        source,
        hits,
        denied,
        observed_paths: value.paths.len(),
        overflow_hits: value.overflow_hits,
        denied_samples,
    }
}

pub(crate) fn network_activity(
    value: &pvisor_overlaynet::InterceptionSnapshot,
    source: ObservationSource,
) -> NetworkActivity {
    let denied_samples = value
        .targets
        .iter()
        .filter(|(_, counts)| counts.denied > 0)
        .take(MAX_SAMPLES)
        .map(|(target, counts)| {
            let (target, truncated) = excerpt(target, MAX_SAMPLE_BYTES);
            DeniedSample {
                target,
                denied: counts.denied,
                truncated,
            }
        })
        .collect();
    NetworkActivity {
        source,
        policy_allowed: value.policy_allowed,
        denied: value.policy_denied.saturating_add(value.tcp_flows_denied),
        transport_failures: value.tcp_connect_failures.saturating_add(value.failures),
        observed_targets: value.targets.len(),
        overflow_hits: value.target_overflow,
        denied_samples,
    }
}

impl SessionService {
    /// Resolve and validate one Session, route live queries to its owner, or reconcile
    /// offline completion under existing leases. Return a schema-2 bounded view;
    /// missing optional observations remain absent, while corrupt authority is an error.
    pub fn status(
        context: &ServiceContext<'_>,
        request: StatusRequest,
    ) -> anyhow::Result<StatusResponse> {
        let mut record = Self::resolve(context, &request.session)?;
        let owner = super::super::instance_control::local_owner(&record);
        let endpoint = record
            .live_control
            .as_ref()
            .filter(|endpoint| endpoint.is_available());
        if let Some(endpoint) = endpoint
            && owner.is_none()
            && endpoint.pid != std::process::id()
        {
            match endpoint.exchange_checked(
                &record.run_id,
                record
                    .execution_id
                    .as_deref()
                    .context("live Session has no Execution identity")?,
                crate::host_transport::InstanceCommand::SessionStatus {},
                context.check_cancelled,
            ) {
                Ok(value) => return Ok(serde_json::from_value(value)?),
                Err(error)
                    if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                        )
                    }) => {}
                Err(error) => return Err(error),
            }
        }
        let live = owner.is_some();
        if !live && record.completion_protocol.is_some() {
            let _session = super::lock_selected_session(context, &record)?;
            match record.lock_current() {
                Ok((mut current, _lease)) => {
                    context.check(&current)?;
                    super::super::completion::reconcile(&mut current)?;
                    record = current;
                }
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::WouldBlock) => {}
                Err(error) => return Err(error),
            }
        }
        let workspace_checkpoint_count =
            crate::runtime::checkpoint::list_checkpoints(&record)?.len();
        let apply_batch_count = load_apply_records(&record.stage_dir())?.len();
        let filesystem = record
            .overlay
            .as_ref()
            .map(|overlay| {
                let (changed_files, whiteouts, samples) = if let Some(owner) = &owner {
                    #[cfg(test)]
                    super::super::instance_control::before_status_filesystem_sample();
                    // Discovery may end during this request. The selected
                    // owner remains valid for this read and still checks
                    // the current Session/Execution binding itself.
                    let status: pvisor_core::overlay::OverlayStatus = serde_json::from_value(
                        owner
                            .workspace(crate::host_transport::InstanceCommand::OverlayStatus {})?,
                    )?;
                    (status.changed_files, status.whiteouts, status.sample_paths)
                } else {
                    let status = overlay_status(overlay)?;
                    (status.changed_files, status.whiteouts, status.sample_paths)
                };
                let mut samples_truncated = samples.len() > MAX_SAMPLES;
                let sample_paths = samples
                    .iter()
                    .take(MAX_SAMPLES)
                    .map(|path| {
                        let (path, shortened) = excerpt(path, MAX_SAMPLE_BYTES);
                        samples_truncated |= shortened;
                        path
                    })
                    .collect();
                Ok::<_, anyhow::Error>(FilesystemSummary {
                    changed_files,
                    whiteouts,
                    sample_paths,
                    samples_truncated,
                })
            })
            .transpose()?;
        let observations: Option<Activity> = owner.as_ref().and_then(|owner| {
            owner
                .workspace(crate::host_transport::InstanceCommand::StatusObservations {})
                .ok()
                .and_then(|value| serde_json::from_value(value).ok())
        });
        let observations = observations.unwrap_or_default();
        let filesystem_observation = observations.filesystem.or_else(|| {
            record
                .filesystem_observation
                .as_ref()
                .map(|value| filesystem_activity(value, ObservationSource::Durable))
        });
        let network_observation = observations.network.or_else(|| {
            record
                .network_interception_metrics
                .as_ref()
                .map(|value| network_activity(value, ObservationSource::Durable))
        });
        let execution = crate::Session::read(&record)?
            .map(|session| {
                ensure!(
                    session.active_execution().len() <= 256
                        && session.head().is_none_or(|head| head.len() <= 256),
                    "invalid execution summary identity"
                );
                Ok::<_, anyhow::Error>(ExecutionSummary {
                    state: session.state(),
                    suspended_head: session.head().map(str::to_owned),
                    active_execution: session.active_execution().to_owned(),
                    checkpoint_count: session.checkpoints().len(),
                    capture_request_count: session.requests().len(),
                    resume_request_count: session.resumes().len(),
                    fork_request_count: session.forks().len(),
                })
            })
            .transpose()?;
        let mut command_truncated = record.command.len() > 32;
        let command = record
            .command
            .iter()
            .take(32)
            .map(|arg| {
                let (arg, truncated) = excerpt(arg, 512);
                command_truncated |= truncated;
                arg
            })
            .collect();
        let execution_blocker = execution_blocker(&record).map(|value| excerpt(&value, 1024).0);
        let mut response = StatusResponse {
            state: crate::StatusState::recorded(record.state),
            schema_version: 3,
            live,
            live_execution: None,
            execution_progress: None,
            execution_blocker,
            workspace_checkpoint_count,
            apply_batch_count,
            filesystem,
            filesystem_observation,
            network_observation,
            execution,
            record: StatusRun {
                stage: record.stage_dir(),
                run_id: record.run_id,
                execution_id: record.execution_id,
                session_id: record.session_id,
                state: record.state,
                pid: record.pid,
                agent: excerpt(&record.agent, 256).0,
                command,
                command_truncated,
                started_at_unix_ms: record.started_at_unix_ms,
                finished_at_unix_ms: record.finished_at_unix_ms,
                gateway_listen: record.gateway_listen.map(|value| excerpt(&value, 512).0),
                overlaynet_listen: record.overlaynet_listen.map(|value| excerpt(&value, 512).0),
                network: record
                    .network
                    .get("mode")
                    .and_then(serde_json::Value::as_str)
                    .map(|mode| excerpt(mode, 64).0)
                    .unwrap_or_else(|| "unspecified".into()),
                overlay: record.overlay.map(|overlay| StatusOverlay {
                    state: overlay.state,
                    generation: overlay.generation,
                    target: overlay.target,
                    upper: overlay.upper.path().to_path_buf(),
                }),
            },
        };
        if let Some(owner) = owner {
            response.project_live(owner.execution.snapshot())?;
        }
        ensure!(
            serde_json::to_vec(&response)?.len() <= MAX_STATUS_BYTES,
            "Session status summary exceeds size limit"
        );
        Ok(response)
    }
}
