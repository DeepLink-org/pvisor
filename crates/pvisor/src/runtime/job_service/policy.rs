//! Secret-free, runtime-resolved workspace launch policy, not an enforcement claim.
//! RunConfig supplied to a managed start does not configure the embedded PVisor.
use crate::config::{
    FilesystemMode, GatewayMode, OverlayNetMode, OverlayNetPolicy, RunConfig, RunPolicy, RunStdio,
};
use crate::runtime::{RunRecord, run::ResolvedRun};
use anyhow::Context;
use pvisor_core::{
    ExecutorKind, NetworkCapability, NetworkDefaultAction, PolicyMode, RunInvocation,
    RuntimeConfig, SessionPolicies, StdioMode,
};
use serde::{Deserialize, Serialize};

const FILENAME: &str = "workspace-launch-policy.json";

/// Inherited launch inputs are complete, including deliberately absent scopes.
/// Keep provenance separate from policy layers: even an identity layer can change
/// executor isolation/evidence selected by the final capability's representation.
#[derive(Clone, Copy)]
pub enum PolicySource {
    CurrentDefaults,
    Inherited(pvisor_core::IsolationKind),
}

impl PolicySource {
    pub fn load_defaults(
        self,
        config: &mut RunConfig,
        workspace: &std::path::Path,
        user_root: Option<&std::path::Path>,
    ) -> anyhow::Result<()> {
        match self {
            Self::CurrentDefaults => config.load_policy_defaults(workspace, user_root),
            Self::Inherited(_) => Ok(()),
        }
    }

    pub fn validate_executor(self, executor: &pvisor_core::ExecutorPlan) -> anyhow::Result<()> {
        if let Self::Inherited(parent) = self {
            anyhow::ensure!(
                executor.kind == ExecutorKind::Process
                    && (parent == pvisor_core::IsolationKind::HostProcess
                        || executor.isolation == parent),
                "workspace fork cannot preserve parent executor boundary {parent:?} with {:?}; refusing best-effort downgrade",
                executor.isolation
            );
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchPolicy {
    version: u32,
    run_id: String,
    attempt_id: Option<String>,
    unsupported: Option<String>,
    runtime: RuntimeConfig,
    filesystem: FilesystemMode,
    grants: Vec<pvisor_core::FilesystemCapability>,
    network: pvisor_core::NetworkConfig,
    policies: SessionPolicies,
    inherit_env: bool,
    pass_env: Vec<String>,
    stdio: RunStdio,
    required_sandbox: bool,
}

pub(super) fn persist(record: &RunRecord, resolved: &ResolvedRun) -> anyhow::Result<()> {
    let spec = &resolved.spec;
    let RunInvocation::Process(process) = &spec.invocation;
    let filesystem = spec
        .metadata
        .get("pvisor.filesystem.mode")
        .and_then(|v| v.as_str());
    let defaults = RuntimeConfig::default();
    let unsupported = if resolved.descriptor.kind != ExecutorKind::Process
        || !matches!(
            resolved.descriptor.name.as_str(),
            "local-process-v1" | "local-rootless-v1" | "local-seatbelt-v1"
        ) {
        Some("workspace launch policy cannot reconstruct this executor (VM/container/custom executor)".into())
    } else if !matches!(filesystem, Some("host" | "sandbox")) {
        Some("workspace launch policy lacks an explicit filesystem mode".into())
    } else if record.gateway_listen.is_some() {
        Some("workspace launch policy cannot safely reconstruct Gateway routes/credentials".into())
    } else if spec
        .metadata
        .get(crate::executor::sandbox::LANDLOCK_SANDBOX_KEY)
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        && !spec
            .metadata
            .get(crate::executor::sandbox::REQUIRED_SANDBOX_KEY)
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        Some("workspace launch policy cannot reconstruct standalone Landlock requirement".into())
    } else if spec.runtime.cpu_qos.is_some()
        || spec.runtime.termination_grace_ms != defaults.termination_grace_ms
        || spec.runtime.max_output_bytes != defaults.max_output_bytes
        || !spec.capabilities.models.is_empty()
        || !spec.capabilities.tools.is_empty()
        || !spec.capabilities.secrets.is_empty()
        || spec.capabilities.allow_subprocess
    {
        Some("workspace launch policy contains controls not expressible by RunConfig".into())
    } else if process.stdout != process.stderr
        || process.stdin != StdioMode::Inherit
        || !matches!(process.stdout, StdioMode::Inherit | StdioMode::Capture)
    {
        Some("workspace launch policy cannot reconstruct process stdio".into())
    } else if spec
        .metadata
        .get("pvisor.stage")
        .and_then(|v| v.get("size_limit_bytes"))
        .is_some_and(|v| !v.is_null())
    {
        Some("workspace launch policy cannot attest an aggregate stage size budget".into())
    } else {
        None
    };
    let policy = LaunchPolicy {
        version: 1,
        run_id: record.run_id.clone(),
        attempt_id: record.attempt_id.clone(),
        unsupported,
        runtime: spec.runtime.clone(),
        filesystem: if filesystem == Some("sandbox") {
            FilesystemMode::Sandbox
        } else {
            FilesystemMode::Host
        },
        grants: spec.capabilities.filesystem.clone(),
        network: record
            .network_policy
            .as_ref()
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .context("prepared network policy is not reconstructible")?
            .unwrap_or_else(|| resolved.preparation.network.clone()),
        policies: spec.policies.clone(),
        inherit_env: process.inherit_env,
        // Names only: never retain environment values, command, arbitrary metadata or config routes.
        pass_env: resolved.preparation.environment.projected_keys.clone(),
        stdio: if process.stdout == StdioMode::Capture {
            RunStdio::Capture
        } else {
            RunStdio::Inherit
        },
        required_sandbox: spec
            .metadata
            .get(crate::executor::sandbox::REQUIRED_SANDBOX_KEY)
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    };
    crate::util::write_private_json(&record.stage_dir().join(FILENAME), &policy)
}

pub fn workspace_config(record: &RunRecord) -> anyhow::Result<(RunConfig, bool)> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    const MAX_BYTES: u64 = 1024 * 1024;
    let file = std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(record.stage_dir().join(FILENAME)).context(
        "workspace fork requires persisted runtime-resolved launch policy; legacy RunRecord/Bundle or Job config/spec alone is insufficient; start a new parent Job")?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0
            && metadata.len() <= MAX_BYTES,
        "workspace launch policy must be a private regular file owned by the current user, at most 1 MiB"
    );
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_BYTES,
        "workspace launch policy exceeds 1 MiB"
    );
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .context("invalid workspace launch policy; refusing default-policy fallback")?;
    let policy: LaunchPolicy = serde_json::from_value(value.clone())
        .context("invalid workspace launch policy; refusing default-policy fallback")?;
    // Nested shared DTOs have serde defaults. A partial snapshot must not silently
    // acquire unlimited resources or empty policy layers through those defaults.
    anyhow::ensure!(
        serde_json::to_value(&policy)? == value,
        "incomplete or noncanonical workspace launch policy; refusing default-policy fallback"
    );
    policy.config(record)
}

impl LaunchPolicy {
    fn config(self, record: &RunRecord) -> anyhow::Result<(RunConfig, bool)> {
        anyhow::ensure!(
            self.version == 1,
            "unsupported workspace launch policy version {}",
            self.version
        );
        anyhow::ensure!(
            self.run_id == record.run_id && self.attempt_id == record.attempt_id,
            "workspace launch policy belongs to a different Job/Attempt"
        );
        if let Some(reason) = self.unsupported {
            anyhow::bail!("workspace fork refused: {reason}");
        }
        anyhow::ensure!(
            record.gateway_listen.is_none(),
            "workspace fork cannot reconstruct Gateway"
        );
        let defaults = RuntimeConfig::default();
        anyhow::ensure!(
            self.runtime.cpu_qos.is_none()
                && self.runtime.termination_grace_ms == defaults.termination_grace_ms
                && self.runtime.max_output_bytes == defaults.max_output_bytes,
            "workspace fork cannot preserve saved runtime controls with this launcher's defaults"
        );
        let mut config = RunConfig {
            filesystem: self.filesystem,
            ..Default::default()
        };
        config.run.timeout_ms = self.runtime.timeout_ms;
        config.run.resource_limits = self.runtime.resource_limits;
        config.run.policy = if self.runtime.policy_mode == PolicyMode::Enforce {
            RunPolicy::Enforce
        } else {
            RunPolicy::Observe
        };
        config.run.inherit_env = self.inherit_env;
        config.run.pass_env = self.pass_env;
        config.run.stdio = self.stdio;
        config.run.filesystem = self.grants;
        config.gateway.mode = GatewayMode::Off;
        // Listeners are attempt-local addresses, not policy. Do not reuse parent ports.
        config.gateway.admin_listen = "127.0.0.1:0".into();
        config.overlaynet.listen = "127.0.0.1:0".into();
        config.overlaynet.mode = if record.overlaynet_listen.is_some() {
            OverlayNetMode::Proxy
        } else {
            OverlayNetMode::Off
        };
        config.policies = self.policies;
        let supplemental_limits = self.network.limits.clone();
        let mut capability = self
            .network
            .capability
            .context("workspace launch policy lacks resolved network capability")?;
        while let NetworkCapability::Scoped { layers, fallback } = capability {
            for (scope, policy) in layers {
                let layer = match scope {
                    pvisor_core::PolicyScope::Session => &mut config.policies.session,
                    pvisor_core::PolicyScope::Workspace => &mut config.policies.workspace,
                    pvisor_core::PolicyScope::User => &mut config.policies.user,
                };
                layer.network = Some(policy);
            }
            capability = *fallback;
        }
        match capability {
            NetworkCapability::Ambient => config.overlaynet.policy = OverlayNetPolicy::Public,
            NetworkCapability::Deny => config.overlaynet.policy = OverlayNetPolicy::Deny,
            NetworkCapability::AllowList { hosts, rules } => {
                config.overlaynet.policy = OverlayNetPolicy::Allowlist;
                config.overlaynet.allow = hosts;
                config.overlaynet.rules = rules;
            }
            NetworkCapability::Policy {
                default_action,
                allow,
                deny,
                limits,
            } => {
                config.overlaynet.policy = if default_action == NetworkDefaultAction::Allow {
                    OverlayNetPolicy::Public
                } else {
                    OverlayNetPolicy::Allowlist
                };
                config.overlaynet.rules = allow;
                config.overlaynet.deny = deny;
                config.overlaynet.limits = limits;
            }
            NetworkCapability::Scoped { .. } => unreachable!(),
        }
        for limit in supplemental_limits {
            if !config.overlaynet.limits.contains(&limit) {
                config.overlaynet.limits.push(limit);
            }
        }

        Ok((config, self.required_sandbox))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NetworkDriverConfig, PVisor};
    use pvisor_core::{NetworkAccessRule, NetworkBandwidthLimit, NetworkMode, RunSpec};
    use std::path::Path;

    fn record(root: &Path) -> RunRecord {
        serde_json::from_value(serde_json::json!({
            "schema_version":1,"run_id":"parent","attempt_id":"attempt-parent",
            "session_id":"parent","agent":"probe","pid":0,"command":["probe"],
            "state":"completed","started_at_unix_ms":1,"finished_at_unix_ms":2,
            "storage":root,"network":{},"gateway_listen":null,"overlay":null
        }))
        .unwrap()
    }

    fn spec() -> RunSpec {
        let mut spec = RunSpec::process("parent", "probe", "/bin/true");
        spec.metadata
            .insert("pvisor.filesystem.mode".into(), "host".into());
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.stdin = StdioMode::Inherit;
        process.stdout = StdioMode::Capture;
        process.stderr = StdioMode::Capture;
        spec
    }

    fn save(root: &Path, spec: RunSpec, network: pvisor_core::NetworkConfig) -> RunRecord {
        let visor = PVisor::builder()
            .network(NetworkDriverConfig::new(OverlayNetMode::Proxy, network))
            .build();
        let resolved = visor.resolve_run(spec).unwrap();
        let mut record = record(root);
        record.overlaynet_listen = Some("127.0.0.1:1234".into());
        record.network_policy = Some(serde_json::to_value(&resolved.preparation.network).unwrap());
        persist(&record, &resolved).unwrap();
        record
    }

    #[test]
    fn workspace_launch_policy_preserves_network_resources_and_environment_names() {
        let root = tempfile::tempdir().unwrap();
        let mut spec = spec();
        spec.runtime.timeout_ms = Some(1234);
        spec.runtime.resource_limits.memory_bytes = Some(512 * 1024 * 1024);
        spec.runtime.resource_limits.processes = Some(16);
        spec.runtime.resource_limits.cpu_time_ms = Some(2000);
        spec.runtime.resource_limits.open_files = Some(32);
        spec.runtime.resource_limits.file_size_bytes = Some(4096);
        spec.capabilities
            .filesystem
            .push(pvisor_core::FilesystemCapability {
                path: "/opt/tools".into(),
                access: pvisor_core::FilesystemAccess::Read,
            });
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.inherit_env = false;
        process
            .env
            .insert("TOKEN".into(), "secret-env-value".into());
        process.args.push("secret-command-value".into());
        spec.metadata
            .insert("unrelated.secret".into(), "secret-metadata-value".into());
        let rule = NetworkAccessRule {
            host: "api.example".into(),
            ports: vec![443],
            transports: vec![],
            allow_private_ips: false,
        };
        let limit = NetworkBandwidthLimit {
            host: Some("api.example".into()),
            port: Some(443),
            bytes_per_second: 128,
        };
        let network = pvisor_core::NetworkConfig {
            mode: NetworkMode::Allowlist,
            rules: vec![rule.clone()],
            deny_rules: vec![NetworkAccessRule {
                host: "blocked.example".into(),
                ..rule.clone()
            }],
            limits: vec![limit.clone()],
            ..Default::default()
        };
        let record = save(root.path(), spec.clone(), network.clone());
        let (child, safe) = workspace_config(&record).unwrap();
        assert!(!safe);
        assert_eq!(child.run.timeout_ms, spec.runtime.timeout_ms);
        assert_eq!(child.run.resource_limits, spec.runtime.resource_limits);
        assert_eq!(child.run.filesystem, spec.capabilities.filesystem);
        assert_eq!(child.overlaynet.policy, OverlayNetPolicy::Allowlist);
        assert_eq!(child.overlaynet.rules, network.rules);
        assert_eq!(child.overlaynet.deny, network.deny_rules);
        assert_eq!(child.overlaynet.limits, vec![limit]);
        assert!(!child.run.inherit_env);
        assert_eq!(child.run.pass_env, vec!["TOKEN"]);
        assert_eq!(child.run.stdio, RunStdio::Capture);
        let text = std::fs::read_to_string(root.path().join(FILENAME)).unwrap();
        for secret in [
            "secret-env-value",
            "secret-command-value",
            "secret-metadata-value",
        ] {
            assert!(!text.contains(secret));
        }
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(root.path().join(FILENAME))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn workspace_launch_policy_blocks_new_ambient_defaults_and_preserves_scopes() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let user = root.path().join("user");
        std::fs::create_dir_all(workspace.join(".pvisor")).unwrap();
        std::fs::create_dir_all(user.join("pvisor")).unwrap();
        let changed =
            "[network]\ndefault_action = 'deny'\n[filesystem]\ndeny = ['new-secret/**']\n";
        std::fs::write(workspace.join(".pvisor/policy.toml"), changed).unwrap();
        std::fs::write(user.join("pvisor/policy.toml"), changed).unwrap();
        let record = save(
            root.path(),
            spec(),
            pvisor_core::NetworkConfig {
                mode: NetworkMode::NoNetwork,
                ..Default::default()
            },
        );
        let (mut child, _) = workspace_config(&record).unwrap();
        let before = child.policies.clone();
        PolicySource::Inherited(pvisor_core::IsolationKind::HostProcess)
            .load_defaults(&mut child, &workspace, Some(&user))
            .unwrap();
        assert_eq!(before, child.policies);
        assert_eq!(child.policies, SessionPolicies::default());
        assert_eq!(child.overlaynet.policy, OverlayNetPolicy::Deny);
        let mut fresh = RunConfig::default();
        PolicySource::CurrentDefaults
            .load_defaults(&mut fresh, &workspace, Some(&user))
            .unwrap();
        assert_ne!(fresh.policies, child.policies);
        // Inherited execution must not even parse current policy files.
        std::fs::write(workspace.join(".pvisor/policy.toml"), "not valid TOML [").unwrap();
        PolicySource::Inherited(pvisor_core::IsolationKind::HostProcess)
            .load_defaults(&mut child, &workspace, Some(&user))
            .unwrap();
        assert_eq!(before, child.policies);
    }

    #[test]
    fn workspace_launch_policy_rejects_legacy_partial_foreign_and_future_records() {
        let root = tempfile::tempdir().unwrap();
        let record = record(root.path());
        assert!(
            workspace_config(&record)
                .unwrap_err()
                .to_string()
                .contains("legacy")
        );
        let record = save(root.path(), spec(), Default::default());
        let path = root.path().join(FILENAME);
        let original: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for field in [
            "resource_limits",
            "policy_mode",
            "termination_grace_ms",
            "max_output_bytes",
        ] {
            let mut value = original.clone();
            value["runtime"].as_object_mut().unwrap().remove(field);
            crate::util::write_private_json(&path, &value).unwrap();
            assert!(workspace_config(&record).is_err(), "missing {field}");
        }
        for (field, value) in [
            ("run_id", serde_json::json!("other")),
            ("attempt_id", serde_json::json!("other")),
            ("version", serde_json::json!(2)),
        ] {
            let mut altered = original.clone();
            altered[field] = value;
            crate::util::write_private_json(&path, &altered).unwrap();
            assert!(workspace_config(&record).is_err(), "changed {field}");
        }
        std::fs::write(path, b"{broken").unwrap();
        assert!(workspace_config(&record).is_err());
    }

    #[test]
    fn workspace_launch_policy_refuses_unrepresentable_controls_and_gateway() {
        let root = tempfile::tempdir().unwrap();
        let mut custom = spec();
        custom.runtime.max_output_bytes = 3;
        let record = save(root.path(), custom, Default::default());
        assert!(
            workspace_config(&record)
                .unwrap_err()
                .to_string()
                .contains("not expressible")
        );
        let mut record = save(root.path(), spec(), Default::default());
        record.gateway_listen = Some("127.0.0.1:1234".into());
        assert!(workspace_config(&record).is_err());
    }

    #[test]
    fn workspace_launch_policy_preserves_scoped_decisions_and_runtime_bandwidth_limits() {
        let root = tempfile::tempdir().unwrap();
        let mut parent = spec();
        let rule = NetworkAccessRule {
            host: "api.example".into(),
            ports: vec![443],
            transports: vec![],
            allow_private_ips: false,
        };
        parent.policies.user.network = Some(pvisor_core::NetworkPolicyLayer {
            allow: vec![rule.clone()],
            ..Default::default()
        });
        parent.policies.workspace.filesystem =
            Some(pvisor_core::FileAccessPolicy::new(vec!["secret/**".into()], vec![]).unwrap());
        let visor = PVisor::builder()
            .network(NetworkDriverConfig::new(
                OverlayNetMode::Proxy,
                pvisor_core::NetworkConfig {
                    mode: NetworkMode::Allowlist,
                    rules: vec![rule],
                    ..Default::default()
                },
            ))
            .overlay(crate::OverlayHint {
                lower_dirs: vec![root.path().into()],
                ..Default::default()
            })
            .build();
        let resolved = visor.resolve_run(parent.clone()).unwrap();
        let mut network = resolved.preparation.network.clone();
        // This limit is attached by the runtime, not present in the resolved capability.
        let limit = NetworkBandwidthLimit {
            host: None,
            port: None,
            bytes_per_second: 64,
        };
        network.limits.push(limit.clone());
        let mut record = record(root.path());
        record.overlaynet_listen = Some("127.0.0.1:1234".into());
        record.network_policy = Some(serde_json::to_value(&network).unwrap());
        persist(&record, &resolved).unwrap();
        let (child, _) = workspace_config(&record).unwrap();
        assert_eq!(child.policies.user.network, parent.policies.user.network);
        assert_eq!(
            child.policies.workspace.filesystem,
            parent.policies.workspace.filesystem
        );
        assert_eq!(child.overlaynet.limits, vec![limit]);
        let mut reconstructed = pvisor_core::NetworkConfig {
            mode: NetworkMode::Allowlist,
            rules: child.overlaynet.rules,
            allowed_hosts: child.overlaynet.allow,
            deny_rules: child.overlaynet.deny,
            limits: child.overlaynet.limits,
            capability: None,
        };
        reconstructed.capability = Some(
            child
                .policies
                .network(pvisor_core::network::network_capability(&reconstructed)),
        );
        let original = pvisor_core::NetworkPolicy::compile(&network).unwrap();
        let forked = pvisor_core::NetworkPolicy::compile(&reconstructed).unwrap();
        for (host, port, allowed) in [
            ("api.example", 443, true),
            ("api.example", 80, false),
            ("other.example", 443, false),
        ] {
            let request = pvisor_core::NetworkAccessRequest {
                run_id: None,
                attempt_id: None,
                host: host.into(),
                port: Some(port),
                transport: pvisor_core::NetworkTransport::TcpTunnel,
                resolved_ip: None,
            };
            assert_eq!(original.preflight(&request).is_ok(), allowed);
            assert_eq!(forked.preflight(&request).is_ok(), allowed);
        }
        assert_eq!(
            original.matching_limits("api.example", Some(443), &[]),
            forked.matching_limits("api.example", Some(443), &[])
        );
    }

    #[test]
    fn workspace_launch_policy_preserves_missing_network_driver_and_refuses_vm() {
        let root = tempfile::tempdir().unwrap();
        let visor = PVisor::builder()
            .network(NetworkDriverConfig::new(
                OverlayNetMode::Off,
                Default::default(),
            ))
            .build();
        let mut resolved = visor.resolve_run(spec()).unwrap();
        let mut record = record(root.path());
        persist(&record, &resolved).unwrap();
        let (child, _) = workspace_config(&record).unwrap();
        assert_eq!(child.overlaynet.mode, OverlayNetMode::Off);
        assert_eq!(child.policies, SessionPolicies::default());
        record.overlaynet_listen = Some("127.0.0.1:1234".into());
        resolved.descriptor.kind = ExecutorKind::VirtualMachine;
        persist(&record, &resolved).unwrap();
        assert!(
            workspace_config(&record)
                .unwrap_err()
                .to_string()
                .contains("VM/container/custom")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn workspace_launch_policy_preserves_final_network_isolation_and_enforcement() {
        use crate::executor::{process::ProcessExecutor, sandbox::NetworkIsolation};
        use pvisor_core::{CapabilityDimension, EnforcementPlanLevel};
        use std::sync::Arc;

        let root = tempfile::tempdir().unwrap();
        // Admission/launcher planning only: no namespaces or workload are started.
        let executor = Arc::new(ProcessExecutor::rootless_with_launcher("/bin/true").unwrap());
        let resolve = |config: &RunConfig, safe: bool| {
            let network = pvisor_core::NetworkConfig {
                mode: match config.overlaynet.policy {
                    OverlayNetPolicy::Public => NetworkMode::Public,
                    OverlayNetPolicy::Deny => NetworkMode::NoNetwork,
                    OverlayNetPolicy::Allowlist => NetworkMode::Allowlist,
                },
                allowed_hosts: config.overlaynet.allow.clone(),
                rules: config.overlaynet.rules.clone(),
                deny_rules: config.overlaynet.deny.clone(),
                limits: config.overlaynet.limits.clone(),
                capability: None,
            };
            let visor = PVisor::builder()
                .executors(vec![executor.clone()])
                .network(NetworkDriverConfig::new(config.overlaynet.mode, network))
                .overlay(crate::OverlayHint {
                    lower_dirs: vec![root.path().into()],
                    ..Default::default()
                })
                .build();
            let mut spec = spec();
            spec.metadata.insert(
                "pvisor.filesystem.mode".into(),
                serde_json::to_value(config.filesystem).unwrap(),
            );
            if safe {
                spec.metadata.insert(
                    crate::executor::sandbox::REQUIRED_SANDBOX_KEY.into(),
                    true.into(),
                );
                spec.metadata.insert(
                    crate::executor::sandbox::LANDLOCK_SANDBOX_KEY.into(),
                    true.into(),
                );
            }
            spec.policies = config.policies.clone();
            spec.runtime.timeout_ms = config.run.timeout_ms;
            spec.runtime.resource_limits = config.run.resource_limits.clone();
            spec.runtime.policy_mode = if config.run.policy == RunPolicy::Enforce {
                PolicyMode::Enforce
            } else {
                PolicyMode::Audit
            };
            spec.capabilities.filesystem = config.run.filesystem.clone();
            let RunInvocation::Process(process) = &mut spec.invocation;
            process.inherit_env = config.run.inherit_env;
            visor.resolve_run(spec).unwrap()
        };
        for filesystem in [FilesystemMode::Host, FilesystemMode::Sandbox] {
            for (policy, scoped, safe) in [
                (OverlayNetPolicy::Deny, false, false),
                (
                    OverlayNetPolicy::Deny,
                    false,
                    filesystem == FilesystemMode::Sandbox,
                ),
                (OverlayNetPolicy::Public, false, false),
                (OverlayNetPolicy::Allowlist, true, false),
            ] {
                let mut config = RunConfig {
                    filesystem,
                    ..Default::default()
                };
                config.overlaynet.mode = OverlayNetMode::Proxy;
                config.overlaynet.policy = policy;
                if scoped {
                    config.overlaynet.allow = vec!["api.example".into()];
                    config.policies.user.network = Some(pvisor_core::NetworkPolicyLayer {
                        allow: vec![NetworkAccessRule {
                            host: "api.example".into(),
                            ports: vec![443],
                            transports: vec![],
                            allow_private_ips: false,
                        }],
                        ..Default::default()
                    });
                    config.policies.workspace.filesystem = Some(
                        pvisor_core::FileAccessPolicy::new(vec!["secret/**".into()], vec![])
                            .unwrap(),
                    );
                }
                config.run.timeout_ms = Some(1234);
                config.run.resource_limits.open_files = Some(32);
                config.run.filesystem = vec![pvisor_core::FilesystemCapability {
                    path: "/opt/tools".into(),
                    access: pvisor_core::FilesystemAccess::Read,
                }];
                let parent = resolve(&config, safe);
                let mut record = record(root.path());
                record.executor = Some((&parent.descriptor).into());
                record.overlaynet_listen = Some("127.0.0.1:1234".into());
                record.network_policy =
                    Some(serde_json::to_value(&parent.preparation.network).unwrap());
                persist(&record, &parent).unwrap();
                let (mut child_config, child_safe) = workspace_config(&record).unwrap();
                let source = PolicySource::Inherited(parent.descriptor.isolation);
                source
                    .load_defaults(&mut child_config, root.path(), None)
                    .unwrap();
                let child = resolve(&child_config, child_safe);
                source.validate_executor(&child.descriptor).unwrap();
                let parent_isolation =
                    crate::executor::process::network_isolation(&parent.spec).unwrap();
                let child_isolation =
                    crate::executor::process::network_isolation(&child.spec).unwrap();
                assert_eq!(parent_isolation, child_isolation);
                assert_eq!(
                    parent.spec.capabilities.network,
                    child.spec.capabilities.network
                );
                if policy == OverlayNetPolicy::Deny {
                    assert_eq!(child.spec.capabilities.network, NetworkCapability::Deny);
                    assert_eq!(child_isolation, NetworkIsolation::LoopbackOnly);
                    let enforcement =
                        &child.descriptor.capability_plan.dimensions[&CapabilityDimension::Network];
                    assert_eq!(enforcement.level, EnforcementPlanLevel::Planned);
                    assert!(
                        enforcement
                            .mechanisms
                            .iter()
                            .any(|m| m == "linux-network-namespace")
                    );
                }
                // Check the complete final plan, including filesystem and resource strength.
                assert_eq!(
                    serde_json::to_value(&parent.descriptor.capability_plan).unwrap(),
                    serde_json::to_value(&child.descriptor.capability_plan).unwrap()
                );
                assert_eq!(
                    parent.spec.runtime.resource_limits,
                    child.spec.runtime.resource_limits
                );
                assert_eq!(
                    parent.spec.runtime.policy_mode,
                    child.spec.runtime.policy_mode
                );
                assert_eq!(
                    parent.spec.capabilities.filesystem,
                    child.spec.capabilities.filesystem
                );
                assert_eq!(child_config.filesystem, filesystem);
                assert_eq!(child_safe, safe);
            }
        }
    }

    #[test]
    fn workspace_launch_policy_refuses_runtime_default_drift() {
        let root = tempfile::tempdir().unwrap();
        let record = save(root.path(), spec(), Default::default());
        let path = root.path().join(FILENAME);
        let original: LaunchPolicy =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let original = serde_json::to_vec(&original).unwrap();
        for output_budget in [false, true] {
            let mut policy: LaunchPolicy = serde_json::from_slice(&original).unwrap();
            if output_budget {
                policy.runtime.max_output_bytes += 1;
            } else {
                policy.runtime.termination_grace_ms += 1;
            }
            crate::util::write_private_json(&path, &policy).unwrap();
            assert!(
                workspace_config(&record)
                    .unwrap_err()
                    .to_string()
                    .contains("cannot preserve saved runtime controls")
            );
        }
    }

    #[test]
    fn workspace_launch_policy_projection_preserves_enforce_mode() {
        let root = tempfile::tempdir().unwrap();
        let record = save(root.path(), spec(), Default::default());
        let path = root.path().join(FILENAME);
        let mut policy: LaunchPolicy =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // Projection test only: standard host admission may reject Enforce for
        // unsupported dimensions, but restoration must never turn it into Audit.
        policy.runtime.policy_mode = PolicyMode::Enforce;
        crate::util::write_private_json(&path, &policy).unwrap();
        let (child, _) = workspace_config(&record).unwrap();
        assert_eq!(child.run.policy, RunPolicy::Enforce);
    }

    #[test]
    fn workspace_launch_policy_refuses_executor_boundary_downgrade() {
        use crate::executor::{RunExecutor, process::ProcessExecutor};
        use pvisor_core::IsolationKind;
        let mut executor = ProcessExecutor::default().descriptor();
        PolicySource::CurrentDefaults
            .validate_executor(&executor)
            .unwrap();
        PolicySource::Inherited(IsolationKind::HostProcess)
            .validate_executor(&executor)
            .unwrap();
        for isolation in [
            IsolationKind::RootlessProcess,
            IsolationKind::SandboxedProcess,
        ] {
            let source = PolicySource::Inherited(isolation);
            assert!(source.validate_executor(&executor).is_err());
            executor.isolation = isolation;
            source.validate_executor(&executor).unwrap();
            executor.isolation = IsolationKind::HostProcess;
        }
    }

    #[tokio::test]
    async fn workspace_launch_policy_uses_actual_runtime_not_managed_config() {
        let root = tempfile::tempdir().unwrap();
        let visor = PVisor::builder()
            .storage(root.path())
            .network(NetworkDriverConfig::new(
                OverlayNetMode::Proxy,
                pvisor_core::NetworkConfig {
                    mode: NetworkMode::NoNetwork,
                    ..Default::default()
                },
            ))
            .build();
        let mut requested = spec();
        requested.runtime.timeout_ms = Some(1234);
        let mut unrelated = RunConfig::default();
        unrelated.run.executor = crate::RunExecutorKind::Vm;
        unrelated.run.timeout_ms = Some(9999);
        unrelated.gateway.routes = vec![];
        let run = super::super::RuntimeJobService::start_managed(&visor, requested, unrelated)
            .await
            .unwrap();
        run.wait().await.unwrap();
        let record = RunRecord::read(root.path()).unwrap();
        let (child, _) = workspace_config(&record).unwrap();
        assert_eq!(child.run.executor, crate::RunExecutorKind::Host);
        assert_eq!(child.run.timeout_ms, Some(1234));
        assert_eq!(child.overlaynet.policy, OverlayNetPolicy::Deny);
    }
}
