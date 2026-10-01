//! Durable, versioned summary of one pVisor Run.

use crate::runtime::{
    ChangeEntry, OverlayState, RunLineage, RunRecord, overlay_changes, overlay_status,
};
use crate::util::sync_directory;
use crate::{AgentCtlSnapshot, unix_now_ms};
use pvisor_control::{
    ArtifactRef, CapabilityDimension, ExecutorIdentity, ExecutorObservations, ExecutorPlan,
    IsolationKind, ProcessOutput, ResourceLimits, RunFailure, RunResult, RunState,
};
use pvisor_overlaynet::{InterceptionProfile, InterceptionSnapshot};
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub const RUN_BUNDLE_SCHEMA_VERSION: u32 = 3;
pub const RUN_BUNDLE_FILENAME: &str = "run-bundle.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunBundle {
    pub schema_version: u32,
    pub generated_at_unix_ms: u64,
    pub run: BundleRun,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lineage: Option<RunLineage>,
    /// Sole authoritative evidence: emitted by the executor at teardown.
    pub executor_observations: ExecutorObservations,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor_plan: Option<ExecutorPlan>,
    pub safety: SafetySummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem: Option<FilesystemSummary>,
    pub network: NetworkSummary,
    #[serde(default)]
    pub environment: crate::runtime::EnvironmentProjection,
    #[serde(default)]
    pub resources: ResourceSummary,
    pub agentctl: AgentCtlSnapshot,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub orchestration: std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_plan: Option<pvisor_control::ir::run::RunPlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_observation: Option<pvisor_control::ir::run::RunObservation>,
    #[serde(default)]
    pub artifacts: Vec<BundleArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleRun {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub attempt_id: String,
    pub session_id: String,
    pub agent: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<ExecutorIdentity>,
    pub state: RunState,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: u64,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RunFailure>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub output: ProcessOutput,
    #[serde(default)]
    pub metrics: std::collections::BTreeMap<String, f64>,
    #[serde(default)]
    pub result_artifacts: Vec<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_stream_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetySummary {
    pub safe_profile_requested: bool,
    pub host_process: bool,
    pub filesystem_changes_staged: bool,
    /// Both filesystem reads and writes are confined to declared roots.
    pub filesystem_non_bypassable: bool,
    /// Filesystem reads outside declared roots are blocked by the executor.
    #[serde(default)]
    pub filesystem_read_non_bypassable: bool,
    /// Filesystem writes outside the staged workspace/capabilities are blocked.
    #[serde(default)]
    pub filesystem_write_non_bypassable: bool,
    pub network_non_bypassable: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemSummary {
    pub state: OverlayState,
    pub target: PathBuf,
    pub upper: PathBuf,
    pub changed_files: usize,
    pub whiteouts: usize,
    #[serde(default)]
    pub root_overlay: bool,
    #[serde(default)]
    pub excluded_paths: Vec<PathBuf>,
    #[serde(default)]
    pub access_policy: pvisor_control::overlay::FileAccessPolicy,
    /// Identity of the host root used as the immutable lower view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_root_device: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_root_inode: Option<u64>,
    /// Host credentials intentionally mirrored into a full-root guest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_gid: Option<u32>,
    #[serde(default)]
    pub sample_paths: Vec<String>,
    #[serde(default)]
    pub changes: Vec<ChangeEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkSummary {
    pub policy: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interception: Option<InterceptionProfile>,
    /// Present only when a driver exports final counters into the bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intercepted: Option<InterceptionSnapshot>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceSummary {
    pub requested: ResourceLimits,
    pub effective: ResourceLimits,
    #[serde(default)]
    pub mechanisms: Vec<String>,
    #[serde(default)]
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleArtifact {
    pub kind: String,
    pub path: PathBuf,
}

impl RunBundle {
    pub fn capture(
        record: &RunRecord,
        result: &RunResult,
        agentctl: AgentCtlSnapshot,
        safe_profile_requested: bool,
    ) -> anyhow::Result<Self> {
        let filesystem = record
            .overlay
            .as_ref()
            .map(|overlay| {
                let status = overlay_status(overlay)?;
                let root_overlay = overlay.target == Path::new("/");
                let root_metadata = root_overlay.then(|| fs::metadata("/")).transpose()?;
                Ok::<_, anyhow::Error>(FilesystemSummary {
                    state: overlay.state,
                    target: overlay.target.clone(),
                    upper: overlay.upper.path().to_path_buf(),
                    changed_files: status.changed_files,
                    whiteouts: status.whiteouts,
                    root_overlay,
                    excluded_paths: overlay.excluded_paths.clone(),
                    access_policy: overlay.access_policy.clone(),
                    host_root_device: root_metadata.as_ref().map(MetadataExt::dev),
                    host_root_inode: root_metadata.as_ref().map(MetadataExt::ino),
                    host_uid: root_overlay.then(|| unsafe { libc::geteuid() }),
                    host_gid: root_overlay.then(|| unsafe { libc::getegid() }),
                    sample_paths: status.sample_paths,
                    changes: overlay_changes(overlay, &record.overlay_lowers)?,
                })
            })
            .transpose()?;
        let filesystem_changes_staged = filesystem
            .as_ref()
            .is_some_and(|fs| fs.state == OverlayState::Staged);
        let host_process = record
            .executor
            .as_ref()
            .is_none_or(|executor| executor.isolation == IsolationKind::HostProcess);
        let rootless_process = record
            .executor
            .as_ref()
            .is_some_and(|executor| executor.isolation == IsolationKind::RootlessProcess);
        let seatbelt_process = record
            .executor
            .as_ref()
            .is_some_and(|executor| executor.isolation == IsolationKind::SandboxedProcess);
        let virtual_machine = record
            .executor
            .as_ref()
            .is_some_and(|executor| executor.isolation == IsolationKind::VirtualMachine);
        let enforcement = &result.executor_observations.enforcement;
        let network_non_bypassable = enforcement.is_enforced(CapabilityDimension::Network);
        let filesystem_read_non_bypassable =
            enforcement.is_enforced(CapabilityDimension::FilesystemRead);
        let filesystem_write_non_bypassable =
            enforcement.is_enforced(CapabilityDimension::FilesystemWrite);
        let filesystem_non_bypassable =
            filesystem_read_non_bypassable && filesystem_write_non_bypassable;
        let resources = resource_summary(record, result);
        let mut safety_warnings = Vec::new();
        if safe_profile_requested {
            if host_process {
                safety_warnings.push(
                    "local-process execution can access host paths outside the staged workspace"
                        .into(),
                );
            }
            if !network_non_bypassable {
                safety_warnings.push(
                    "network policy covers cooperative proxy traffic; direct sockets may bypass it"
                        .into(),
                );
            }
            if rootless_process {
                if enforcement.is_enforced(CapabilityDimension::FilesystemRead)
                    || enforcement.is_enforced(CapabilityDimension::FilesystemWrite)
                {
                    safety_warnings.push(
                        "filesystem access and process-tree cleanup are kernel-enforced; the host kernel and syscall surface remain shared"
                            .into(),
                    );
                } else {
                    safety_warnings.push(
                        "process-tree cleanup and selected network boundaries are kernel-enforced; filesystem access remains host-visible"
                            .into(),
                    );
                }
            }
            if seatbelt_process {
                if enforcement.is_enforced(CapabilityDimension::FilesystemWrite) {
                    safety_warnings.push(
                        "filesystem writes are Seatbelt-enforced; reads, the host PID namespace, syscall surface, and resource limits remain shared"
                            .into(),
                    );
                } else {
                    safety_warnings.push(
                        "selected network boundaries are Seatbelt-enforced; filesystem writes remain host-visible"
                            .into(),
                    );
                }
            }
            if virtual_machine {
                safety_warnings.push(
                    "the libkrun guest can read the complete configured rootfs and currently runs its workload as guest root"
                        .into(),
                );
            }
        }
        let mut artifacts = vec![BundleArtifact {
            kind: "run-record".into(),
            path: record.stage_dir().join("run.json"),
        }];
        for (kind, relative) in [("capture", ".capture"), ("live-markdown", "live.md")] {
            let path = record.storage.join(relative);
            if path.exists() {
                artifacts.push(BundleArtifact {
                    kind: kind.into(),
                    path,
                });
            }
        }

        Ok(Self {
            schema_version: RUN_BUNDLE_SCHEMA_VERSION,
            generated_at_unix_ms: unix_now_ms(),
            executor_observations: result.executor_observations.clone(),
            executor_plan: record.executor_plan.clone(),
            run: BundleRun {
                run_id: record.run_id.clone(),
                parent_run_id: record.parent_run_id.clone(),
                task_id: record.task_id.clone(),
                attempt_id: result.attempt_id.as_str().to_string(),
                session_id: record.session_id.clone(),
                agent: record.agent.clone(),
                command: record.command.clone(),
                executor: record.executor.clone(),
                state: result.state,
                started_at_unix_ms: result.started_at_unix_ms,
                finished_at_unix_ms: result.finished_at_unix_ms,
                duration_ms: result
                    .finished_at_unix_ms
                    .saturating_sub(result.started_at_unix_ms),
                exit_code: result.exit_code,
                failure: result.failure.clone(),
                warnings: result.warnings.clone(),
                output: result.output.clone(),
                metrics: result.metrics.clone(),
                result_artifacts: result.artifacts.clone(),
                event_stream_ref: result.event_stream_ref.clone(),
            },
            lineage: record.lineage.clone(),
            safety: SafetySummary {
                safe_profile_requested,
                host_process,
                filesystem_changes_staged,
                filesystem_non_bypassable,
                filesystem_read_non_bypassable,
                filesystem_write_non_bypassable,
                network_non_bypassable,
                warnings: safety_warnings,
            },
            filesystem,
            network: NetworkSummary {
                policy: record
                    .network_policy
                    .clone()
                    .unwrap_or_else(|| record.network.clone()),
                interception: record.network_interception.clone(),
                intercepted: record.network_interception_metrics.clone(),
            },
            environment: environment_summary(record),
            resources,
            agentctl,
            orchestration: record.orchestration.clone(),
            run_plan: record.run_plan.clone(),
            run_observation: record
                .run_plan
                .as_ref()
                .map(|plan| {
                    crate::runtime::plan::observe(
                        plan,
                        result,
                        record.network_interception_metrics.as_ref(),
                        record.filesystem_observation.as_ref(),
                    )
                })
                .transpose()?,
            artifacts,
        })
    }

    pub fn path(stage_dir: &Path) -> PathBuf {
        stage_dir.join(RUN_BUNDLE_FILENAME)
    }

    pub fn write(&self, stage_dir: &Path) -> anyhow::Result<PathBuf> {
        let path = Self::path(stage_dir);
        crate::util::write_private_json(&path, self)?;
        Ok(path)
    }

    pub fn read(stage_dir: &Path) -> anyhow::Result<Self> {
        let path = Self::path(stage_dir);
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        anyhow::ensure!(
            value["schema_version"] == RUN_BUNDLE_SCHEMA_VERSION,
            "unsupported Run Bundle schema {}; expected {}",
            value["schema_version"],
            RUN_BUNDLE_SCHEMA_VERSION
        );
        let bundle: Self = serde_json::from_value(value)?;
        Ok(bundle)
    }

    pub(crate) fn invalidate(stage_dir: &Path) -> anyhow::Result<()> {
        let path = Self::path(stage_dir);
        match fs::remove_file(&path) {
            Ok(()) => sync_directory(stage_dir),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn resource_summary(record: &RunRecord, result: &RunResult) -> ResourceSummary {
    let isolation = record.executor.as_ref().map(|executor| executor.isolation);
    let mut effective = record.resource_limits.clone();
    let mut mechanisms = Vec::new();
    let mut limitations: Vec<String> = Vec::new();
    if !result
        .executor_observations
        .enforcement
        .is_enforced(CapabilityDimension::Resources)
        && result.metrics.get("resource.posix_rlimit") != Some(&1.0)
    {
        return ResourceSummary {
            requested: record.resource_limits.clone(),
            effective: Default::default(),
            mechanisms: Vec::new(),
            limitations: vec!["executor did not observe installed resource controls".into()],
        };
    }
    if !record.resource_limits.is_empty() {
        mechanisms.extend(
            result
                .executor_observations
                .enforcement
                .evidence(CapabilityDimension::Resources)
                .into_iter()
                .flat_map(|evidence| evidence.mechanisms.clone()),
        );
        if result.metrics.get("resource.posix_rlimit") == Some(&1.0)
            && !mechanisms
                .iter()
                .any(|mechanism| mechanism == "posix-rlimit")
        {
            mechanisms.push("posix-rlimit".into());
        }
        match isolation {
            Some(IsolationKind::Container) => {}
            Some(IsolationKind::VirtualMachine) => {
                if let Some(bytes) = result.metrics.get("resource.vm_memory_bytes") {
                    effective.memory_bytes = Some(*bytes as u64);
                }
                limitations.push(
                    "process/open-file/file-size limits are inherited inside the guest helper"
                        .into(),
                );
            }
            _ => limitations.push(
                "memory and CPU rlimits are per process/address space, not aggregate cgroup accounting"
                    .into(),
            ),
        }
        if matches!(
            isolation,
            None | Some(IsolationKind::HostProcess)
                | Some(IsolationKind::RootlessProcess)
                | Some(IsolationKind::SandboxedProcess)
        ) {
            effective = effective_native_limits(&record.resource_limits);
            #[cfg(target_os = "macos")]
            if record.resource_limits.memory_bytes.is_some() {
                limitations.push("macOS does not enforce RLIMIT_AS memory limits".into());
            }
        }
        if result.metrics.get("resource.cgroup_v2") == Some(&1.0) {
            mechanisms.push("Linux cgroup v2 memory/pids controller".into());
            limitations.retain(|limitation| !limitation.contains("not aggregate cgroup"));
            if record.resource_limits.cpu_time_ms.is_some() {
                limitations.push(
                    "CPU-time budget uses inherited RLIMIT_CPU; cgroup v2 does not provide a total CPU-time ceiling"
                        .into(),
                );
            }
        }
    }
    ResourceSummary {
        requested: record.resource_limits.clone(),
        effective,
        mechanisms,
        limitations,
    }
}

#[cfg(unix)]
fn effective_native_limits(requested: &ResourceLimits) -> ResourceLimits {
    #[cfg(all(target_os = "linux", not(target_env = "musl")))]
    type RlimitResource = libc::__rlimit_resource_t;
    #[cfg(any(not(target_os = "linux"), target_env = "musl"))]
    type RlimitResource = libc::c_int;

    fn clamp(resource: RlimitResource, requested: Option<u64>) -> Option<u64> {
        let requested = requested?;
        let mut current = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if unsafe { libc::getrlimit(resource, &mut current) } != 0 {
            return None;
        }
        Some(requested.min(current.rlim_max))
    }
    ResourceLimits {
        #[cfg(not(target_os = "macos"))]
        memory_bytes: clamp(libc::RLIMIT_AS, requested.memory_bytes),
        #[cfg(target_os = "macos")]
        memory_bytes: None,
        processes: clamp(libc::RLIMIT_NPROC, requested.processes),
        cpu_time_ms: clamp(
            libc::RLIMIT_CPU,
            requested
                .cpu_time_ms
                .map(|milliseconds| milliseconds.div_ceil(1_000)),
        )
        .map(|seconds| seconds.saturating_mul(1_000)),
        open_files: clamp(libc::RLIMIT_NOFILE, requested.open_files),
        file_size_bytes: clamp(libc::RLIMIT_FSIZE, requested.file_size_bytes),
    }
}

#[cfg(not(unix))]
fn effective_native_limits(_requested: &ResourceLimits) -> ResourceLimits {
    ResourceLimits::default()
}

fn environment_summary(record: &RunRecord) -> crate::runtime::EnvironmentProjection {
    let mut summary = record.environment.clone();
    summary.runtime_injected_keys.extend(
        [
            "PVISOR_AGENT",
            "PVISOR_AGENTCTL_ENDPOINT",
            "PVISOR_AGENTCTL_TOKEN",
            "PVISOR_AGENTCTL_TRANSPORT",
            "PVISOR_AGENTCTL_VERSION",
            "PVISOR_ROLE",
            "PVISOR_RUNTIME",
            "PVISOR_STORAGE",
            "PVISOR_RUN_ID",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    summary.runtime_injected_keys.sort();
    summary.runtime_injected_keys.dedup();
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{OverlayRecord, OverlayUpper};
    use pvisor_control::{AttemptId, CapabilityEnforcementEvidence, NetworkCapability, RunId};
    use std::os::unix::fs::PermissionsExt;

    const V1_MINIMAL_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/bundles/v1-minimal.json"
    ));

    #[test]
    fn bundle_without_executor_observations_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(RUN_BUNDLE_FILENAME), V1_MINIMAL_FIXTURE).unwrap();
        assert!(RunBundle::read(temp.path()).is_err());
    }

    #[test]
    fn unsupported_bundle_schema_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let mut fixture: serde_json::Value = serde_json::from_slice(V1_MINIMAL_FIXTURE).unwrap();
        fixture["schema_version"] = serde_json::json!(999);
        fs::write(
            temp.path().join(RUN_BUNDLE_FILENAME),
            serde_json::to_vec_pretty(&fixture).unwrap(),
        )
        .unwrap();

        let error = RunBundle::read(temp.path()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported Run Bundle schema 999")
        );
    }

    #[test]
    fn bundle_is_private_and_roundtrips() {
        let temp = tempfile::tempdir().unwrap();
        let upper = temp.path().join("upper");
        fs::create_dir(&upper).unwrap();
        fs::write(upper.join("changed.txt"), b"changed").unwrap();
        let mut record = RunRecord {
            schema_version: 1,
            run_id: "run-1".into(),
            parent_run_id: Some("job-1".into()),
            task_id: Some("task-1".into()),
            session_id: "run-1".into(),
            agent: "codex".into(),
            pid: 1,
            command: vec!["codex".into()],
            executor: None,
            executor_plan: None,
            state: "completed".into(),
            started_at_unix_ms: 10,
            finished_at_unix_ms: Some(20),
            storage: temp.path().to_path_buf(),
            workspace: None,
            overlaynet_listen: None,
            network_interception: Some(InterceptionProfile::explicit_proxy()),
            network_interception_metrics: None,
            filesystem_observation: None,
            gateway_listen: None,
            network: serde_json::json!({"mode": "ambient"}),
            network_policy: None,
            environment: Default::default(),
            resource_limits: Default::default(),
            overlay: Some(OverlayRecord {
                id: "run-1".into(),
                generation: 0,
                target: temp.path().join("target"),
                upper: OverlayUpper {
                    upper_dir: upper,
                    work_dir: temp.path().join("work"),
                },
                merged_dir: temp.path().join("merged"),
                stage_dir: temp.path().to_path_buf(),
                excluded_paths: Vec::new(),
                access_policy: Default::default(),
                auto_apply: false,
                auto_discard: false,
                protect_target: false,
                state: OverlayState::Staged,
            }),
            overlay_lowers: vec![],
            lineage: None,
            orchestration: std::collections::BTreeMap::from([(
                "pvisor.orchestration.job_id".into(),
                serde_json::json!("job-1"),
            )]),
            run_plan: None,
        };
        let mut result = RunResult {
            executor_observations: Default::default(),
            run_id: RunId::new("run-1"),
            attempt_id: AttemptId::new("attempt-1"),
            lease_epoch: 1,
            state: RunState::Completed,
            started_at_unix_ms: 10,
            finished_at_unix_ms: 20,
            exit_code: Some(0),
            failure: None,
            output: Default::default(),
            value: None,
            metrics: Default::default(),
            artifacts: vec![],
            event_stream_ref: None,
            warnings: vec![],
        };
        let agentctl = AgentCtlSnapshot {
            run_id: "run-1".into(),
            attempt_id: "attempt-1".into(),
            directive: crate::AgentDirective::Continue,
            clients: vec![],
        };
        let bundle = RunBundle::capture(&record, &result, agentctl.clone(), true).unwrap();
        let path = bundle.write(temp.path()).unwrap();
        assert_eq!(RunBundle::read(temp.path()).unwrap().run.run_id, "run-1");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(bundle.filesystem.unwrap().changed_files, 1);
        assert!(!bundle.safety.network_non_bypassable);
        assert_eq!(bundle.run.parent_run_id.as_deref(), Some("job-1"));
        assert_eq!(bundle.run.task_id.as_deref(), Some("task-1"));
        assert_eq!(bundle.orchestration["pvisor.orchestration.job_id"], "job-1");
        assert!(
            bundle
                .environment
                .runtime_injected_keys
                .iter()
                .any(|key| key == "PVISOR_AGENTCTL_VERSION")
        );

        result.executor_observations = Default::default();
        record.executor = Some(ExecutorIdentity {
            name: "libkrun-root-overlay-v1".into(),
            kind: pvisor_control::ExecutorKind::VirtualMachine,
            isolation: IsolationKind::VirtualMachine,
        });
        record.network_interception = Some(InterceptionProfile::explicit_proxy());
        let cooperative_vm = RunBundle::capture(&record, &result, agentctl.clone(), true).unwrap();
        assert!(!cooperative_vm.safety.network_non_bypassable);
        record.network_interception = Some(InterceptionProfile::vm_smoltcp());
        record.executor_plan = Some(ExecutorPlan {
            name: "planned-vm".into(),
            kind: pvisor_control::ExecutorKind::VirtualMachine,
            isolation: IsolationKind::VirtualMachine,
            capability_plan: pvisor_control::CapabilityEnforcementPlan::default()
                .planned(CapabilityDimension::FilesystemRead, "planned-read")
                .planned(CapabilityDimension::FilesystemWrite, "planned-write")
                .planned(CapabilityDimension::Network, "planned-network"),
            supports_checkpoint: false,
            supports_migration: false,
        });
        let registry_json = serde_json::to_value(&record).unwrap();
        assert!(registry_json.get("executor_plan").is_none());
        assert!(registry_json.get("run_plan").is_none());
        assert!(registry_json["executor"].get("capability_plan").is_none());

        let label_only_vm = RunBundle::capture(&record, &result, agentctl.clone(), true).unwrap();
        assert!(!label_only_vm.safety.filesystem_non_bypassable);
        assert!(!label_only_vm.safety.network_non_bypassable);
        result.executor_observations.enforcement = CapabilityEnforcementEvidence::default()
            .enforced(CapabilityDimension::FilesystemRead, "test-vm-read-boundary")
            .enforced(
                CapabilityDimension::FilesystemWrite,
                "test-vm-write-boundary",
            )
            .enforced(CapabilityDimension::Network, "test-vm-network-boundary");
        let intercepted_vm = RunBundle::capture(&record, &result, agentctl.clone(), true).unwrap();
        assert!(intercepted_vm.safety.filesystem_non_bypassable);
        assert!(intercepted_vm.safety.network_non_bypassable);
        let mut vm_without_stage = record.clone();
        vm_without_stage.overlay = None;
        let vm_without_stage =
            RunBundle::capture(&vm_without_stage, &result, agentctl.clone(), true).unwrap();
        assert!(vm_without_stage.safety.filesystem_non_bypassable);

        result.executor_observations = Default::default();
        record.executor = Some(ExecutorIdentity {
            name: "local-rootless-v1".into(),
            kind: pvisor_control::ExecutorKind::Process,
            isolation: IsolationKind::RootlessProcess,
        });
        record.network = serde_json::to_value(NetworkCapability::Deny).unwrap();
        let label_only_rootless =
            RunBundle::capture(&record, &result, agentctl.clone(), true).unwrap();
        assert!(!label_only_rootless.safety.filesystem_non_bypassable);
        assert!(!label_only_rootless.safety.network_non_bypassable);
        result.executor_observations.enforcement = CapabilityEnforcementEvidence::default()
            .enforced(
                CapabilityDimension::FilesystemRead,
                "test-rootless-read-boundary",
            )
            .enforced(
                CapabilityDimension::FilesystemWrite,
                "test-rootless-write-boundary",
            )
            .enforced(
                CapabilityDimension::Network,
                "test-rootless-network-boundary",
            );
        let denied = RunBundle::capture(&record, &result, agentctl.clone(), true).unwrap();
        assert!(denied.safety.filesystem_non_bypassable);
        assert!(denied.safety.network_non_bypassable);
        assert!(
            denied
                .safety
                .warnings
                .iter()
                .all(|warning| !warning.contains("direct sockets may bypass"))
        );

        result.executor_observations = Default::default();
        record.executor = Some(ExecutorIdentity {
            name: "local-seatbelt-v1".into(),
            kind: pvisor_control::ExecutorKind::Process,
            isolation: IsolationKind::SandboxedProcess,
        });
        result.executor_observations.enforcement = CapabilityEnforcementEvidence::default()
            .enforced(
                CapabilityDimension::FilesystemWrite,
                "test-seatbelt-write-boundary",
            )
            .enforced(
                CapabilityDimension::Network,
                "test-seatbelt-network-boundary",
            );
        result.warnings.push("sandbox_setup_failed".into());
        let seatbelt = RunBundle::capture(&record, &result, agentctl, true).unwrap();
        assert!(!seatbelt.safety.filesystem_non_bypassable);
        assert!(!seatbelt.safety.filesystem_read_non_bypassable);
        assert!(seatbelt.safety.filesystem_write_non_bypassable);
        assert!(seatbelt.safety.network_non_bypassable);
        assert!(
            seatbelt
                .safety
                .warnings
                .iter()
                .any(|warning| warning.contains("reads"))
        );
    }
}
