//! Resolve a logical target once, authorize each concrete address, and return
//! only addresses that the connector is permitted to use.

use std::collections::HashSet;
#[cfg(test)]
use std::net::IpAddr;
use std::net::SocketAddr;
use std::time::Duration;

use pvisor_core::ControlController;
use pvisor_core::NetworkAccessRequest;
use tokio::net::lookup_host;
use tokio::time::timeout;

use pvisor_core::network::{DenyReason, NetworkPolicy};

const DNS_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub(crate) struct AuthorizedTarget {
    pub(crate) host: String,
    pub(crate) addresses: Vec<SocketAddr>,
}

#[derive(Debug)]
pub(crate) enum TargetAuthorizationError {
    Denied(DenyReason),
    Resolve(anyhow::Error),
}

pub(crate) use pvisor_core::network::ResolvedAddressPolicy;

#[cfg(test)]
pub(crate) async fn authorize_target(
    controller: &dyn ControlController,
    policy: &NetworkPolicy,
    request: NetworkAccessRequest,
) -> Result<AuthorizedTarget, TargetAuthorizationError> {
    authorize_target_with_policy(controller, policy, request, ResolvedAddressPolicy::Strict).await
}

pub(crate) async fn authorize_target_with_policy(
    controller: &dyn ControlController,
    policy: &NetworkPolicy,
    mut request: NetworkAccessRequest,
    resolved_address_policy: ResolvedAddressPolicy,
) -> Result<AuthorizedTarget, TargetAuthorizationError> {
    request.resolved_ip = None;
    let grant = match policy.authorize(controller, &request) {
        Ok(()) => None,
        Err(reason) => {
            #[cfg(unix)]
            if matches!(
                reason,
                DenyReason::AllowlistEmpty
                    | DenyReason::NotInAllowlist
                    | DenyReason::PortNotAllowed
                    | DenyReason::TransportNotAllowed
            ) && pvisor_core::audit::enabled()
            {
                let prompt = pvisor_core::audit::AuditRequest {
                    scope: request
                        .run_id
                        .as_ref()
                        .zip(request.execution_id.as_ref())
                        .map(|(run_id, execution_id)| pvisor_core::audit::AuditScope {
                            run_id: run_id.to_string(),
                            execution_id: execution_id.to_string(),
                            view: "network".into(),
                        }),
                    kind: pvisor_core::audit::AuditKind::Network,
                    target: format!(
                        "{}:{} ({:?})",
                        request.host,
                        request.port.unwrap_or(0),
                        request.transport
                    ),
                    reason: reason.as_str().into(),
                    host: Some(request.host.clone()),
                    port: request.port,
                    transport: Some(request.transport),
                };
                let decision =
                    tokio::task::spawn_blocking(move || pvisor_core::audit::request(&prompt))
                        .await
                        .unwrap_or(pvisor_core::audit::AuditDecision::Deny);
                if decision == pvisor_core::audit::AuditDecision::Allow {
                    let grant = policy
                        .one_time_grant(&request)
                        .map_err(TargetAuthorizationError::Resolve)?;
                    grant
                        .authorize(controller, &request)
                        .map_err(TargetAuthorizationError::Denied)?;
                    Some(grant)
                } else {
                    return Err(TargetAuthorizationError::Denied(reason));
                }
            } else {
                return Err(TargetAuthorizationError::Denied(reason));
            }
            #[cfg(not(unix))]
            return Err(TargetAuthorizationError::Denied(reason));
        }
    };
    let policy = grant.as_ref().unwrap_or(policy);

    let port = request.port.ok_or_else(|| {
        TargetAuthorizationError::Resolve(anyhow::anyhow!(
            "network target `{}` has no effective port",
            request.host
        ))
    })?;
    let resolved = timeout(DNS_TIMEOUT, lookup_host((request.host.as_str(), port)))
        .await
        .map_err(|_| {
            TargetAuthorizationError::Resolve(anyhow::anyhow!(
                "DNS resolution for `{}` timed out",
                request.host
            ))
        })?
        .map_err(|error| {
            TargetAuthorizationError::Resolve(anyhow::anyhow!(
                "resolve `{}`: {error}",
                request.host
            ))
        })?
        .collect::<Vec<_>>();

    authorize_resolved_target_with_policy(
        controller,
        policy,
        request,
        resolved,
        resolved_address_policy,
    )
}

fn authorize_resolved_target_with_policy(
    controller: &dyn ControlController,
    policy: &NetworkPolicy,
    mut request: NetworkAccessRequest,
    resolved: impl IntoIterator<Item = SocketAddr>,
    resolved_address_policy: ResolvedAddressPolicy,
) -> Result<AuthorizedTarget, TargetAuthorizationError> {
    let mut seen = HashSet::new();
    let mut addresses = Vec::new();
    let mut denied = DenyReason::ResolvedAddressNotAllowed;
    let mut resolved_any = false;
    for address in resolved {
        resolved_any = true;
        if !seen.insert(address) {
            continue;
        }
        request.resolved_ip = Some(address.ip());
        let authorization =
            policy.authorize_resolved(controller, &request, resolved_address_policy);
        match authorization {
            Ok(()) => addresses.push(address),
            Err(reason) => denied = reason,
        }
    }
    if !resolved_any {
        return Err(TargetAuthorizationError::Resolve(anyhow::anyhow!(
            "DNS resolution for `{}` returned no addresses",
            request.host
        )));
    }
    if addresses.is_empty() {
        return Err(TargetAuthorizationError::Denied(denied));
    }
    Ok(AuthorizedTarget {
        host: request.host,
        addresses,
    })
}

/// The benchmarking range is commonly used as an opaque fake-IP namespace by
/// host DNS/TUN connectors. It is never a connector alias for an IP-literal
/// request, so a guest cannot use this exception as direct egress.
#[cfg(test)]
use pvisor_core::is_host_connector_alias;

#[cfg(test)]
mod tests {
    use super::*;
    use pvisor_core::network::{NetworkConfig, NetworkMode};

    use pvisor_core::{ControlReason, ControlRequest, ControlTransition, PolicyControlController};
    use pvisor_core::{NetworkAccessRule, NetworkTransport};

    #[rstest::rstest]
    fn vm_resolution_pins_only_safe_addresses_from_mixed_dns_answers() {
        let policy = NetworkPolicy::compile(&NetworkConfig {
            mode: NetworkMode::Public,
            ..Default::default()
        })
        .unwrap();
        let target = authorize_resolved_target_with_policy(
            &PolicyControlController,
            &policy,
            request("mixed.example"),
            ["169.254.169.254:443", "[::1]:443", "8.8.8.8:443"].map(|ip| ip.parse().unwrap()),
            ResolvedAddressPolicy::VmEgress,
        )
        .unwrap();
        assert_eq!(
            target.addresses,
            vec!["8.8.8.8:443".parse::<SocketAddr>().unwrap()]
        );
        assert!(matches!(
            authorize_resolved_target_with_policy(
                &PolicyControlController,
                &policy,
                request("metadata.example"),
                ["169.254.169.254:443".parse().unwrap()],
                ResolvedAddressPolicy::VmEgress
            ),
            Err(TargetAuthorizationError::Denied(
                DenyReason::ResolvedAddressNotAllowed
            ))
        ));
    }

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

    fn authorize_resolved_target(
        controller: &dyn ControlController,
        policy: &NetworkPolicy,
        request: NetworkAccessRequest,
        resolved: impl IntoIterator<Item = SocketAddr>,
    ) -> Result<AuthorizedTarget, TargetAuthorizationError> {
        authorize_resolved_target_with_policy(
            controller,
            policy,
            request,
            resolved,
            ResolvedAddressPolicy::Strict,
        )
    }

    #[rstest::fixture]
    fn host_allowlist(#[default("api.example.com")] host: &str) -> NetworkPolicy {
        NetworkPolicy::compile(&NetworkConfig {
            mode: NetworkMode::Allowlist,
            rules: vec![NetworkAccessRule {
                host: host.into(),
                ports: vec![443],
                transports: vec![NetworkTransport::TcpTunnel],
                allow_private_ips: false,
            }],
            ..NetworkConfig::default()
        })
        .unwrap()
    }

    #[rstest::rstest]
    #[case::short_ipv4("127.1", "127.0.0.1", "127.0.0.0/8")]
    #[case::octal_ipv4("0177.0.0.1", "127.0.0.1", "127.0.0.0/8")]
    #[case::hex_ipv4("0x7f000001", "127.0.0.1", "127.0.0.0/8")]
    #[case::integer_ipv4("2130706433", "127.0.0.1", "127.0.0.0/8")]
    #[case::trailing_dot("127.0.0.1.", "127.0.0.1", "127.0.0.0/8")]
    #[case::compressed_ipv6("[::1]", "::1", "::1/128")]
    #[case::expanded_ipv6("[0:0:0:0:0:0:0:1]", "::1", "::1/128")]
    #[case::mapped_ipv6("[::ffff:127.0.0.1]", "::ffff:127.0.0.1", "127.0.0.0/8")]
    fn alternative_ip_spellings_keep_resolved_address_policy(
        #[case] host: &str,
        #[case] expected_ip: &str,
        #[case] deny_host: &str,
        #[values(false, true)] allow_private_ips: bool,
        #[values(false, true)] explicit_deny: bool,
    ) {
        let ip: IpAddr = expected_ip.parse().unwrap();
        let url = reqwest::Url::parse(&format!("http://{host}:443/")).unwrap();
        let url_ip: IpAddr = url
            .host_str()
            .unwrap()
            .trim_matches(['[', ']'])
            .parse()
            .unwrap();
        assert_eq!(url_ip, ip, "{host}");
        let rule = NetworkAccessRule {
            host: host.trim_matches(['[', ']']).into(),
            ports: vec![443],
            transports: vec![NetworkTransport::TcpTunnel],
            allow_private_ips,
        };
        let policy = NetworkPolicy::compile(&NetworkConfig {
            mode: NetworkMode::Allowlist,
            rules: vec![rule.clone()],
            deny_rules: if explicit_deny {
                vec![NetworkAccessRule {
                    host: deny_host.into(),
                    ..rule
                }]
            } else {
                Vec::new()
            },
            ..NetworkConfig::default()
        })
        .unwrap();
        // Fake resolver output: never ask DNS about alternative spellings.
        let result = authorize_resolved_target(
            &PolicyControlController,
            &policy,
            request(host),
            [SocketAddr::new(ip, 443)],
        );
        // An exact IP rule is itself a private-address opt-in; only
        // hostname rules require the separate allow_private_ips flag.
        let exact_ip_rule = host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok();
        if (allow_private_ips || exact_ip_rule) && !explicit_deny {
            assert_eq!(result.unwrap().addresses, [SocketAddr::new(ip, 443)]);
        } else {
            assert!(
                matches!(result, Err(TargetAuthorizationError::Denied(_))),
                "{host}"
            );
        }
    }

    #[rstest::rstest]
    fn filters_private_addresses_and_deduplicates_public_results() {
        let policy = host_allowlist("api.example.com");
        let public: SocketAddr = "93.184.216.34:443".parse().unwrap();
        let private: SocketAddr = "127.0.0.1:443".parse().unwrap();
        let target = authorize_resolved_target(
            &PolicyControlController,
            &policy,
            request("api.example.com"),
            [private, public, public],
        )
        .unwrap();
        assert_eq!(target.addresses, [public]);
    }

    #[rstest::rstest]
    fn rejects_when_every_resolved_address_is_private() {
        let policy = host_allowlist("api.example.com");
        let result = authorize_resolved_target(
            &PolicyControlController,
            &policy,
            request("api.example.com"),
            [
                "127.0.0.1:443".parse().unwrap(),
                "[::1]:443".parse().unwrap(),
            ],
        );
        assert!(matches!(
            result,
            Err(TargetAuthorizationError::Denied(
                DenyReason::ResolvedAddressNotAllowed
            ))
        ));
    }

    #[rstest::rstest]
    fn strict_resolution_rejects_host_connector_aliases() {
        let policy = host_allowlist("api.example.com");
        let result = authorize_resolved_target(
            &PolicyControlController,
            &policy,
            request("api.example.com"),
            ["198.18.0.42:443".parse().unwrap()],
        );
        assert!(matches!(
            result,
            Err(TargetAuthorizationError::Denied(
                DenyReason::ResolvedAddressNotAllowed
            ))
        ));
    }

    #[rstest::rstest]
    fn vm_resolution_accepts_an_authorized_host_connector_alias() {
        let policy = host_allowlist("api.example.com");
        let alias: SocketAddr = "198.18.0.42:443".parse().unwrap();
        let target = authorize_resolved_target_with_policy(
            &PolicyControlController,
            &policy,
            request("api.example.com"),
            [alias],
            ResolvedAddressPolicy::HostConnectorAliases,
        )
        .unwrap();
        assert_eq!(target.addresses, [alias]);
    }

    #[rstest::rstest]
    fn explicit_connector_range_deny_is_not_bypassed() {
        let policy = NetworkPolicy::compile(&NetworkConfig {
            mode: NetworkMode::Allowlist,
            rules: vec![NetworkAccessRule {
                host: "api.example.com".into(),
                ports: vec![443],
                transports: vec![NetworkTransport::TcpTunnel],
                allow_private_ips: false,
            }],
            deny_rules: vec![NetworkAccessRule {
                host: "198.18.0.0/15".into(),
                ports: Vec::new(),
                transports: Vec::new(),
                allow_private_ips: false,
            }],
            ..NetworkConfig::default()
        })
        .unwrap();
        let result = authorize_resolved_target_with_policy(
            &PolicyControlController,
            &policy,
            request("api.example.com"),
            ["198.18.0.42:443".parse().unwrap()],
            ResolvedAddressPolicy::HostConnectorAliases,
        );
        assert!(matches!(
            result,
            Err(TargetAuthorizationError::Denied(DenyReason::ExplicitDeny))
        ));
    }

    #[rstest::rstest]
    fn connector_aliases_require_a_logical_hostname() {
        assert!(is_host_connector_alias(
            "api.example.com",
            "198.18.0.42".parse().unwrap()
        ));
        assert!(!is_host_connector_alias(
            "198.18.0.42",
            "198.18.0.42".parse().unwrap()
        ));
        assert!(!is_host_connector_alias(
            "api.example.com",
            "93.184.216.34".parse().unwrap()
        ));
    }

    #[rstest::rstest]
    fn explicit_cidr_deny_filters_only_matching_results() {
        let policy = NetworkPolicy::compile(&NetworkConfig {
            mode: NetworkMode::Public,
            deny_rules: vec![NetworkAccessRule {
                host: "10.0.0.0/8".into(),
                ports: Vec::new(),
                transports: Vec::new(),
                allow_private_ips: false,
            }],
            ..NetworkConfig::default()
        })
        .unwrap();
        let allowed: SocketAddr = "93.184.216.34:443".parse().unwrap();
        let target = authorize_resolved_target(
            &PolicyControlController,
            &policy,
            request("api.example.com"),
            ["10.0.0.1:443".parse().unwrap(), allowed],
        )
        .unwrap();
        assert_eq!(target.addresses, [allowed]);
    }

    #[rstest::rstest]
    fn empty_resolution_is_an_upstream_error_not_a_policy_denial() {
        let policy = host_allowlist("api.example.com");
        let result = authorize_resolved_target(
            &PolicyControlController,
            &policy,
            request("api.example.com"),
            [],
        );
        assert!(matches!(result, Err(TargetAuthorizationError::Resolve(_))));
    }

    struct PerAddressController;

    impl ControlController for PerAddressController {
        fn authorize(&self, request: ControlRequest<'_>) -> ControlTransition {
            if let ControlRequest::Network { request, .. } = &request
                && request.resolved_ip == Some("93.184.216.34".parse().unwrap())
            {
                return ControlTransition::denied(ControlReason::ExplicitlyDenied);
            }
            PolicyControlController.authorize(request)
        }
    }

    #[rstest::rstest]
    fn injected_controller_can_veto_one_concrete_address() {
        let policy = host_allowlist("api.example.com");
        let retained: SocketAddr = "1.1.1.1:443".parse().unwrap();
        let target = authorize_resolved_target(
            &PerAddressController,
            &policy,
            request("api.example.com"),
            ["93.184.216.34:443".parse().unwrap(), retained],
        )
        .unwrap();
        assert_eq!(target.addresses, [retained]);
    }

    struct DenyHostController;

    impl ControlController for DenyHostController {
        fn authorize(&self, request: ControlRequest<'_>) -> ControlTransition {
            if let ControlRequest::Network { request, .. } = request
                && request.host == "controller-denied.invalid"
            {
                return ControlTransition::denied(ControlReason::ExplicitlyDenied);
            }
            ControlTransition::allowed(ControlReason::AmbientNetwork)
        }
    }

    #[rstest::rstest]
    fn connector_aliases_do_not_bypass_the_logical_controller_decision() {
        let policy = host_allowlist("controller-denied.invalid");
        let result = authorize_resolved_target_with_policy(
            &DenyHostController,
            &policy,
            request("controller-denied.invalid"),
            ["198.18.0.42:443".parse().unwrap()],
            ResolvedAddressPolicy::HostConnectorAliases,
        );
        assert!(matches!(
            result,
            Err(TargetAuthorizationError::Denied(DenyReason::ExplicitDeny))
        ));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn injected_controller_denies_before_dns_resolution() {
        let policy = NetworkPolicy::compile(&NetworkConfig::default()).unwrap();
        let result = authorize_target(
            &DenyHostController,
            &policy,
            request("controller-denied.invalid"),
        )
        .await;
        assert!(matches!(
            result,
            Err(TargetAuthorizationError::Denied(DenyReason::ExplicitDeny))
        ));
    }

    struct AllowEverythingController;

    impl ControlController for AllowEverythingController {
        fn authorize(&self, _request: ControlRequest<'_>) -> ControlTransition {
            ControlTransition::allowed(ControlReason::AmbientNetwork)
        }
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn injected_controller_cannot_widen_compiled_policy() {
        let policy = NetworkPolicy::compile(&NetworkConfig {
            mode: NetworkMode::NoNetwork,
            ..NetworkConfig::default()
        })
        .unwrap();
        let result = authorize_target(
            &AllowEverythingController,
            &policy,
            request("must-not-resolve.invalid"),
        )
        .await;
        assert!(matches!(
            result,
            Err(TargetAuthorizationError::Denied(DenyReason::NoNetwork))
        ));
    }

    #[rstest::rstest]
    #[tokio::test]
    async fn static_denial_happens_before_dns_resolution() {
        let policy = NetworkPolicy::compile(&NetworkConfig {
            mode: NetworkMode::NoNetwork,
            ..NetworkConfig::default()
        })
        .unwrap();
        let result = authorize_target(
            &PolicyControlController,
            &policy,
            request("must-not-resolve.invalid"),
        )
        .await;
        assert!(matches!(
            result,
            Err(TargetAuthorizationError::Denied(DenyReason::NoNetwork))
        ));

        let policy = host_allowlist("allowed.example.com");
        let result = authorize_target(
            &PolicyControlController,
            &policy,
            request("unlisted.must-not-resolve.invalid"),
        )
        .await;
        assert!(matches!(
            result,
            Err(TargetAuthorizationError::Denied(DenyReason::NotInAllowlist))
        ));
    }

    #[rstest::rstest]
    #[case::before_range("198.17.255.255", false)]
    #[case::range_start("198.18.0.0", true)]
    #[case::first_half_end("198.18.255.255", true)]
    #[case::second_half_start("198.19.0.0", true)]
    #[case::range_end("198.19.255.255", true)]
    #[case::after_range("198.20.0.0", false)]
    #[case::different_prefix("197.18.0.1", false)]
    #[case::public("8.8.8.8", false)]
    #[case::private("10.0.0.1", false)]
    #[case::loopback("127.0.0.1", false)]
    #[case::ipv6("::1", false)]
    fn connector_aliases_require_both_the_range_and_a_logical_host(
        #[case] address: IpAddr,
        #[case] alias: bool,
    ) {
        assert_eq!(is_host_connector_alias("api.example.com", address), alias);
        assert!(!is_host_connector_alias(&address.to_string(), address));
    }

    #[rstest::rstest]
    fn connector_alias_tail_octets_do_not_change_classification(
        #[values(17, 18, 19, 20)] second: u8,
        #[values(0, 127, 255)] third: u8,
    ) {
        for fourth in 0..=255 {
            let address = IpAddr::from([198, second, third, fourth]);
            assert_eq!(
                is_host_connector_alias("api.example.com", address),
                matches!(second, 18 | 19),
                "{address}"
            );
        }
    }

    #[rstest::rstest]
    #[case::one_public(&[0], &[0])]
    #[case::all_private(&[3, 4, 5], &[])]
    #[case::duplicates(&[0, 0, 1, 0, 2], &[0, 1, 2])]
    #[case::mixed(&[5, 2, 3, 1, 2, 4], &[2, 1])]
    #[case::reverse_order(&[2, 1, 0], &[2, 1, 0])]
    #[case::long_repeated_prefix(&[0; 31], &[0])]
    fn authorized_results_are_public_deduplicated_and_first_seen_ordered(
        host_allowlist: NetworkPolicy,
        #[case] indexes: &[usize],
        #[case] expected: &[usize],
    ) {
        let candidates: [SocketAddr; 6] = [
            "1.1.1.1:443",
            "8.8.8.8:443",
            "9.9.9.9:443",
            "127.0.0.1:443",
            "10.0.0.1:443",
            "192.168.0.1:443",
        ]
        .map(|value| value.parse().unwrap());
        let resolved = indexes.iter().map(|index| candidates[*index]);
        let result = authorize_resolved_target(
            &PolicyControlController,
            &host_allowlist,
            request("api.example.com"),
            resolved,
        );
        if expected.is_empty() {
            assert!(matches!(
                result,
                Err(TargetAuthorizationError::Denied(
                    DenyReason::ResolvedAddressNotAllowed
                ))
            ));
        } else {
            assert_eq!(
                result.unwrap().addresses,
                expected
                    .iter()
                    .map(|index| candidates[*index])
                    .collect::<Vec<_>>()
            );
        }
    }

    #[rstest::rstest]
    #[case::range_start("198.18.0.0")]
    #[case::middle("198.18.127.128")]
    #[case::second_half("198.19.0.0")]
    #[case::range_end("198.19.255.255")]
    fn connector_policy_pins_the_authorized_alias(
        host_allowlist: NetworkPolicy,
        #[case] address: IpAddr,
    ) {
        let socket = SocketAddr::new(address, 443);
        let target = authorize_resolved_target_with_policy(
            &PolicyControlController,
            &host_allowlist,
            request("api.example.com"),
            [socket],
            ResolvedAddressPolicy::HostConnectorAliases,
        )
        .unwrap();
        assert_eq!(target.addresses, [socket]);
    }
}
