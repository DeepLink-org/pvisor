use pvisor_core::network::{NetworkConfig, NetworkMode, NetworkPolicy};
use pvisor_core::{NetworkAccessRequest, NetworkAccessRule, NetworkTransport};

#[test]
fn ipv4_and_mapped_ipv6_share_exact_and_cidr_authorization() {
    for rule in [
        "10.1.2.3",
        "10.0.0.0/8",
        "::ffff:10.1.2.3",
        "::ffff:10.0.0.0/104",
    ] {
        for deny in [false, true] {
            let entry = NetworkAccessRule {
                host: rule.into(),
                ports: vec![443],
                transports: vec![NetworkTransport::TcpTunnel],
                allow_private_ips: false,
            };
            let policy = NetworkPolicy::compile(&NetworkConfig {
                mode: if deny {
                    NetworkMode::Public
                } else {
                    NetworkMode::Allowlist
                },
                rules: if deny { vec![] } else { vec![entry.clone()] },
                deny_rules: if deny { vec![entry] } else { vec![] },
                ..Default::default()
            })
            .unwrap();
            for address in ["10.1.2.3", "::ffff:10.1.2.3"] {
                for literal in [false, true] {
                    let request = NetworkAccessRequest {
                        run_id: None,
                        attempt_id: None,
                        host: if literal { address } else { "service.example" }.into(),
                        port: Some(443),
                        transport: NetworkTransport::TcpTunnel,
                        resolved_ip: Some(address.parse().unwrap()),
                    };
                    assert_eq!(
                        policy.preflight(&request).is_ok(),
                        !deny,
                        "{rule} {address} literal={literal} deny={deny}"
                    );
                    let wrong_port = NetworkAccessRequest {
                        port: Some(80),
                        ..request.clone()
                    };
                    assert_eq!(policy.preflight(&wrong_port).is_ok(), deny);
                    let wrong_transport = NetworkAccessRequest {
                        transport: NetworkTransport::Https,
                        ..request.clone()
                    };
                    assert_eq!(policy.preflight(&wrong_transport).is_ok(), deny);
                    if literal {
                        let unresolved = NetworkAccessRequest {
                            resolved_ip: None,
                            ..request
                        };
                        assert_eq!(policy.preflight(&unresolved).is_ok(), !deny);
                    }
                }
            }
        }
    }
}

#[test]
fn native_ipv6_and_ipv4_compatible_addresses_do_not_gain_ipv4_grants() {
    let policy = NetworkPolicy::compile(&NetworkConfig {
        mode: NetworkMode::Allowlist,
        allowed_hosts: vec!["10.0.0.0/8".into()],
        ..Default::default()
    })
    .unwrap();
    for address in ["::10.1.2.3", "2001:db8::a01:203", "::1", "::ffff:11.1.2.3"] {
        let request = NetworkAccessRequest {
            run_id: None,
            attempt_id: None,
            host: address.into(),
            port: Some(443),
            transport: NetworkTransport::TcpTunnel,
            resolved_ip: Some(address.parse().unwrap()),
        };
        assert!(policy.preflight(&request).is_err(), "{address}");
    }
    let ipv6 = NetworkPolicy::compile(&NetworkConfig {
        mode: NetworkMode::Allowlist,
        allowed_hosts: vec!["2001:4860::/32".into()],
        ..Default::default()
    })
    .unwrap();
    let request = NetworkAccessRequest {
        run_id: None,
        attempt_id: None,
        host: "dns.example".into(),
        port: Some(443),
        transport: NetworkTransport::TcpTunnel,
        resolved_ip: Some("2001:4860:4860::8888".parse().unwrap()),
    };
    assert!(ipv6.preflight(&request).is_ok());
}
