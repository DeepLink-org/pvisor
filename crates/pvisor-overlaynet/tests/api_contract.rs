use axum::http::{HeaderMap, HeaderValue};
use proptest::prelude::*;
use pvisor_core::{
    PolicyControlController,
    network::{
        NetworkAccessRule, NetworkBandwidthLimit, NetworkConfig, NetworkMode, NetworkPolicy,
    },
};
use pvisor_overlaynet::api::{
    BandwidthRegistry, EgressContext, EgressControl, EgressRuntime, ForwardingHeaders,
    InterceptionCoverage, InterceptionDriver, InterceptionMetrics, InterceptionObservation,
    InterceptionProfile, InterceptionSnapshot, PeerNetworkConfig, ROUTER_IPV4, TargetCounters,
    VmNetwork, VmNetworkConfig, VmNetworkConfiguration, VmNetworkRuntime,
};
use std::{
    io,
    net::Ipv4Addr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

fn policy(mode: NetworkMode) -> NetworkPolicy {
    NetworkPolicy::compile(&NetworkConfig {
        mode,
        ..NetworkConfig::default()
    })
    .unwrap()
}

#[rstest::fixture]
fn config() -> VmNetworkConfig {
    VmNetworkConfig::new(
        EgressRuntime::with_bandwidth_registry(
            policy(NetworkMode::Public),
            Arc::new(PolicyControlController),
            BandwidthRegistry::default(),
        ),
        EgressContext::default(),
    )
}

#[rstest::rstest]
fn configuration_keeps_default_resources_and_attribution(config: VmNetworkConfig) {
    assert_eq!(config.max_flows, 1024);
    assert!(config.gateway.is_none() && config.peer_network.is_none());
    assert!(config.context.run_id.is_none() && config.context.execution_id.is_none());
    assert_eq!(config.metrics.snapshot(), InterceptionSnapshot::default());
    assert_eq!(
        serde_json::from_str::<InterceptionSnapshot>("{}").unwrap(),
        InterceptionSnapshot::default()
    );
}

#[rstest::rstest]
#[case::proxy(InterceptionProfile::explicit_proxy, serde_json::json!({
    "driver": "explicit-proxy", "strength": "cooperative", "http": true,
    "tcp_connect": true, "dns": false, "udp": false,
    "inherited_by_children": false, "peer_ipv4": false
}))]
#[case::vm(InterceptionProfile::vm_smoltcp, serde_json::json!({
    "driver": "vm-smoltcp", "strength": "non-bypassable", "http": false,
    "tcp_connect": true, "dns": true, "udp": false,
    "inherited_by_children": true, "peer_ipv4": false
}))]
fn coverage_profiles_preserve_exact_capabilities_and_old_records(
    #[case] construct: fn() -> InterceptionProfile,
    #[case] expected: serde_json::Value,
) {
    let profile = construct();
    assert_eq!(serde_json::to_value(&profile).unwrap(), expected);
    assert_eq!(
        serde_json::from_value::<InterceptionProfile>(expected.clone()).unwrap(),
        profile
    );
    let mut legacy = expected;
    legacy.as_object_mut().unwrap().remove("peer_ipv4");
    assert_eq!(
        serde_json::from_value::<InterceptionProfile>(legacy).unwrap(),
        profile
    );
    let with_peers = InterceptionProfile {
        peer_ipv4: true,
        ..profile
    };
    assert_eq!(
        serde_json::from_value::<InterceptionProfile>(serde_json::to_value(&with_peers).unwrap())
            .unwrap(),
        with_peers
    );
}

#[rstest::rstest]
#[case::proxy(InterceptionDriver::ExplicitProxy, "explicit-proxy")]
#[case::vm(InterceptionDriver::VmSmoltcp, "vm-smoltcp")]
#[case::netns(InterceptionDriver::LinuxNetns, "linux-netns")]
#[case::seccomp(InterceptionDriver::LinuxSeccompNotify, "linux-seccomp-notify")]
fn driver_record_names_are_stable(#[case] driver: InterceptionDriver, #[case] name: &str) {
    assert_eq!(serde_json::to_value(driver).unwrap(), name);
    assert_eq!(
        serde_json::from_value::<InterceptionDriver>(name.into()).unwrap(),
        driver
    );
}

#[rstest::rstest]
#[case::empty(serde_json::json!({}), TargetCounters::default())]
#[case::allowed(serde_json::json!({"allowed": 7}), TargetCounters { allowed: 7, ..Default::default() })]
#[case::denied(serde_json::json!({"denied": 11}), TargetCounters { denied: 11, ..Default::default() })]
#[case::failed(serde_json::json!({"failed": u64::MAX}), TargetCounters { failed: u64::MAX, ..Default::default() })]
fn legacy_target_records_default_only_missing_counters(
    #[case] record: serde_json::Value,
    #[case] expected: TargetCounters,
) {
    let snapshot: InterceptionSnapshot = serde_json::from_value(serde_json::json!({
        "targets": { "HTTP example.com:80": record }
    }))
    .unwrap();
    assert_eq!(
        snapshot,
        InterceptionSnapshot {
            targets: [("HTTP example.com:80".into(), expected)].into(),
            ..Default::default()
        }
    );
    assert_eq!(
        serde_json::from_value::<InterceptionSnapshot>(serde_json::to_value(&snapshot).unwrap())
            .unwrap(),
        snapshot
    );
}

#[rstest::rstest]
#[case::legacy_default([192, 0, 2, 2], [2, 80, 86, 0, 0, 2])]
#[case::router([192, 0, 2, 1], [2, 80, 192, 0, 2, 1])]
#[case::after_default([192, 0, 2, 3], [2, 80, 192, 0, 2, 3])]
#[case::zero([0; 4], [2, 80, 0, 0, 0, 0])]
#[case::maximum([255; 4], [2, 80, 255, 255, 255, 255])]
#[case::private([10, 2, 3, 4], [2, 80, 10, 2, 3, 4])]
#[case::high_bits([128, 127, 255, 0], [2, 80, 128, 127, 255, 0])]
fn addressing_is_deterministic_without_allocating_membership(
    #[case] octets: [u8; 4],
    #[case] expected: [u8; 6],
) {
    let address = Ipv4Addr::from(octets);
    let mac = VmNetworkConfig::guest_mac(address);
    assert_eq!(mac, expected);
    assert_eq!(VmNetworkConfig::guest_mac(address), mac);
    assert_eq!(mac[0] & 3, 2);
    assert_eq!(ROUTER_IPV4, Ipv4Addr::new(192, 0, 2, 1));
}

#[rstest::rstest]
#[case::public(NetworkMode::Public, true)]
#[case::deny_all(NetworkMode::NoNetwork, false)]
#[case::allowlist(NetworkMode::Allowlist, false)]
fn restrictive_policy_cannot_construct_peer_authority(
    #[case] mode: NetworkMode,
    #[case] admitted: bool,
) {
    assert_eq!(
        EgressRuntime::for_peer_network(policy(mode), BandwidthRegistry::default()).is_ok(),
        admitted,
    );
}

fn assert_rejected_before_adoption(config: VmNetworkConfig, expected: &str) {
    let called = Arc::new(AtomicBool::new(false));
    let observed = called.clone();
    let error = VmNetwork::start_with_io::<tokio::io::DuplexStream, _>(config, move || {
        observed.store(true, Ordering::SeqCst);
        Err(io::Error::other("must not adopt"))
    })
    .err()
    .unwrap();
    assert!(error.to_string().contains(expected));
    assert!(!called.load(Ordering::SeqCst));
}

#[rstest::rstest]
fn invalid_capacity_rejects_before_transport_adoption(mut config: VmNetworkConfig) {
    config.max_flows = 0;
    assert_rejected_before_adoption(config, "max_flows");
}

#[rstest::rstest]
#[case::adoption(io::ErrorKind::Other, "adoption contract")]
#[case::broken_pipe(io::ErrorKind::BrokenPipe, "closed endpoint")]
#[case::permission(io::ErrorKind::PermissionDenied, "denied endpoint")]
fn failed_adoption_releases_the_transport_and_allows_a_new_owner(
    config: VmNetworkConfig,
    #[case] kind: io::ErrorKind,
    #[case] message: &'static str,
    #[values(1, 32, 1024)] capacity: usize,
) {
    assert_failed_adoption(config, kind, message, capacity);
}

#[rstest::rstest]
fn drop_releases_the_owned_transport_and_metrics_are_independent_snapshots(
    config: VmNetworkConfig,
) {
    let metrics: InterceptionMetrics = config.metrics.clone();
    let before = metrics.snapshot();
    let (host, mut guest) = tokio::io::duplex(4096);
    let network = VmNetwork::start_with_io(config, move || Ok(host)).unwrap();
    drop(network);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            use tokio::io::AsyncReadExt;
            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(3), guest.read(&mut [0; 1]))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
        });
    assert_eq!(metrics.snapshot(), before);
}

#[rstest::fixture]
fn headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.append("connection", HeaderValue::from_static("keep-alive"));
    headers.append(
        "connection",
        HeaderValue::from_static(" x-private-hop , upgrade "),
    );
    headers
}

#[rstest::rstest]
#[case::authorization("authorization", true, false, false)]
#[case::api_key("x-api-key", true, false, false)]
#[case::google_key("x-goog-api-key", true, false, false)]
#[case::expect("expect", true, false, false)]
#[case::host("host", true, false, false)]
#[case::length("content-length", true, true, true)]
#[case::connection("connection", true, true, true)]
#[case::proxy_connection("proxy-connection", true, true, true)]
#[case::keep_alive("keep-alive", true, true, true)]
#[case::proxy_authenticate("proxy-authenticate", true, true, true)]
#[case::proxy_authorization("proxy-authorization", true, true, true)]
#[case::te("te", true, true, true)]
#[case::trailer("trailer", true, true, true)]
#[case::trailers("trailers", true, true, true)]
#[case::transfer_encoding("transfer-encoding", true, true, true)]
#[case::upgrade("upgrade", true, true, true)]
#[case::nominated("x-private-hop", true, true, true)]
#[case::type_metadata("content-type", false, false, true)]
#[case::encoding_metadata("content-encoding", false, false, true)]
#[case::end_to_end("x-request-id", false, false, false)]
#[case::credential_prefix("authorization-extra", false, false, false)]
#[case::nomination_prefix("x-private-hop-other", false, false, false)]
fn forwarding_headers_follow_request_and_response_contracts(
    headers: HeaderMap,
    #[case] name: &str,
    #[case] upstream_removed: bool,
    #[case] unchanged_body_removed: bool,
    #[case] changed_body_removed: bool,
    #[values(false, true)] uppercase: bool,
) {
    let name = if uppercase {
        name.to_ascii_uppercase()
    } else {
        name.into()
    };
    assert_eq!(
        headers.skip_upstream_forward_header(&name),
        upstream_removed
    );
    assert_eq!(
        headers.skip_response_header_after_reframing(&name, false),
        unchanged_body_removed
    );
    assert_eq!(
        headers.skip_response_header_after_reframing(&name, true),
        changed_body_removed
    );
}

#[rstest::rstest]
#[case::absent(&[], false)]
#[case::empty(&[""], false)]
#[case::lowercase(&["websocket"], true)]
#[case::uppercase(&["WEBSOCKET"], true)]
#[case::mixed_list(&["h2c, WebSocket"], true)]
#[case::multiple_values(&["h2c", " WebSocket "], true)]
#[case::other_protocol(&["h2c"], false)]
#[case::prefix(&["xwebsocket"], false)]
#[case::suffix(&["websocketx"], false)]
#[case::substring(&["xwebsockety"], false)]
fn websocket_upgrade_requires_a_whole_protocol_token(
    #[case] values: &[&str],
    #[case] expected: bool,
) {
    let mut headers = HeaderMap::new();
    for value in values {
        headers.append("upgrade", value.parse().unwrap());
    }
    assert_eq!(headers.is_websocket_upgrade(), expected);
}

#[rstest::rstest]
#[case::empty(0)]
#[case::short(13)]
#[case::oversized(65_536)]
#[case::maximum(u32::MAX)]
fn malformed_transport_fails_closed_and_explicit_shutdown_reports_the_worker_error(
    config: VmNetworkConfig,
    #[case] length: u32,
    #[values(0, 1, 2, 3, 4)] split: usize,
) {
    assert_malformed_transport(config, length, split);
}

#[rstest::rstest]
#[case::zero(0, 0)]
#[case::different_counters(7, 11)]
#[case::maximum(u64::MAX, u64::MAX)]
fn legacy_snapshot_defaults_preserve_present_counters(
    #[case] seen: u64,
    #[case] denied: u64,
    #[values(false, true)] include_seen: bool,
    #[values(false, true)] include_denied: bool,
) {
    let mut record = serde_json::json!({});
    if include_seen {
        record["requests_seen"] = seen.into();
    }
    if include_denied {
        record["policy_denied"] = denied.into();
    }
    let snapshot: InterceptionSnapshot = serde_json::from_value(record).unwrap();
    assert_eq!(
        snapshot,
        InterceptionSnapshot {
            requests_seen: if include_seen { seen } else { 0 },
            policy_denied: if include_denied { denied } else { 0 },
            ..Default::default()
        }
    );
    let decoded: InterceptionSnapshot =
        serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
    assert_eq!(decoded, snapshot);
}

#[rstest::rstest]
#[case::scoped_grant(NetworkConfig {
    mode: NetworkMode::Allowlist,
    rules: vec![NetworkAccessRule { host: "api.example".into(), ports: vec![443],
        transports: vec![], allow_private_ips: false }], ..Default::default()
})]
#[case::explicit_deny(NetworkConfig {
    deny_rules: vec![NetworkAccessRule { host: "api.example".into(), ports: vec![443],
        transports: vec![], allow_private_ips: false }], ..Default::default()
})]
#[case::bandwidth(NetworkConfig {
    limits: vec![NetworkBandwidthLimit { host: None, port: Some(443), bytes_per_second: 1 }],
    ..Default::default()
})]
fn peer_admission_rejects_scoped_policy_and_bandwidth(#[case] network: NetworkConfig) {
    assert_peer_policy_rejected(network);
}

#[rstest::rstest]
#[case::empty(vec![String::new()])]
#[case::long_label(vec!["a".repeat(64)])]
#[case::total_length(vec![format!("{}.{}.a", "a".repeat(63), "a".repeat(63))])]
#[case::empty_label(vec!["one..two".into()])]
#[case::invalid_character(vec!["bad name".into()])]
#[case::too_many(vec!["valid".into(); 17])]
fn invalid_peer_alias_rejects_before_transport_adoption(
    config: VmNetworkConfig,
    #[case] aliases: Vec<String>,
    #[values(2, 254)] host: u8,
) {
    assert_invalid_peer_alias(config, aliases, host);
}

#[rstest::rstest]
#[case::ordinary(false)]
#[case::restricted_peer(true)]
fn policy_replacement_cannot_bypass_peer_authority(
    mut config: VmNetworkConfig,
    #[case] peer_capable: bool,
) {
    if peer_capable {
        config.egress = EgressRuntime::for_peer_network(
            policy(NetworkMode::Public),
            BandwidthRegistry::default(),
        )
        .unwrap();
    }
    let clone = config.egress.clone();
    clone.replace_policy(policy(if peer_capable {
        NetworkMode::NoNetwork
    } else {
        NetworkMode::Public
    }));
    config.peer_network = Some(PeerNetworkConfig {
        directory: "relative-unused-directory".into(),
        address: Ipv4Addr::new(192, 0, 2, 2),
        gateway: ROUTER_IPV4,
        aliases: vec![],
    });
    for egress in [config.egress.clone(), clone] {
        config.egress = egress;
        assert_rejected_before_adoption(config.clone(), "peer networking requires");
    }
}

fn assert_failed_adoption(
    mut config: VmNetworkConfig,
    kind: io::ErrorKind,
    message: &str,
    capacity: usize,
) {
    config.max_flows = capacity;
    let diagnostic = message.to_owned();
    let successor = config.clone();
    let (host, guest) = tokio::io::duplex(4096);
    let error = VmNetwork::start_with_io::<tokio::io::DuplexStream, _>(config, move || {
        assert!(tokio::runtime::Handle::try_current().is_ok());
        drop(host);
        Err(io::Error::new(kind, diagnostic))
    })
    .err()
    .unwrap();
    assert!(format!("{error:#}").contains(message));
    assert!(format!("{error:#}").contains("adopt VM network transport"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        use tokio::io::AsyncReadExt;
        let mut guest = guest;
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(3), guest.read(&mut [0; 1]))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    });
    let (host, _guest) = tokio::io::duplex(4096);
    let network = VmNetwork::start_with_io(successor, move || {
        assert!(tokio::runtime::Handle::try_current().is_ok());
        Ok(host)
    })
    .unwrap();
    assert_eq!(network.shutdown().unwrap(), InterceptionSnapshot::default());
}

fn assert_malformed_transport(config: VmNetworkConfig, length: u32, split: usize) {
    let (host, mut guest) = tokio::io::duplex(4096);
    let owner = VmNetwork::start_with_io(config, move || Ok(host)).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let header = length.to_be_bytes();
            guest.write_all(&header[..split]).await.unwrap();
            guest.write_all(&header[split..]).await.unwrap();
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(3), guest.read(&mut [0; 1]))
                    .await
                    .unwrap();
            assert_eq!(result.unwrap(), 0);
        });
    let error = owner.shutdown().unwrap_err();
    assert!(format!("{error:#}").contains("invalid libkrun Ethernet frame length"));
}

fn assert_invalid_peer_alias(mut config: VmNetworkConfig, aliases: Vec<String>, host: u8) {
    config.egress =
        EgressRuntime::for_peer_network(policy(NetworkMode::Public), BandwidthRegistry::default())
            .unwrap();
    config.peer_network = Some(PeerNetworkConfig {
        directory: "relative-unused-directory".into(),
        address: Ipv4Addr::new(192, 0, 2, host),
        gateway: ROUTER_IPV4,
        aliases,
    });
    assert_rejected_before_adoption(config, "invalid peer DNS alias");
}

fn assert_peer_policy_rejected(network: NetworkConfig) {
    let policy = NetworkPolicy::compile(&network).unwrap();
    assert!(EgressRuntime::for_peer_network(policy, BandwidthRegistry::default()).is_err());
    assert!(
        EgressRuntime::for_peer_network(
            self::policy(NetworkMode::Public),
            BandwidthRegistry::default()
        )
        .is_ok()
    );
}

fn invalid_aliases() -> impl Strategy<Value = Vec<String>> {
    prop_oneof![
        Just(vec![String::new()]),
        (64usize..=96).prop_map(|length| vec!["a".repeat(length)]),
        "[a-z]{1,24}".prop_map(|label| vec![format!("{label}..invalid")]),
        "[a-z]{1,24}".prop_map(|label| vec![format!("{label} invalid")]),
        (17usize..=24).prop_map(|count| vec!["valid".into(); count]),
        (63usize..=80).prop_map(|length| vec![format!(
            "{}.{}.a",
            "a".repeat(length),
            "b".repeat(63)
        )]),
    ]
}

fn scoped_policy() -> impl Strategy<Value = NetworkConfig> {
    (
        "[a-z]{1,16}\\.example",
        1u16..=u16::MAX,
        1u64..=1_000_000_000,
        0u8..3,
    )
        .prop_map(|(host, port, rate, kind)| {
            let rule = NetworkAccessRule {
                host,
                ports: vec![port],
                transports: vec![],
                allow_private_ips: false,
            };
            match kind {
                0 => NetworkConfig {
                    mode: NetworkMode::Allowlist,
                    rules: vec![rule],
                    ..Default::default()
                },
                1 => NetworkConfig {
                    deny_rules: vec![rule],
                    ..Default::default()
                },
                _ => NetworkConfig {
                    limits: vec![NetworkBandwidthLimit {
                        host: None,
                        port: Some(port),
                        bytes_per_second: rate,
                    }],
                    ..Default::default()
                },
            }
        })
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(
            concat!(env!("CARGO_MANIFEST_DIR"), "/proptest-regressions/api_contract.txt")
        ))),
        ..ProptestConfig::default()
    })]
    #[test]
    fn mac_derivation_preserves_address_identity_and_local_unicast_bits(octets in any::<[u8; 4]>()) {
        let address = Ipv4Addr::from(octets);
        let mac = VmNetworkConfig::guest_mac(address);
        prop_assert_eq!(mac, VmNetworkConfig::guest_mac(address));
        prop_assert_eq!(mac[0] & 3, 2);
        prop_assert_eq!(&mac[..2], &[2, 80]);
        if address == pvisor_overlaynet::api::GUEST_IPV4 {
            prop_assert_eq!(mac, [2, 80, 86, 0, 0, 2]);
        } else {
            prop_assert_eq!(&mac[2..], &octets);
        }
    }

    #[test]
    fn legacy_records_preserve_generated_present_and_missing_counters(
        seen in prop::option::of(any::<u64>()),
        denied in prop::option::of(any::<u64>()),
        targets in prop::collection::btree_map("[a-z]{1,12}\\.example", (
            prop::option::of(any::<u64>()), prop::option::of(any::<u64>()), prop::option::of(any::<u64>())
        ), 0..8),
    ) {
        let mut record = serde_json::json!({ "targets": {} });
        if let Some(value) = seen { record["requests_seen"] = value.into(); }
        if let Some(value) = denied { record["policy_denied"] = value.into(); }
        let mut expected = InterceptionSnapshot {
            requests_seen: seen.unwrap_or_default(), policy_denied: denied.unwrap_or_default(), ..Default::default()
        };
        for (host, (allowed, denied, failed)) in targets {
            let mut counts = serde_json::json!({});
            if let Some(value) = allowed { counts["allowed"] = value.into(); }
            if let Some(value) = denied { counts["denied"] = value.into(); }
            if let Some(value) = failed { counts["failed"] = value.into(); }
            record["targets"][&host] = counts;
            expected.targets.insert(host, TargetCounters {
                allowed: allowed.unwrap_or_default(), denied: denied.unwrap_or_default(), failed: failed.unwrap_or_default()
            });
        }
        let decoded: InterceptionSnapshot = serde_json::from_value(record).unwrap();
        prop_assert_eq!(&decoded, &expected);
        let roundtrip: InterceptionSnapshot = serde_json::from_slice(&serde_json::to_vec(&decoded).unwrap()).unwrap();
        prop_assert_eq!(roundtrip, expected);
    }

    #[test]
    fn connection_nomination_filters_whole_tokens_across_values_and_case(
        token in "x-owned-[a-z0-9-]{1,16}",
        nominated_upper in any::<bool>(), query_upper in any::<bool>(),
        spacing in prop::sample::select(vec!["", " ", "\t", " \t"]),
        position in 0usize..3,
    ) {
        let nominated = if nominated_upper { token.to_ascii_uppercase() } else { token.clone() };
        let query = if query_upper { token.to_ascii_uppercase() } else { token.clone() };
        let mut values = vec!["keep-alive".into(), "upgrade".into()];
        values.insert(position, format!("{spacing}other,{spacing}{nominated}{spacing},"));
        let mut headers = HeaderMap::new();
        for value in values { headers.append("connection", value.parse().unwrap()); }
        prop_assert!(headers.skip_upstream_forward_header(&query));
        let unrelated = format!("{token}-other");
        for changed in [false, true] {
            prop_assert!(headers.skip_response_header_after_reframing(&query, changed));
            prop_assert!(!headers.skip_response_header_after_reframing(&unrelated, changed));
        }
        prop_assert!(!headers.skip_upstream_forward_header(&unrelated));
    }

    #[test]
    fn websocket_detection_depends_on_protocol_tokens_not_substrings(
        values in prop::collection::vec(prop::collection::vec(prop_oneof![
            Just("websocket".to_owned()), Just("WebSocket".to_owned()),
            Just("xwebsocket".to_owned()), Just("websocketx".to_owned()), "[a-z]{1,16}"
        ], 0..6), 0..5),
        spacing in prop::sample::select(vec!["", " ", "\t"]),
    ) {
        let expected = values.iter().flatten().any(|token| token.eq_ignore_ascii_case("websocket"));
        let mut headers = HeaderMap::new();
        for tokens in values {
            let value = tokens.iter().map(|token| format!("{spacing}{token}{spacing}")).collect::<Vec<_>>().join(",");
            headers.append("upgrade", value.parse().unwrap());
        }
        prop_assert_eq!(headers.is_websocket_upgrade(), expected);
    }

    #[test]
    fn adoption_failures_release_endpoints_before_a_fresh_owner_starts(
        capacity in 1usize..=1024,
        kind in prop::sample::select(vec![io::ErrorKind::Other, io::ErrorKind::BrokenPipe, io::ErrorKind::PermissionDenied]),
        diagnostic in "[A-Za-z0-9 _-]{1,64}",
    ) {
        assert_failed_adoption(config(), kind, &diagnostic, capacity);
    }

    #[test]
    fn every_invalid_frame_length_fails_closed_across_prefix_splits(
        length in prop_oneof![0u32..14, 65_536u32..=u32::MAX], split in 0usize..=4,
    ) {
        assert_malformed_transport(config(), length, split);
    }

    #[test]
    fn malformed_aliases_reject_before_adopting_any_guest_transport(
        aliases in invalid_aliases(), host in 2u8..=254,
    ) {
        assert_invalid_peer_alias(config(), aliases, host);
    }

    #[test]
    fn scoped_policy_never_grants_unrestricted_peer_authority(network in scoped_policy()) {
        assert_peer_policy_rejected(network);
    }
}
