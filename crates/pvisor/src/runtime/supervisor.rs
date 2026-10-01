#[cfg(feature = "gateway")]
use super::attempt::{AttemptPrepareOpts, prepare_attempt};
use super::attempt::{
    AttemptSession, OverlayAttemptPrepareOpts, apply_implant, prepare_overlay_attempt,
    prepare_storage_attempt,
};
use super::implant::{ImplantPlan, OverlayHint};
#[cfg(feature = "gateway")]
use crate::GatewayDriverConfig;
#[cfg(feature = "gateway")]
use crate::TrajectoryEventSink;
use crate::{NetworkDriverConfig, OverlayNetMode};
use pvisor_core::{AttemptId, NetworkCapability, RunSpec};
use pvisor_core::{ControlController, PolicyControlController};
#[cfg(feature = "gateway")]
use pvisor_gateway::config::ProxyConfig;
use std::path::PathBuf;
use std::sync::Arc;

/// Runtime mechanisms available on this host, not the installed controls for a Run.
///
/// `network` and `filesystem` report non-bypassable policy enforcement, not
/// proxy injection or a staged filesystem projection.
#[derive(Debug, Clone)]
pub struct RuntimeCapabilities {
    pub agentctl: bool,
    pub gateway: bool,
    pub network: bool,
    pub filesystem: bool,
    pub providers: Vec<&'static str>,
    /// Network interception support available to a VM Attempt. This is not a
    /// claim that every configured executor is currently enforcing it.
    pub vm_network: bool,
}

impl Default for RuntimeCapabilities {
    fn default() -> Self {
        let vm_network = vm_network_supported();
        let mut providers = vec![
            "local-process",
            "agentctl-unix-v1",
            "overlaynet-explicit-proxy",
            "fs-overlay-staging",
        ];
        if cfg!(feature = "gateway") {
            providers.push("in-process-capture");
        }
        if vm_network {
            providers.push("overlaynet-vm-smoltcp");
        }
        Self {
            agentctl: true,
            gateway: cfg!(feature = "gateway"),
            network: false,
            filesystem: false,
            providers,
            vm_network,
        }
    }
}

fn vm_network_supported() -> bool {
    cfg!(any(
        target_os = "linux",
        all(target_os = "macos", target_arch = "aarch64")
    ))
}

fn network_config_from_capability(
    capability: &NetworkCapability,
) -> pvisor_overlaynet::NetworkConfig {
    use pvisor_core::NetworkDefaultAction;
    use pvisor_overlaynet::NetworkMode;

    match capability {
        NetworkCapability::Scoped { .. } => pvisor_core::NetworkConfig {
            capability: Some(capability.clone()),
            ..Default::default()
        },
        NetworkCapability::Ambient => pvisor_overlaynet::NetworkConfig::default(),
        NetworkCapability::Deny => pvisor_overlaynet::NetworkConfig {
            mode: NetworkMode::NoNetwork,
            ..Default::default()
        },
        NetworkCapability::AllowList { hosts, rules } => pvisor_overlaynet::NetworkConfig {
            mode: NetworkMode::Allowlist,
            allowed_hosts: hosts.clone(),
            rules: rules.clone(),
            ..Default::default()
        },
        NetworkCapability::Policy {
            default_action,
            allow,
            deny,
            limits,
        } => pvisor_overlaynet::NetworkConfig {
            capability: None,
            mode: match default_action {
                NetworkDefaultAction::Allow => NetworkMode::Public,
                NetworkDefaultAction::Deny => NetworkMode::Allowlist,
            },
            allowed_hosts: Vec::new(),
            rules: allow.clone(),
            deny_rules: deny.clone(),
            limits: limits.clone(),
        },
    }
}

/// Builder for Attempt prepare options (capture / overlay). Crate-private;
/// public configuration goes through [`crate::PVisorBuilder`].
#[derive(Clone, Default)]
pub struct RuntimeSupervisorBuilder {
    #[cfg(feature = "gateway")]
    proxy: Option<ProxyConfig>,
    #[cfg(feature = "gateway")]
    gateway_output_dir: Option<PathBuf>,
    #[cfg(feature = "gateway")]
    gateway_enabled: bool,
    storage: Option<PathBuf>,
    #[cfg(feature = "gateway")]
    stream_markdown: bool,
    #[cfg(feature = "gateway")]
    sink: Option<Arc<dyn TrajectoryEventSink>>,
    overlay: OverlayHint,
    controller: Option<Arc<dyn ControlController>>,
    network: Option<NetworkDriverConfig>,
}

impl std::fmt::Debug for RuntimeSupervisorBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeSupervisorBuilder")
            .field("storage", &self.storage)
            .field("overlay", &self.overlay)
            .field("network", &self.network)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "gateway")]
struct SharedJournalObserver {
    journal: crate::trace::Journal,
    observer: Option<Arc<dyn TrajectoryEventSink>>,
}
#[cfg(feature = "gateway")]
impl pvisor_gateway::sink::CaptureEventObserver for SharedJournalObserver {
    fn observe(&self, event: &pvisor_core::event::Event) -> anyhow::Result<()> {
        match &self.observer {
            Some(observer) => observer.observe(event),
            None => Ok(()),
        }
    }
    fn journal(&self) -> Option<crate::trace::Journal> {
        Some(self.journal.clone())
    }
}

impl RuntimeSupervisorBuilder {
    #[cfg(feature = "gateway")]
    pub(crate) fn journal(mut self, journal: crate::trace::Journal) -> Self {
        self.sink = Some(Arc::new(SharedJournalObserver {
            journal,
            observer: self.sink.take(),
        }));
        self
    }

    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(feature = "gateway")]
    pub fn gateway(mut self, gateway: GatewayDriverConfig) -> Self {
        self.proxy = Some(gateway.proxy);
        self.gateway_output_dir = Some(gateway.output_dir);
        self.stream_markdown = gateway.stream_markdown;
        self.gateway_enabled = gateway.gateway_enabled;
        self
    }

    pub fn storage(mut self, storage: PathBuf) -> Self {
        self.storage = Some(storage);
        self
    }

    #[cfg(feature = "gateway")]
    pub fn trajectory_sink(mut self, sink: Arc<dyn TrajectoryEventSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    pub fn overlay(mut self, overlay: OverlayHint) -> Self {
        self.overlay = overlay;
        self
    }

    pub fn control_controller(mut self, controller: Arc<dyn ControlController>) -> Self {
        self.controller = Some(controller);
        self
    }

    pub fn network(mut self, network: NetworkDriverConfig) -> Self {
        self.network = Some(network);
        self
    }

    pub fn build(self) -> RuntimeSupervisor {
        RuntimeSupervisor {
            #[cfg(feature = "gateway")]
            proxy: self.proxy,
            #[cfg(feature = "gateway")]
            gateway_output_dir: self.gateway_output_dir,
            #[cfg(feature = "gateway")]
            gateway_enabled: self.gateway_enabled,
            storage: self.storage,
            #[cfg(feature = "gateway")]
            stream_markdown: self.stream_markdown,
            #[cfg(feature = "gateway")]
            sink: self.sink,
            overlay: self.overlay,
            controller: self
                .controller
                .unwrap_or_else(|| Arc::new(PolicyControlController)),
            network: self.network,
        }
    }
}

/// Capture / network / overlay prepare options for one Attempt.
#[derive(Clone)]
pub struct RuntimeSupervisor {
    #[cfg(feature = "gateway")]
    proxy: Option<ProxyConfig>,
    #[cfg(feature = "gateway")]
    gateway_output_dir: Option<PathBuf>,
    #[cfg(feature = "gateway")]
    gateway_enabled: bool,
    storage: Option<PathBuf>,
    #[cfg(feature = "gateway")]
    stream_markdown: bool,
    #[cfg(feature = "gateway")]
    sink: Option<Arc<dyn TrajectoryEventSink>>,
    overlay: OverlayHint,
    controller: Arc<dyn ControlController>,
    network: Option<NetworkDriverConfig>,
}

impl Default for RuntimeSupervisor {
    fn default() -> Self {
        RuntimeSupervisorBuilder::new().build()
    }
}

impl RuntimeSupervisor {
    pub(crate) fn overlay_hint(&self) -> &OverlayHint {
        &self.overlay
    }
    fn network_mode(&self) -> OverlayNetMode {
        self.network
            .as_ref()
            .map_or(OverlayNetMode::Auto, |network| network.mode)
    }

    fn effective_network_config(&self, spec: &RunSpec) -> pvisor_overlaynet::NetworkConfig {
        if matches!(spec.capabilities.network, NetworkCapability::Scoped { .. }) {
            return pvisor_core::NetworkConfig {
                capability: Some(spec.capabilities.network.clone()),
                ..Default::default()
            };
        }
        self.network
            .as_ref()
            .map(|network| network.network.clone())
            .or({
                #[cfg(feature = "gateway")]
                {
                    self.proxy.as_ref().map(|proxy| proxy.network.clone())
                }
                #[cfg(not(feature = "gateway"))]
                {
                    None
                }
            })
            .unwrap_or_else(|| network_config_from_capability(&spec.capabilities.network))
    }

    pub(crate) fn vm_network_is_enforcing(&self) -> bool {
        self.network_mode() == OverlayNetMode::Auto && vm_network_supported()
    }

    pub(crate) fn vm_network_is_requested(&self) -> bool {
        self.network_mode() == OverlayNetMode::Auto
    }

    pub(crate) fn proxy_network_is_configured(&self) -> bool {
        #[cfg(feature = "gateway")]
        if self.proxy.is_some() {
            return true;
        }
        self.network_mode() == OverlayNetMode::Proxy
    }

    pub(crate) fn apply_network_capability(&self, spec: &mut RunSpec) {
        let network = self.effective_network_config(spec);
        spec.capabilities.network = pvisor_overlaynet::policy::network_capability(&network);
    }

    fn vm_network_options(
        &self,
        mut network: pvisor_overlaynet::NetworkConfig,
        supervisor_limits: &[pvisor_core::NetworkBandwidthLimit],
        attempt_id: &AttemptId,
    ) -> super::attempt::VmNetworkPrepareOpts {
        network.limits.extend_from_slice(supervisor_limits);
        super::attempt::VmNetworkPrepareOpts {
            network,
            controller: Arc::clone(&self.controller),
            attempt_id: attempt_id.to_string(),
        }
    }

    pub fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities::default()
    }

    /// Start configured pVisor drivers and merge their implant into `spec`.
    pub fn prepare(
        &self,
        spec: &mut RunSpec,
        limits: &[pvisor_core::NetworkBandwidthLimit],
        vm_executor: bool,
        attempt_id: &AttemptId,
    ) -> anyhow::Result<Option<AttemptSession>> {
        let mut session = self.prepare_drivers(spec, limits, vm_executor, attempt_id)?;
        if self.network_mode() == OverlayNetMode::Proxy {
            #[cfg(feature = "gateway")]
            if self.proxy.is_some() {
                return Ok(session);
            }
            let mut network = self.network.clone().unwrap_or_default();
            network.network = self.effective_network_config(spec);
            network.network.limits.extend_from_slice(limits);
            if session.is_none() {
                let storage = self.storage.clone().unwrap_or_else(|| {
                    super::registry::default_run_home().join(spec.run_id.as_str())
                });
                std::fs::create_dir_all(&storage)?;
                session = Some(prepare_storage_attempt(spec, &storage, None)?);
            }
            let session_ref = session.as_mut().unwrap();
            if let Err(error) =
                session_ref.start_proxy(spec, &network, Arc::clone(&self.controller), attempt_id)
            {
                if let Some(session) = session.take() {
                    // Startup failed before execution; release the owned drivers and durable lease.
                    let snapshot = crate::AgentCtlSnapshot {
                        run_id: spec.run_id.to_string(),
                        attempt_id: attempt_id.to_string(),
                        directive: pvisor_core::AgentDirective::Continue,
                        clients: Vec::new(),
                    };
                    if let Err(cleanup) = session.abort_startup(
                        attempt_id,
                        snapshot,
                        spec.metadata
                            .get("pvisor.safe")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                        format!("OverlayNet proxy startup failed: {error:#}"),
                    ) {
                        tracing::warn!(%cleanup, "persist failed proxy startup");
                    }
                }
                return Err(error);
            }
        }
        Ok(session)
    }

    fn prepare_drivers(
        &self,
        spec: &mut RunSpec,
        supervisor_limits: &[pvisor_core::NetworkBandwidthLimit],
        vm_executor: bool,
        attempt_id: &AttemptId,
    ) -> anyhow::Result<Option<AttemptSession>> {
        let mut overlay = OverlayHint {
            access_policy: spec.policies.filesystem(&self.overlay.access_policy),
            ..self.overlay.clone()
        };
        overlay
            .access_policy
            .bind_session(spec.run_id.as_str(), attempt_id.as_str(), "workspace");
        let network_mode = self.network_mode();
        let network = self.effective_network_config(spec);
        let vm_network = vm_executor && network_mode == OverlayNetMode::Auto;
        if vm_executor && network_mode == OverlayNetMode::Proxy {
            anyhow::bail!(
                "overlaynet mode `proxy` is only valid for host/container execution; use `auto` for VM smoltcp networking"
            );
        }
        #[cfg(feature = "gateway")]
        if vm_executor && network_mode == OverlayNetMode::Off && self.proxy.is_some() {
            anyhow::bail!(
                "overlaynet mode `off` makes the VM offline and cannot be combined with Gateway/proxy configuration"
            );
        }
        #[cfg(feature = "gateway")]
        if let Some(proxy) = &self.proxy {
            let mut proxy = proxy.clone();
            // NetworkDriverConfig is the one Attempt policy source. ProxyConfig
            // retains its field for standalone Gateway use only.
            if vm_network {
                proxy.network = self
                    .vm_network_options(network, supervisor_limits, attempt_id)
                    .network;
            } else {
                proxy.network = network;
                proxy.network.limits.extend_from_slice(supervisor_limits);
            }
            let storage = self
                .storage
                .clone()
                .unwrap_or_else(|| PathBuf::from(".pvisor/capture"));
            let capture_storage = self
                .gateway_output_dir
                .clone()
                .unwrap_or_else(|| storage.clone());
            let session = prepare_attempt(
                spec,
                AttemptPrepareOpts {
                    config: &proxy,
                    storage: &storage,
                    capture_storage: &capture_storage,
                    #[cfg(feature = "gateway")]
                    sink: self.sink.clone(),
                    #[cfg(feature = "gateway")]
                    stream_markdown: self.stream_markdown,
                    overlay_override: overlay.clone(),
                    controller: Arc::clone(&self.controller),
                    #[cfg(feature = "gateway")]
                    gateway_enabled: self.gateway_enabled,
                    vm_network,
                    attempt_id: attempt_id.as_str(),
                },
            )?;
            return Ok(Some(session));
        }

        if !self.overlay.lower_dirs.is_empty()
            || self.overlay.stage_dir.is_some()
            || self.overlay.upper_dir.is_some()
            || self.overlay.work_dir.is_some()
            || self.overlay.merged_dir.is_some()
        {
            let storage = self
                .storage
                .clone()
                .unwrap_or_else(|| PathBuf::from(".pvisor/capture"));
            let session = prepare_overlay_attempt(
                spec,
                OverlayAttemptPrepareOpts {
                    storage: &storage,
                    overlay: overlay.clone(),
                    vm_network: vm_network
                        .then(|| self.vm_network_options(network, supervisor_limits, attempt_id)),
                },
            )?;
            return Ok(Some(session));
        }

        if let Some(storage) = &self.storage {
            let session = prepare_storage_attempt(
                spec,
                storage,
                vm_network.then(|| self.vm_network_options(network, supervisor_limits, attempt_id)),
            )?;
            return Ok(Some(session));
        }

        if vm_network {
            let storage = super::registry::default_run_home().join(spec.run_id.as_str());
            std::fs::create_dir_all(&storage)?;
            let session = prepare_storage_attempt(
                spec,
                &storage,
                Some(self.vm_network_options(network, supervisor_limits, attempt_id)),
            )?;
            return Ok(Some(session));
        }

        let _ = self.enrich_spec(spec);
        Ok(None)
    }

    /// Build the implant plan and merge it into a process RunSpec (env markers only).
    pub fn enrich_spec(&self, spec: &mut RunSpec) -> ImplantPlan {
        let plan = self.plan_for(spec);
        let pvisor_core::RunInvocation::Process(ref mut process) = spec.invocation;
        apply_implant(process, &plan);
        spec.metadata
            .insert("pvisor.runtime.implant".into(), plan.as_metadata_json());
        plan
    }

    pub fn plan_for(&self, spec: &RunSpec) -> ImplantPlan {
        let mut plan = ImplantPlan {
            env: ImplantPlan::marker_env(),
            cwd: self.overlay.merged_dir.clone(),
            overlay: self.overlay.clone(),
            notes: Vec::new(),
        };

        plan.env
            .insert("PVISOR_RUN_ID".into(), spec.run_id.as_str().to_string());
        plan.env
            .insert("PVISOR_AGENT".into(), spec.agent.name.clone());

        if self.proxy_network_is_configured() {
            plan.notes
                .push("network: in-process OverlayNet proxy configured".into());
            plan.env
                .insert("PVISOR_OVERLAYNET_DRIVER".into(), "explicit-proxy".into());
            plan.env
                .insert("PVISOR_OVERLAYNET_STRENGTH".into(), "cooperative".into());
        }
        #[cfg(feature = "gateway")]
        if let Some(path) = &self.gateway_output_dir {
            plan.env
                .insert("PVISOR_CAPTURE_STORAGE".into(), path.display().to_string());
            plan.notes.push("capture: storage path exported".into());
        }

        match &spec.capabilities.network {
            NetworkCapability::Scoped { .. } => {
                plan.env
                    .insert("PVISOR_NETWORK_POLICY".into(), "scoped".into());
                plan.notes
                    .push("network: session > workspace > user policy".into());
            }

            NetworkCapability::Ambient => {
                plan.env
                    .insert("PVISOR_NETWORK_POLICY".into(), "ambient".into());
                plan.notes.push("network: ambient".into());
            }
            NetworkCapability::Deny => {
                plan.env
                    .insert("PVISOR_NETWORK_POLICY".into(), "deny".into());
                plan.notes.push(
                    "network: deny requested; only intercepted proxy traffic is controlled".into(),
                );
            }
            NetworkCapability::AllowList { hosts, rules } => {
                plan.env
                    .insert("PVISOR_NETWORK_POLICY".into(), "allowlist".into());
                plan.env
                    .insert("PVISOR_NETWORK_ALLOWLIST".into(), hosts.join(","));
                if let Ok(serialized) = serde_json::to_string(rules) {
                    plan.env.insert("PVISOR_NETWORK_RULES".into(), serialized);
                }
                plan.notes.push(format!(
                    "network: allowlist ({} legacy hosts, {} structured rules)",
                    hosts.len(),
                    rules.len()
                ));
            }
            NetworkCapability::Policy {
                default_action,
                allow,
                deny,
                limits,
            } => {
                plan.env.insert(
                    "PVISOR_NETWORK_POLICY".into(),
                    match default_action {
                        pvisor_core::NetworkDefaultAction::Allow => "default-allow",
                        pvisor_core::NetworkDefaultAction::Deny => "default-deny",
                    }
                    .into(),
                );
                for (key, value) in [
                    ("PVISOR_NETWORK_RULES", allow),
                    ("PVISOR_NETWORK_DENY", deny),
                ] {
                    if let Ok(serialized) = serde_json::to_string(value) {
                        plan.env.insert(key.into(), serialized);
                    }
                }
                if let Ok(serialized) = serde_json::to_string(limits) {
                    plan.env.insert("PVISOR_NETWORK_LIMITS".into(), serialized);
                }
                plan.notes.push(format!(
                    "network: policy ({} allow, {} deny, {} bandwidth limits)",
                    allow.len(),
                    deny.len(),
                    limits.len()
                ));
            }
        }

        if self.overlay.merged_dir.is_some() {
            plan.notes
                .push("filesystem: merged overlay root selected as cwd".into());
        } else {
            plan.notes
                .push("filesystem: host view (no overlay merged_dir)".into());
        }

        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "gateway")]
    use pvisor_gateway::config::ProxyConfig;

    #[cfg(feature = "gateway")]
    fn test_proxy() -> ProxyConfig {
        ProxyConfig::from_toml_str(
            r#"
listen = "127.0.0.1:19081"
admin_listen = "127.0.0.1:19876"
agent_id = "test"
models = []
"#,
        )
        .unwrap()
    }

    #[test]
    fn capabilities_report_vm_smoltcp_only_on_supported_hosts() {
        let capabilities = RuntimeCapabilities::default();
        assert_eq!(capabilities.vm_network, vm_network_supported());
        assert_eq!(
            capabilities.providers.contains(&"overlaynet-vm-smoltcp"),
            vm_network_supported()
        );
    }

    #[test]
    #[cfg(feature = "gateway")]
    fn explicit_network_config_is_the_attempt_policy_source() {
        let mut proxy = test_proxy();
        proxy.network.mode = pvisor_overlaynet::NetworkMode::Public;
        let supervisor = RuntimeSupervisorBuilder::new()
            .gateway(GatewayDriverConfig::new(proxy))
            .network(NetworkDriverConfig::new(
                OverlayNetMode::Proxy,
                pvisor_overlaynet::NetworkConfig {
                    mode: pvisor_overlaynet::NetworkMode::NoNetwork,
                    ..Default::default()
                },
            ))
            .build();
        let mut spec = RunSpec::process("configured-policy", "test", "true");

        supervisor.apply_network_capability(&mut spec);

        assert_eq!(spec.capabilities.network, NetworkCapability::Deny);
    }

    #[test]
    fn absent_network_config_preserves_the_run_spec_policy() {
        let supervisor = RuntimeSupervisorBuilder::new().build();
        let mut spec = RunSpec::process("spec-policy", "test", "true");
        spec.capabilities.network = NetworkCapability::Deny;

        supervisor.apply_network_capability(&mut spec);

        assert_eq!(spec.capabilities.network, NetworkCapability::Deny);
    }

    #[test]
    fn network_capability_roundtrips_into_driver_config() {
        let cases = [
            NetworkCapability::Ambient,
            NetworkCapability::Deny,
            NetworkCapability::AllowList {
                hosts: vec!["api.example.com".into()],
                rules: Vec::new(),
            },
            NetworkCapability::Policy {
                default_action: pvisor_core::NetworkDefaultAction::Deny,
                allow: vec![pvisor_core::NetworkAccessRule {
                    host: "api.example.com".into(),
                    ports: vec![443],
                    transports: vec![pvisor_core::NetworkTransport::TcpTunnel],
                    allow_private_ips: false,
                }],
                deny: vec![pvisor_core::NetworkAccessRule {
                    host: "metadata.internal".into(),
                    ports: Vec::new(),
                    transports: Vec::new(),
                    allow_private_ips: false,
                }],
                limits: Vec::new(),
            },
        ];

        for capability in cases {
            let config = network_config_from_capability(&capability);
            assert_eq!(
                pvisor_overlaynet::policy::network_capability(&config),
                capability
            );
        }
    }

    #[test]
    #[cfg(feature = "gateway")]
    fn offline_vm_rejects_gateway_configuration() {
        let supervisor = RuntimeSupervisorBuilder::new()
            .network(NetworkDriverConfig::new(
                OverlayNetMode::Off,
                Default::default(),
            ))
            .gateway(GatewayDriverConfig::new(test_proxy()))
            .build();
        let mut spec = RunSpec::process("offline-vm", "test", "true");
        let error =
            match supervisor.prepare(&mut spec, &[], true, &AttemptId::new("attempt-offline")) {
                Ok(_) => panic!("offline VM accepted Gateway configuration"),
                Err(error) => error,
            };
        assert!(
            error
                .to_string()
                .contains("mode `off` makes the VM offline")
        );
    }
}
