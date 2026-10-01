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
    /// Absence falls through to the next scope; an explicit default terminates lookup.
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
    fn scoped_policies_preserve_priority_constraints_and_serialization() {
        let mut policies = SessionPolicies::default();
        policies.user.network = Some(NetworkPolicyLayer {
            default_action: Some(NetworkDefaultAction::Deny),
            ..Default::default()
        });
        policies.workspace.network = Some(NetworkPolicyLayer {
            allow: vec![rule("workspace.example", vec![443])],
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
            storyline_id: None,
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
            FileAccessDecision::Allow
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
            ["session.fs.allow.0"]
        );
        assert_eq!(
            files
                .prefixed("home/me")
                .unwrap()
                .authorize(std::path::Path::new("home/me/secret/session")),
            FileAccessDecision::Allow
        );
        let mut inherited = restored.clone();
        inherited.session = Default::default();
        assert_eq!(
            inherited
                .filesystem(&Default::default())
                .authorize(std::path::Path::new("secret/workspace")),
            FileAccessDecision::Allow
        );
    }
}
