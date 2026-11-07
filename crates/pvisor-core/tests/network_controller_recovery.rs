use pvisor_core::{
    ControlController, ControlReason, ControlRequest, ControlTransition, NetworkAccessRequest,
    NetworkAccessRule, NetworkCapability, NetworkConfig, NetworkDefaultAction, NetworkPolicy,
    NetworkTransport,
    network::{DenyReason, ResolvedAddressPolicy},
};
use std::{net::IpAddr, sync::Mutex};

struct RecordingController {
    decision: ControlTransition,
    addresses: Mutex<Vec<Option<IpAddr>>>,
}

impl ControlController for RecordingController {
    fn authorize(&self, request: ControlRequest<'_>) -> ControlTransition {
        let ControlRequest::Network { request, .. } = request else {
            panic!("expected network authorization");
        };
        assert_eq!(request.host, "service.example");
        assert_eq!(request.port, Some(443));
        assert_eq!(request.transport, NetworkTransport::TcpTunnel);
        self.addresses.lock().unwrap().push(request.resolved_ip);
        self.decision.clone()
    }
}

#[test]
fn resolved_controller_failures_do_not_leak_into_later_requests() {
    // README: preflight precedes the injected controller; no decision is cached
    // across requests or DNS addresses. authorize_resolved additionally promises
    // that a host-connector alias still passes logical policy AND controller.
    // Exercise 48 cases, each followed by a legal request (96 operations total).
    // No resolver, socket, clock, or external resource is involved.
    let policy = NetworkPolicy::compile(&NetworkConfig {
        capability: Some(NetworkCapability::Policy {
            default_action: NetworkDefaultAction::Deny,
            allow: vec![NetworkAccessRule {
                host: "service.example".into(),
                ports: vec![443],
                transports: vec![NetworkTransport::TcpTunnel],
                allow_private_ips: false,
            }],
            deny: vec![NetworkAccessRule {
                host: "198.18.0.2".into(),
                ports: vec![],
                transports: vec![],
                allow_private_ips: false,
            }],
            limits: vec![],
        }),
        ..Default::default()
    })
    .unwrap();
    let public: IpAddr = "8.8.8.8".parse().unwrap();
    let alias: IpAddr = "198.18.0.1".parse().unwrap();
    let blocked_alias: IpAddr = "198.18.0.2".parse().unwrap();
    let loopback: IpAddr = "127.0.0.1".parse().unwrap();
    let legal = NetworkAccessRequest {
        run_id: None,
        execution_id: None,
        host: "service.example".into(),
        port: Some(443),
        transport: NetworkTransport::TcpTunnel,
        resolved_ip: Some(public),
    };
    let allow = ControlTransition::allowed(ControlReason::NetworkAllowList);
    let deny = ControlTransition::denied(ControlReason::NetworkDenied);
    let decisions = [
        (allow.clone(), Ok(())),
        (deny.clone(), Err(DenyReason::NoNetwork)),
        (allow.applied().unwrap(), Err(DenyReason::InvalidController)),
        (allow.failed().unwrap(), Err(DenyReason::InvalidController)),
        (deny.applied().unwrap(), Err(DenyReason::InvalidController)),
        (deny.failed().unwrap(), Err(DenyReason::InvalidController)),
    ];
    let mut controller = RecordingController {
        decision: allow.clone(),
        addresses: Mutex::new(vec![]),
    };
    for resolution in [
        ResolvedAddressPolicy::Strict,
        ResolvedAddressPolicy::HostConnectorAliases,
    ] {
        for address in [public, alias, blocked_alias, loopback] {
            for (decision, controller_result) in &decisions {
                controller.decision = decision.clone();
                let mut input = legal.clone();
                input.resolved_ip = Some(address);
                // A small independent oracle: explicit deny wins; only the
                // trusted connector may replace an alias with a logical check.
                let (expected, seen) = if address == blocked_alias {
                    (Err(DenyReason::ExplicitDeny), vec![])
                } else if address == public {
                    (controller_result.clone(), vec![Some(public)])
                } else if address == alias
                    && resolution == ResolvedAddressPolicy::HostConnectorAliases
                {
                    (controller_result.clone(), vec![None])
                } else {
                    (Err(DenyReason::ResolvedAddressNotAllowed), vec![])
                };
                let context = format!("{resolution:?}, {address}, {decision:?}");
                assert_eq!(
                    policy.authorize_resolved(&controller, &input, resolution),
                    expected,
                    "{context}"
                );
                assert_eq!(
                    std::mem::take(controller.addresses.get_mut().unwrap()),
                    seen,
                    "{context}"
                );
                assert_eq!(input.resolved_ip, Some(address), "{context}");

                // Reuse the exact policy/controller and logical destination.
                // An error must not poison the next request, and a previous
                // allow must not bypass the next concrete-address check.
                controller.decision = allow.clone();
                assert_eq!(policy.authorize(&controller, &legal), Ok(()), "{context}");
                assert_eq!(
                    std::mem::take(controller.addresses.get_mut().unwrap()),
                    vec![Some(public)],
                    "{context}"
                );
            }
        }
    }
}
