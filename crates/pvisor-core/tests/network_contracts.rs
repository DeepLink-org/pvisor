use pvisor_core::{
    NetworkAccessRequest, NetworkAccessRule, NetworkBandwidthLimit, NetworkCapability,
    NetworkConfig, NetworkDefaultAction, NetworkPolicy, NetworkTransport, network::DenyReason,
};

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
