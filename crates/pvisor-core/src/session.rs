//! Immutable policy inputs for one execution Session.
use crate::{
    FileAccessPolicy, NetworkAccessRule, NetworkBandwidthLimit, NetworkCapability,
    NetworkDefaultAction,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyScope {
    User,
    Workspace,
    Session,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkPolicyLayer {
    /// Unmatched targets are denied unless this layer explicitly defaults to allow.
    pub default_action: Option<NetworkDefaultAction>,
    pub allow: Vec<NetworkAccessRule>,
    pub deny: Vec<NetworkAccessRule>,
    pub limits: Vec<NetworkBandwidthLimit>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyLayer {
    pub network: Option<NetworkPolicyLayer>,
    pub filesystem: Option<FileAccessPolicy>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionPolicies {
    pub session: PolicyLayer,
    pub workspace: PolicyLayer,
    pub user: PolicyLayer,
}
impl SessionPolicies {
    pub fn scopes(&self) -> [(PolicyScope, &PolicyLayer); 3] {
        [
            (PolicyScope::Session, &self.session),
            (PolicyScope::Workspace, &self.workspace),
            (PolicyScope::User, &self.user),
        ]
    }
    pub fn network(&self, fallback: NetworkCapability) -> NetworkCapability {
        let mut layers: Vec<_> = self
            .scopes()
            .into_iter()
            .filter_map(|(scope, layer)| layer.network.clone().map(|policy| (scope, policy)))
            .collect();
        let mut base = fallback;
        while let NetworkCapability::Scoped {
            layers: inherited,
            fallback,
        } = base
        {
            for (scope, policy) in inherited {
                if !layers.iter().any(|(existing, _)| *existing == scope) {
                    layers.push((scope, policy));
                }
            }
            base = *fallback;
        }
        layers.sort_by_key(|(scope, _)| std::cmp::Reverse(*scope));
        if layers.is_empty() {
            base
        } else {
            NetworkCapability::Scoped {
                layers,
                fallback: Box::new(base),
            }
        }
    }
    pub fn filesystem(&self, fallback: &FileAccessPolicy) -> FileAccessPolicy {
        FileAccessPolicy::layered(
            self.scopes().into_iter().filter_map(|(scope, layer)| {
                layer.filesystem.as_ref().map(|policy| (scope, policy))
            }),
            fallback,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        FileAccessDecision, NetworkAccessRequest, NetworkConfig, NetworkPolicy, NetworkTransport,
        PolicyControlController,
    };
    fn rule(host: &str, ports: Vec<u16>) -> NetworkAccessRule {
        NetworkAccessRule {
            host: host.into(),
            ports,
            transports: Vec::new(),
            allow_private_ips: false,
        }
    }
    #[test]
    fn scoped_policies_intersect_constraints_and_preserve_serialization() {
        let mut policies = SessionPolicies::default();
        policies.user.network = Some(NetworkPolicyLayer {
            allow: vec![rule("session.example", vec![443])],
            ..Default::default()
        });
        policies.workspace.network = Some(NetworkPolicyLayer {
            allow: vec![
                rule("workspace.example", vec![443]),
                rule("session.example", vec![443]),
            ],
            ..Default::default()
        });
        policies.session.network = Some(NetworkPolicyLayer {
            allow: vec![rule("session.example", vec![443])],
            deny: vec![rule("workspace.example", vec![])],
            ..Default::default()
        });
        policies.user.filesystem =
            Some(FileAccessPolicy::new(vec!["secret/**".into()], vec![]).unwrap());
        policies.workspace.filesystem = Some(
            FileAccessPolicy::new_with_allow(
                vec![],
                vec![],
                vec![],
                vec!["secret/workspace".into()],
            )
            .unwrap(),
        );
        policies.session.filesystem = Some(
            FileAccessPolicy::new_with_allow(
                vec!["secret/workspace".into()],
                vec![],
                vec![],
                vec!["secret/session".into()],
            )
            .unwrap(),
        );
        let restored: SessionPolicies =
            serde_json::from_value(serde_json::to_value(&policies).unwrap()).unwrap();
        assert_eq!(policies, restored);
        let compiled = restored.network(NetworkCapability::Ambient);
        assert_eq!(restored.network(compiled.clone()), compiled);

        let policy = NetworkPolicy::compile(&NetworkConfig {
            capability: Some(restored.network(NetworkCapability::Ambient)),
            ..Default::default()
        })
        .unwrap();
        let mut request = NetworkAccessRequest {
            run_id: None,
            attempt_id: None,
            host: "session.example".into(),
            port: Some(443),
            transport: NetworkTransport::TcpTunnel,
            resolved_ip: Some("8.8.8.8".parse().unwrap()),
        };
        assert!(policy.authorize(&PolicyControlController, &request).is_ok());
        request.port = Some(80);
        assert!(
            policy
                .authorize(&PolicyControlController, &request)
                .is_err()
        );
        request.port = Some(443);
        request.resolved_ip = Some("127.0.0.1".parse().unwrap());
        assert!(
            policy
                .authorize(&PolicyControlController, &request)
                .is_err()
        );
        request.resolved_ip = None;
        request.host = "workspace.example".into();
        assert!(
            policy
                .authorize(&PolicyControlController, &request)
                .is_err()
        );
        request.host = "other.example".into();
        assert!(
            policy
                .authorize(&PolicyControlController, &request)
                .is_err()
        );
        let files = restored.filesystem(&Default::default());
        assert_eq!(
            files.authorize(std::path::Path::new("secret/session")),
            FileAccessDecision::Deny
        );
        assert_eq!(
            files.authorize(std::path::Path::new("secret/workspace")),
            FileAccessDecision::Deny
        );
        assert_eq!(
            files.authorize(std::path::Path::new("secret/other")),
            FileAccessDecision::Deny
        );
        assert_eq!(
            files.matched_rule_ids(std::path::Path::new("secret/session")),
            ["user.fs.deny.0"]
        );
        assert_eq!(
            files
                .prefixed("home/me")
                .unwrap()
                .authorize(std::path::Path::new("home/me/secret/session")),
            FileAccessDecision::Deny
        );
        let mut inherited = restored.clone();
        inherited.session = Default::default();
        assert_eq!(
            inherited
                .filesystem(&Default::default())
                .authorize(std::path::Path::new("secret/workspace")),
            FileAccessDecision::Deny
        );
    }
    #[test]
    fn scoped_network_never_widens_other_layers_or_fallback() {
        let mut policies = SessionPolicies::default();
        policies.workspace.network = Some(NetworkPolicyLayer {
            allow: vec![rule("api.example", vec![443])],
            ..Default::default()
        });
        let compile = |policies: &SessionPolicies, fallback| {
            NetworkPolicy::compile(&NetworkConfig {
                capability: Some(policies.network(fallback)),
                ..Default::default()
            })
            .unwrap()
        };
        let mut request = NetworkAccessRequest {
            run_id: None,
            attempt_id: None,
            host: "evil.example".into(),
            port: Some(443),
            transport: NetworkTransport::TcpTunnel,
            resolved_ip: None,
        };
        assert!(
            compile(&policies, NetworkCapability::Ambient)
                .preflight(&request)
                .is_err()
        );
        request.host = "api.example".into();
        assert!(
            compile(&policies, NetworkCapability::Ambient)
                .preflight(&request)
                .is_ok()
        );
        assert!(
            compile(&policies, NetworkCapability::Deny)
                .preflight(&request)
                .is_err()
        );
        policies.user.network = Some(NetworkPolicyLayer {
            default_action: Some(NetworkDefaultAction::Allow),
            deny: vec![rule("api.example", vec![])],
            ..Default::default()
        });
        policies.session.network = policies.workspace.network.clone();
        let policy = compile(&policies, NetworkCapability::Ambient);
        assert_eq!(
            policy.preflight(&request),
            Err(crate::network::DenyReason::ExplicitDeny)
        );
        assert!(policy.one_time_grant(&request).is_err());
        policies.user.network.as_mut().unwrap().deny.clear();
        policies.user.network.as_mut().unwrap().allow = vec![rule("api.example", vec![80])];
        assert!(
            compile(&policies, NetworkCapability::Ambient)
                .preflight(&request)
                .is_err()
        );
        request.host = "approved.example".into();
        let granted = compile(&policies, NetworkCapability::Ambient)
            .one_time_grant(&request)
            .unwrap();
        assert!(granted.preflight(&request).is_ok());
        request.host = "evil.example".into();
        assert!(granted.preflight(&request).is_err());
        request.host = "approved.example".into();
        request.resolved_ip = Some("127.0.0.1".parse().unwrap());
        assert!(granted.preflight(&request).is_err());
    }

    #[test]
    fn file_restrictions_and_bandwidth_limits_stack_across_scopes() {
        let mut policies = SessionPolicies::default();
        policies.user.filesystem = Some(
            FileAccessPolicy::new_with_ask(
                vec!["secret".into()],
                vec!["approval".into()],
                vec!["warning".into()],
            )
            .unwrap(),
        );
        policies.workspace.filesystem = Some(
            FileAccessPolicy::new_with_allow(
                vec![],
                vec![],
                vec![],
                vec!["secret".into(), "approval".into(), "warning".into()],
            )
            .unwrap(),
        );
        policies.session.filesystem = policies.workspace.filesystem.clone();
        let files = policies.filesystem(&Default::default());
        for (path, decision, id) in [
            ("secret", FileAccessDecision::Deny, "user.fs.deny.0"),
            ("approval", FileAccessDecision::Ask, "user.fs.ask.0"),
            ("warning", FileAccessDecision::Warn, "user.fs.warn.0"),
        ] {
            assert_eq!(files.authorize(std::path::Path::new(path)), decision);
            assert_eq!(files.matched_rule_ids(std::path::Path::new(path)), [id]);
            assert_eq!(
                serde_json::from_value::<FileAccessPolicy>(serde_json::to_value(&files).unwrap())
                    .unwrap()
                    .authorize(std::path::Path::new(path)),
                decision
            );
        }
        for (layer, rate) in [
            (&mut policies.session, 100),
            (&mut policies.workspace, 200),
            (&mut policies.user, 300),
        ] {
            layer.network = Some(NetworkPolicyLayer {
                default_action: Some(NetworkDefaultAction::Allow),
                limits: vec![NetworkBandwidthLimit {
                    host: None,
                    port: None,
                    bytes_per_second: rate,
                }],
                ..Default::default()
            });
        }
        let policy = NetworkPolicy::compile(&NetworkConfig {
            capability: Some(policies.network(NetworkCapability::Ambient)),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            policy.matching_limits("api.example", Some(443), &[]).len(),
            3
        );
    }
}
