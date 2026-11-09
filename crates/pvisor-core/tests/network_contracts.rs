use pvisor_core::{
    NetworkAccessRequest, NetworkAccessRule, NetworkBandwidthLimit, NetworkCapability,
    NetworkConfig, NetworkDefaultAction, NetworkPolicy, NetworkTransport, network::DenyReason,
};

fn request(host: &str) -> NetworkAccessRequest {
    NetworkAccessRequest {
        run_id: None,
        execution_id: None,
        host: host.into(),
        port: Some(443),
        transport: NetworkTransport::TcpTunnel,
        resolved_ip: None,
    }
}

fn rule(host: &str) -> NetworkAccessRule {
    NetworkAccessRule {
        host: host.into(),
        ports: vec![],
        transports: vec![],
        allow_private_ips: false,
    }
}

#[test]
fn ipv4_and_mapped_ipv6_are_equivalent_for_literal_and_resolved_denies() {
    for host in [
        "10.1.2.3",
        "::ffff:10.1.2.3",
        "10.0.0.0/8",
        "::ffff:10.0.0.0/104",
    ] {
        let policy = NetworkPolicy::compile(&NetworkConfig {
            deny_rules: vec![rule(host)],
            ..Default::default()
        })
        .unwrap();
        for address in ["10.1.2.3", "::ffff:10.1.2.3"] {
            assert_eq!(
                policy.preflight(&request(address)),
                Err(DenyReason::ExplicitDeny),
                "{host}: {address}"
            );
            let mut resolved = request("service.example");
            resolved.resolved_ip = Some(address.parse().unwrap());
            assert_eq!(
                policy.preflight(&resolved),
                Err(DenyReason::ExplicitDeny),
                "{host}: resolved {address}"
            );
        }
        assert!(policy.preflight(&request("11.1.2.3")).is_ok());
    }
}

#[test]
fn mapped_allow_rules_use_the_same_equivalence_for_address_safety() {
    for host in [
        "10.1.2.3",
        "::ffff:10.1.2.3",
        "10.0.0.0/8",
        "::ffff:10.0.0.0/104",
    ] {
        let policy = NetworkPolicy::compile(&NetworkConfig {
            capability: Some(NetworkCapability::AllowList {
                hosts: vec![host.into()],
                rules: vec![],
            }),
            ..Default::default()
        })
        .unwrap();
        for address in ["10.1.2.3", "::ffff:10.1.2.3"] {
            let mut resolved = request("service.example");
            resolved.resolved_ip = Some(address.parse().unwrap());
            assert!(policy.preflight(&resolved).is_ok(), "{host}: {address}");
        }
        let mut outside = request("service.example");
        outside.resolved_ip = Some("11.1.2.3".parse().unwrap());
        assert!(policy.preflight(&outside).is_err());
    }
}

#[test]
fn canonical_leaf_approval_grants_only_the_requested_destination() {
    for capability in [
        NetworkCapability::AllowList {
            hosts: vec!["existing.example".into()],
            rules: vec![],
        },
        NetworkCapability::Policy {
            default_action: NetworkDefaultAction::Deny,
            allow: vec![rule("existing.example")],
            deny: vec![rule("blocked.example")],
            limits: vec![NetworkBandwidthLimit {
                host: None,
                port: Some(443),
                bytes_per_second: 256,
            }],
        },
    ] {
        let policy = NetworkPolicy::compile(&NetworkConfig {
            capability: Some(capability),
            ..Default::default()
        })
        .unwrap();
        let original = request("new.example");
        assert!(policy.preflight(&original).is_err());
        let approved = policy.one_time_grant(&original).unwrap();
        assert_eq!(
            approved.matching_limits("new.example", Some(443), &[]),
            policy.matching_limits("new.example", Some(443), &[])
        );
        assert!(approved.preflight(&original).is_ok());
        assert!(policy.preflight(&original).is_err());
        for host in ["other.example", "existing.example", "blocked.example"] {
            assert!(approved.preflight(&request(host)).is_err());
        }
        let mut changed = original.clone();
        changed.port = Some(80);
        assert!(approved.preflight(&changed).is_err());
        changed = original.clone();
        changed.transport = NetworkTransport::Http;
        assert!(approved.preflight(&changed).is_err());
        changed = original;
        changed.resolved_ip = Some("127.0.0.1".parse().unwrap());
        assert_eq!(
            approved.preflight(&changed),
            Err(DenyReason::ResolvedAddressNotAllowed)
        );
    }
}

#[test]
fn native_ipv6_cidrs_remain_distinct_from_ipv4_and_unrelated_ipv6() {
    let policy = NetworkPolicy::compile(&NetworkConfig {
        deny_rules: vec![rule("2001:4860::/32")],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        policy.preflight(&request("2001:4860:4860::8888")),
        Err(DenyReason::ExplicitDeny)
    );
    for host in ["8.8.8.8", "::ffff:8.8.8.8", "2606:4700:4700::1111"] {
        assert!(policy.preflight(&request(host)).is_ok(), "{host}");
    }
}

#[test]
fn canonical_leaf_approval_cannot_override_an_explicit_deny() {
    let policy = NetworkPolicy::compile(&NetworkConfig {
        capability: Some(NetworkCapability::Policy {
            default_action: NetworkDefaultAction::Deny,
            allow: vec![],
            deny: vec![rule("blocked.example")],
            limits: vec![],
        }),
        ..Default::default()
    })
    .unwrap();
    assert!(policy.one_time_grant(&request("blocked.example")).is_err());
}

#[test]
fn scoped_approval_is_request_scoped_without_poisoning_the_source_policy() {
    use pvisor_core::{NetworkPolicyLayer, PolicyScope};

    let limit = NetworkBandwidthLimit {
        host: None,
        port: Some(443),
        bytes_per_second: 256,
    };
    for fallback in [
        NetworkCapability::Ambient,
        NetworkCapability::AllowList {
            hosts: vec!["existing.example".into(), "fallback.example".into()],
            rules: vec![],
        },
        NetworkCapability::Policy {
            default_action: NetworkDefaultAction::Allow,
            allow: vec![],
            deny: vec![rule("fallback-denied.example")],
            limits: vec![limit.clone()],
        },
    ] {
        for default_action in [NetworkDefaultAction::Allow, NetworkDefaultAction::Deny] {
            let config = NetworkConfig {
                capability: Some(NetworkCapability::Scoped {
                    layers: vec![(
                        PolicyScope::Session,
                        NetworkPolicyLayer {
                            default_action: Some(default_action),
                            allow: vec![rule("existing.example")],
                            deny: vec![rule("blocked.example")],
                            limits: vec![limit.clone()],
                        },
                    )],
                    fallback: Box::new(fallback.clone()),
                }),
                ..Default::default()
            };
            let policy = NetworkPolicy::compile(&config).unwrap();
            let input = request("new.example");
            let original_decision = policy.preflight(&input);
            assert!(policy.preflight(&request("existing.example")).is_ok());
            let approved = policy.one_time_grant(&input).unwrap();
            assert!(approved.preflight(&input).is_ok());
            for host in [
                "existing.example",
                "fallback.example",
                "other.example",
                "blocked.example",
                "fallback-denied.example",
            ] {
                assert!(
                    approved.preflight(&request(host)).is_err(),
                    "request-scoped grant authorized {host}: {config:?}"
                );
            }
            assert_eq!(
                approved.preflight(&request("blocked.example")),
                Err(DenyReason::ExplicitDeny)
            );
            if matches!(fallback, NetworkCapability::Policy { .. }) {
                assert_eq!(
                    approved.preflight(&request("fallback-denied.example")),
                    Err(DenyReason::ExplicitDeny)
                );
            }
            let mut changed = input.clone();
            changed.port = Some(80);
            assert!(approved.preflight(&changed).is_err());
            changed = input.clone();
            changed.transport = NetworkTransport::Http;
            assert!(approved.preflight(&changed).is_err());
            changed = input.clone();
            changed.resolved_ip = Some("127.0.0.1".parse().unwrap());
            assert_eq!(
                approved.preflight(&changed),
                Err(DenyReason::ResolvedAddressNotAllowed)
            );
            assert_eq!(
                approved.matching_limits("new.example", Some(443), &[]),
                policy.matching_limits("new.example", Some(443), &[])
            );
            // A denied approval must not poison the existing immutable policy.
            assert!(policy.one_time_grant(&request("blocked.example")).is_err());
            assert!(policy.preflight(&request("existing.example")).is_ok());
            assert_eq!(policy.preflight(&input), original_decision);
        }
    }
}

#[test]
fn scoped_approval_retains_deny_precedence_in_every_scope_and_fallback() {
    use pvisor_core::{NetworkPolicyLayer, PolicyScope};

    for denied_at in [0, 1, 2, 3] {
        let input = request("new.example");
        let layers = [PolicyScope::Session, PolicyScope::Workspace]
            .into_iter()
            .enumerate()
            .map(|(index, scope)| {
                (
                    scope,
                    NetworkPolicyLayer {
                        default_action: Some(NetworkDefaultAction::Allow),
                        allow: vec![rule("new.example")],
                        deny: if index == denied_at {
                            vec![rule("new.example")]
                        } else {
                            vec![]
                        },
                        ..Default::default()
                    },
                )
            })
            .collect();
        let fallback = if denied_at == 3 {
            NetworkCapability::Deny
        } else {
            NetworkCapability::Policy {
                default_action: NetworkDefaultAction::Allow,
                allow: vec![rule("new.example")],
                deny: if denied_at == 2 {
                    vec![rule("new.example")]
                } else {
                    vec![]
                },
                limits: vec![],
            }
        };
        let policy = NetworkPolicy::compile(&NetworkConfig {
            capability: Some(NetworkCapability::Scoped {
                layers,
                fallback: Box::new(fallback),
            }),
            ..Default::default()
        })
        .unwrap();
        let original = policy.preflight(&input);
        assert!(original.is_err(), "denied at {denied_at}");
        for _ in 0..2 {
            assert!(
                policy.one_time_grant(&input).is_err(),
                "denied at {denied_at}"
            );
            assert_eq!(policy.preflight(&input), original);
        }
    }
}

#[test]
fn approval_request_hosts_cannot_be_interpreted_as_multi_destination_rules() {
    // Fixed-seed, bounded cases exercise both policy grammars that widen a host.
    let mut seed = 0xc1_2026_u32;
    for _ in 0..16 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let octet = 1 + seed % 222;
        let suffix = format!("service-{seed:x}.example");
        for capability in [
            None,
            Some(NetworkCapability::AllowList {
                hosts: vec![],
                rules: vec![],
            }),
            Some(NetworkCapability::Policy {
                default_action: NetworkDefaultAction::Deny,
                allow: vec![],
                deny: vec![],
                limits: vec![],
            }),
            Some(NetworkCapability::Scoped {
                layers: vec![(pvisor_core::PolicyScope::Session, Default::default())],
                fallback: Box::new(NetworkCapability::Ambient),
            }),
        ] {
            let config = NetworkConfig {
                mode: pvisor_core::NetworkMode::Allowlist,
                capability,
                ..Default::default()
            };
            let restored: NetworkConfig =
                serde_json::from_slice(&serde_json::to_vec(&config).unwrap()).unwrap();
            let policy = NetworkPolicy::compile(&restored).unwrap();
            for host in [
                format!("*.{suffix}"),
                format!("  *.{suffix}.  "),
                format!("{octet}.0.0.0/8"),
                format!("2001:4860:{:x}::/48", seed as u16),
            ] {
                let input: NetworkAccessRequest =
                    serde_json::from_slice(&serde_json::to_vec(&request(&host)).unwrap()).unwrap();
                let grant = policy.one_time_grant(&input);
                assert!(
                    grant.is_err(),
                    "approval must reject multi-destination request host {host:?}"
                );
            }
            for host in [
                suffix.clone(),
                format!("{octet}.1.2.3"),
                "2001:4860::8888".into(),
            ] {
                let input = request(&host);
                let granted = policy.one_time_grant(&input).unwrap();
                assert!(granted.preflight(&input).is_ok(), "{host}");
                assert!(granted.preflight(&request("unrelated.example")).is_err());
                assert!(policy.preflight(&input).is_err());
            }
        }
    }
}
