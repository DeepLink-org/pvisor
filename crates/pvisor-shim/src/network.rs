//! VM egress attachment: compiles the annotation policy and owns the
//! pvisor-overlaynet smoltcp backend for one VM task.
//!
//! The backend terminates every guest TCP flow on the host and re-originates
//! it only when the compiled policy authorizes the destination; the guest's
//! virtio-net device is its only network path, so the enforcement cannot be
//! bypassed from inside the VM (see pvisor-overlaynet's `vm_smoltcp`
//! interception profile).

use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::path::{Path, PathBuf};

use crate::plan::{ContainerPlan, VmNetworkMode};

/// Host-side egress backend plus the guest-side descriptor handed to the VM
/// runner. Dropping the attachment stops policy enforcement; task deletion
/// owns this lifecycle.
pub struct VmNetworkAttachment {
    backend: pvisor_overlaynet::vm::VmNetwork,
    guest_stream: std::os::unix::net::UnixStream,
    egress: pvisor_overlaynet::EgressRuntime,
}

/// Where the egress sidecar container's Create publishes the OpenSandbox
/// network policy for this pod, keyed by the sandbox id. The VM runner
/// waits for it and compiles the overlaynet attachment from its contents.
pub fn egress_gate_path(sandbox_id: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    sandbox_id.hash(&mut hasher);
    let name = format!("pvisor-egress-{:016x}.json", hasher.finish());
    let directory = if std::path::Path::new("/run").is_dir() {
        std::path::PathBuf::from("/run")
    } else {
        std::env::temp_dir()
    };
    directory.join(name)
}

/// The OpenSandbox egress policy as the server hands it to the sidecar:
/// `{"defaultAction": "allow"|"deny", "egress": [{"action", "target"}]}`.
#[derive(serde::Deserialize)]
pub struct EgressRules {
    #[serde(rename = "defaultAction", default)]
    pub default_action: String,
    #[serde(default)]
    pub egress: Vec<EgressRule>,
}

#[derive(serde::Deserialize)]
pub struct EgressRule {
    pub action: String,
    pub target: String,
}

/// Split a sidecar rule target `host[:port]` the same way the
/// `io.pvisor.vm.network.allow` annotation path does: first colon separates
/// an IPv4 literal or hostname from a decimal port. Without the split the
/// port would corrupt the host pattern and every `host:port` target would
/// compile into a policy that can never match.
fn split_egress_target(target: &str) -> anyhow::Result<(String, u16)> {
    match target.split_once(':') {
        Some((host, port)) => {
            let port = port
                .parse::<u16>()
                .map_err(|_| anyhow::anyhow!("invalid port in egress rule target `{target}`"))?;
            if host.is_empty() {
                anyhow::bail!("empty host in egress rule target `{target}`");
            }
            Ok((host.to_string(), port))
        }
        None => Ok((target.to_string(), 0)),
    }
}

/// Compile the OpenSandbox sidecar policy into a network config. Deny by
/// default turns the egress rules into an allowlist (targets may be wildcard
/// domains; empty target ports mean every port); allow by default keeps the
/// network public with explicit deny rules, which take precedence over every
/// allow — so under allow-by-default an allow rule admits nothing beyond the
/// default and is dropped instead of becoming a public-mode allow entry
/// (which policy compilation rejects). Rule targets use the sidecar
/// contract's `host[:port]` form; a target without a port admits every port.
///
/// The OpenSandbox rule syntax cannot express the private-address opt-in the
/// annotation path exposes (`io.pvisor.vm.network.allow-private`), so it is
/// passed by the caller: with it, an explicitly allowed target may resolve
/// into private space (office-network relays, intranet registries); without
/// it hostname rules keep rejecting private resolutions, the documented
/// anti-DNS-rebinding default.
fn egress_rules_config(
    rules: &EgressRules,
    allow_private: bool,
) -> anyhow::Result<pvisor_core::NetworkConfig> {
    let default_deny = rules.default_action != "allow";
    let mut config = pvisor_core::NetworkConfig {
        mode: if default_deny {
            pvisor_core::NetworkMode::Allowlist
        } else {
            pvisor_core::NetworkMode::Public
        },
        ..Default::default()
    };
    for rule in &rules.egress {
        let (host, port) = split_egress_target(&rule.target)?;
        let access = pvisor_core::NetworkAccessRule {
            host,
            ports: if port != 0 { vec![port] } else { Vec::new() },
            transports: Vec::new(),
            allow_private_ips: allow_private,
        };
        match rule.action.as_str() {
            "allow" if default_deny => config.rules.push(access),
            "deny" => config.deny_rules.push(access),
            "allow" => {}
            other => anyhow::bail!("unsupported egress rule action {other:?}"),
        }
    }
    if default_deny && config.rules.is_empty() {
        anyhow::bail!("deny-by-default policy without allow rules denies everything");
    }
    Ok(config)
}

impl VmNetworkAttachment {
    /// Compile the task's network plan and start the backend. Returns `None`
    /// when the task did not opt into egress.
    pub fn start(plan: &ContainerPlan) -> anyhow::Result<Option<Self>> {
        let network = plan.vm_network_plan().map_err(anyhow::Error::msg)?;
        if network.mode == VmNetworkMode::Off {
            return Ok(None);
        }
        let mut config = pvisor_core::NetworkConfig {
            mode: match network.mode {
                VmNetworkMode::Off => pvisor_core::NetworkMode::NoNetwork,
                VmNetworkMode::Public => pvisor_core::NetworkMode::Public,
                VmNetworkMode::Allowlist => pvisor_core::NetworkMode::Allowlist,
            },
            ..Default::default()
        };
        for (host, port) in &network.allow {
            config.rules.push(pvisor_core::NetworkAccessRule {
                host: host.clone(),
                ports: if *port != 0 { vec![*port] } else { Vec::new() },
                transports: Vec::new(),
                allow_private_ips: network.allow_private,
            });
        }
        Self::from_config(config, &plan.id).map(Some)
    }

    /// Compile an attachment from the OpenSandbox sidecar policy. See
    /// [`egress_rules_config`] for the rule-to-config semantics;
    /// `allow_private` is the task's `io.pvisor.vm.network.allow-private`
    /// opt-in, mirroring the annotation path's semantics.
    pub fn from_egress_rules(
        rules: &EgressRules,
        run_id: &str,
        allow_private: bool,
    ) -> anyhow::Result<Self> {
        Self::from_config(egress_rules_config(rules, allow_private)?, run_id)
    }

    fn from_config(config: pvisor_core::NetworkConfig, run_id: &str) -> anyhow::Result<Self> {
        let policy = pvisor_core::NetworkPolicy::compile(&config)?;
        let controller = std::sync::Arc::new(pvisor_core::PolicyControlController);
        let egress = pvisor_overlaynet::EgressRuntime::with_bandwidth_registry(
            policy,
            controller,
            pvisor_overlaynet::BandwidthRegistry::default(),
        );
        let vm_config = pvisor_overlaynet::vm::VmNetworkConfig::new(
            egress.clone(),
            pvisor_overlaynet::EgressContext {
                run_id: Some(run_id.to_string()),
                attempt_id: Some("containerd-shim".to_string()),
            },
        );
        let (backend, guest_stream) = pvisor_overlaynet::vm::VmNetwork::start(vm_config)?;
        Ok(Self {
            backend,
            guest_stream,
            egress,
        })
    }

    /// The shared egress runtime; a policy replaced here takes effect for
    /// every TCP flow the VM backend authorizes afterwards.
    pub fn egress_runtime(&self) -> &pvisor_overlaynet::EgressRuntime {
        &self.egress
    }

    /// Guest-side stream for the VM runner. The attachment keeps its own
    /// end alive; dropping the returned clone only drops this handle.
    pub fn guest_stream(&self) -> std::io::Result<std::os::unix::net::UnixStream> {
        self.guest_stream.try_clone()
    }

    /// Duplicate the guest-side descriptor without `FD_CLOEXEC` so the VM
    /// runner inherits exactly one end. The caller closes the duplicate
    /// after spawning; the attachment keeps the original.
    pub fn guest_fd_for_child(&self) -> std::io::Result<RawFd> {
        // SAFETY: dup on a live descriptor; it either returns a new valid
        // descriptor or -1 with errno set.
        let fd = unsafe { libc::dup(self.guest_stream.as_raw_fd()) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(fd)
    }

    /// Stop the backend and return its interception counters. Closing the
    /// guest stream first lets a backend blocked on socket I/O observe EOF
    /// before its thread joins.
    pub fn shutdown(self) -> anyhow::Result<pvisor_overlaynet::InterceptionSnapshot> {
        let Self {
            backend,
            guest_stream,
            egress,
        } = self;
        drop(egress);
        drop(guest_stream);
        backend.shutdown()
    }
}

/// Wrap a runner-inherited descriptor for [`crate::vm::boot_vm`].
///
/// # Safety
/// The descriptor must come from [`VmNetworkAttachment::guest_fd_for_child`]
/// and may be wrapped exactly once.
pub unsafe fn stream_from_fd(fd: RawFd) -> std::os::unix::net::UnixStream {
    unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) }
}

/// How the gate watcher polls a republished egress policy.
const GATE_WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);

/// Spawn the dynamic-policy watcher for a gated VM attachment.
///
/// VM egress policy is compiled once from the sidecar's gate file because the
/// sidecar's runtime API can only update nftables in the pod netns, which the
/// VM data plane never traverses. Republishing the gate file is therefore the
/// dynamic-policy contract: this watcher recompiles the file whenever its
/// contents change and hot-swaps the running overlaynet policy. A rule set
/// that fails to compile keeps the previous policy (the VM never runs on a
/// half-built policy), logs the rejection, and is not retried until the file
/// changes again.
pub fn watch_gate(egress: pvisor_overlaynet::EgressRuntime, gate: &Path, allow_private: bool) {
    let egress = egress;
    let gate = gate.to_owned();
    std::thread::Builder::new()
        .name("pvisor-egress-gate".into())
        .spawn(move || {
            let mut last = std::fs::read_to_string(&gate).unwrap_or_default();
            loop {
                std::thread::sleep(GATE_WATCH_INTERVAL);
                let Ok(contents) = std::fs::read_to_string(&gate) else {
                    // A vanished gate means teardown; the runner process
                    // exits soon and takes this thread with it.
                    continue;
                };
                if contents == last {
                    continue;
                }
                last = contents;
                let applied = serde_json::from_str::<EgressRules>(&last)
                    .map_err(anyhow::Error::new)
                    .and_then(|rules| {
                        let config = egress_rules_config(&rules, allow_private)?;
                        let policy = pvisor_core::NetworkPolicy::compile(&config)?;
                        egress.replace_policy(policy);
                        Ok(())
                    });
                match applied {
                    Ok(()) => eprintln!("pvisor shim: egress policy hot-swapped from gate"),
                    Err(error) => eprintln!(
                        "pvisor shim: egress gate republish rejected, keeping previous policy: {error:#}"
                    ),
                }
            }
        })
        .expect("spawn egress gate watcher");
}

#[cfg(test)]
mod tests {
    use super::{EgressRule, EgressRules, egress_rules_config};
    use pvisor_core::network::DenyReason;
    use pvisor_core::{
        NetworkAccessRequest, NetworkPolicy, NetworkTransport, PolicyControlController,
    };

    fn rules(default_action: &str, egress: &[(&str, &str)]) -> EgressRules {
        EgressRules {
            default_action: default_action.to_owned(),
            egress: egress
                .iter()
                .map(|(action, target)| EgressRule {
                    action: action.to_string(),
                    target: target.to_string(),
                })
                .collect(),
        }
    }

    fn request(host: &str) -> NetworkAccessRequest {
        NetworkAccessRequest {
            run_id: None,
            attempt_id: None,
            host: host.into(),
            port: Some(443),
            transport: NetworkTransport::TcpTunnel,
            resolved_ip: None,
        }
    }

    #[test]
    fn gate_targets_parse_host_port_like_the_annotation_path() {
        // deny-default: `host:port` lands in the rule's port list instead of
        // corrupting the host pattern, for names and IPv4 literals alike.
        let config =
            egress_rules_config(&rules("deny", &[("allow", "example.com:443")]), false).unwrap();
        assert_eq!(config.rules[0].host, "example.com");
        assert_eq!(config.rules[0].ports, vec![443]);

        let config =
            egress_rules_config(&rules("deny", &[("allow", "10.140.84.58:18999")]), true).unwrap();
        assert_eq!(config.rules[0].host, "10.140.84.58");
        assert_eq!(config.rules[0].ports, vec![18999]);
        assert!(config.rules[0].allow_private_ips);

        // A bare host keeps the every-port meaning shared with annotations.
        let config =
            egress_rules_config(&rules("deny", &[("allow", "example.com")]), false).unwrap();
        assert_eq!(config.rules[0].host, "example.com");
        assert!(config.rules[0].ports.is_empty());

        // allow-default: deny rules keep their port, allow rules stay dropped.
        let config = egress_rules_config(
            &rules("allow", &[("deny", "blocked.example.com:8080")]),
            false,
        )
        .unwrap();
        assert_eq!(config.deny_rules[0].host, "blocked.example.com");
        assert_eq!(config.deny_rules[0].ports, vec![8080]);

        // Malformed ports fail compilation instead of building a policy that
        // can never match.
        assert!(
            egress_rules_config(&rules("deny", &[("allow", "example.com:https")]), false).is_err()
        );
        assert!(egress_rules_config(&rules("deny", &[("allow", ":443")]), false).is_err());
    }

    #[test]
    fn compiled_gate_policy_matches_port_scoped_targets() {
        // End to end through NetworkPolicy::compile: the port from the
        // sidecar target actually gates the decision.
        let config =
            egress_rules_config(&rules("deny", &[("allow", "example.com:443")]), false).unwrap();
        let policy = NetworkPolicy::compile(&config).unwrap();
        let mut allowed = request("example.com");
        allowed.port = Some(443);
        assert!(policy.authorize(&PolicyControlController, &allowed).is_ok());
        let mut refused = request("example.com");
        refused.port = Some(80);
        assert!(
            policy
                .authorize(&PolicyControlController, &refused)
                .is_err()
        );
    }

    #[test]
    fn default_allow_drops_allow_rules_and_keeps_public_deny_rules() {
        let config = egress_rules_config(
            &rules(
                "allow",
                &[
                    ("allow", "mirrors.example.com"),
                    ("deny", "blocked.example.com"),
                ],
            ),
            false,
        )
        .unwrap();
        assert_eq!(config.mode, pvisor_core::NetworkMode::Public);
        assert!(config.rules.is_empty());
        assert_eq!(config.deny_rules.len(), 1);
        // A public-mode config with allow entries would fail compilation,
        // which boots the VM without network; it must compile and authorize.
        let policy = NetworkPolicy::compile(&config).unwrap();
        assert_eq!(
            policy.authorize(&PolicyControlController, &request("blocked.example.com")),
            Err(DenyReason::ExplicitDeny)
        );
        assert!(
            policy
                .authorize(&PolicyControlController, &request("mirrors.example.com"))
                .is_ok()
        );
        assert!(
            policy
                .authorize(&PolicyControlController, &request("other.example.com"))
                .is_ok()
        );
    }

    #[test]
    fn default_deny_builds_an_allowlist() {
        let config = egress_rules_config(
            &rules(
                "deny",
                &[
                    ("allow", "mirrors.example.com"),
                    ("deny", "blocked.example.com"),
                ],
            ),
            false,
        )
        .unwrap();
        assert_eq!(config.mode, pvisor_core::NetworkMode::Allowlist);
        assert_eq!(config.rules.len(), 1);
        assert_eq!(config.deny_rules.len(), 1);
        let policy = NetworkPolicy::compile(&config).unwrap();
        assert!(
            policy
                .authorize(&PolicyControlController, &request("mirrors.example.com"))
                .is_ok()
        );
        assert_eq!(
            policy.authorize(&PolicyControlController, &request("other.example.com")),
            Err(DenyReason::NotInAllowlist)
        );
    }

    #[test]
    fn deny_by_default_without_allow_rules_is_rejected() {
        assert!(egress_rules_config(&rules("deny", &[]), false).is_err());
        assert!(
            egress_rules_config(&rules("deny", &[("deny", "blocked.example.com")]), false).is_err()
        );
    }

    #[test]
    fn unsupported_rule_actions_are_rejected() {
        assert!(egress_rules_config(&rules("allow", &[("block", "example.com")]), false).is_err());
        assert!(
            egress_rules_config(
                &rules(
                    "deny",
                    &[("allow", "a.example.com"), ("block", "b.example.com")]
                ),
                false
            )
            .is_err()
        );
    }

    #[test]
    fn allow_private_lets_allowed_targets_resolve_into_private_space() {
        // Office-network relays and intranet registries resolve into private
        // space; without the opt-in an explicitly allowed hostname still
        // rejects private resolutions (anti-DNS-rebinding default).
        let rules = &rules("deny", &[("allow", "relay.office.example")]);
        let mut relay_request = request("relay.office.example");
        relay_request.resolved_ip = Some("10.20.30.40".parse().unwrap());

        let strict = NetworkPolicy::compile(&egress_rules_config(rules, false).unwrap()).unwrap();
        assert_eq!(
            strict.authorize(&PolicyControlController, &relay_request),
            Err(DenyReason::ResolvedAddressNotAllowed)
        );

        let opted_in = NetworkPolicy::compile(&egress_rules_config(rules, true).unwrap()).unwrap();
        assert!(
            opted_in
                .authorize(&PolicyControlController, &relay_request)
                .is_ok()
        );

        // The opt-in never widens the allowlist itself: an unlisted host is
        // denied regardless (without a resolved address the failure reason is
        // the allowlist miss itself).
        let unlisted = request("other.office.example");
        assert_eq!(
            opted_in.authorize(&PolicyControlController, &unlisted),
            Err(DenyReason::NotInAllowlist)
        );
    }
}
