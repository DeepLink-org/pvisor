use axum::http::HeaderMap;
use pvisor_core::{
    PolicyControlController,
    network::{NetworkConfig, NetworkMode, NetworkPolicy},
};
use pvisor_overlaynet::api::{
    BandwidthRegistry, EgressContext, EgressControl, EgressRuntime, ForwardingHeaders, GUEST_IPV4,
    InterceptionCoverage, InterceptionMetrics, InterceptionObservation, InterceptionProfile,
    InterceptionSnapshot, ROUTER_IPV4, VmNetwork, VmNetworkConfig, VmNetworkConfiguration,
    VmNetworkRuntime,
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

#[test]
fn configuration_profiles_and_old_records_keep_their_defaults() {
    let config = config();
    assert_eq!(config.max_flows, 1024);
    assert!(config.gateway.is_none() && config.peer_network.is_none());
    assert!(config.context.run_id.is_none() && config.context.execution_id.is_none());
    assert_eq!(config.metrics.snapshot(), InterceptionSnapshot::default());
    assert_eq!(
        serde_json::from_str::<InterceptionSnapshot>("{}").unwrap(),
        InterceptionSnapshot::default()
    );
    let proxy = InterceptionProfile::explicit_proxy();
    let vm = InterceptionProfile::vm_smoltcp();
    assert!(proxy.strength != pvisor_overlaynet::api::InterceptionStrength::NonBypassable);
    assert!(
        vm.strength == pvisor_overlaynet::api::InterceptionStrength::NonBypassable
            && vm.dns
            && vm.tcp_connect
    );
    assert!(!vm.udp && !vm.peer_ipv4);
    let mut old = serde_json::to_value(vm).unwrap();
    old.as_object_mut().unwrap().remove("peer_ipv4");
    assert!(
        !serde_json::from_value::<InterceptionProfile>(old)
            .unwrap()
            .peer_ipv4
    );
}

#[test]
fn addressing_is_deterministic_without_allocating_membership() {
    assert_eq!(VmNetworkConfig::guest_mac(GUEST_IPV4), [2, 80, 86, 0, 0, 2]);
    assert_eq!(ROUTER_IPV4, Ipv4Addr::new(192, 0, 2, 1));
    assert_eq!(
        VmNetworkConfig::guest_mac(Ipv4Addr::new(10, 2, 3, 4)),
        [2, 80, 10, 2, 3, 4]
    );
}

#[test]
fn restrictive_policy_cannot_construct_peer_authority() {
    assert!(
        EgressRuntime::for_peer_network(
            policy(NetworkMode::NoNetwork),
            BandwidthRegistry::default()
        )
        .is_err()
    );
    assert!(
        EgressRuntime::for_peer_network(policy(NetworkMode::Public), BandwidthRegistry::default())
            .is_ok()
    );
}

#[test]
fn invalid_capacity_rejects_before_transport_adoption() {
    let mut config = config();
    config.max_flows = 0;
    let called = Arc::new(AtomicBool::new(false));
    let observed = called.clone();
    let error = VmNetwork::start_with_io::<tokio::io::DuplexStream, _>(config, move || {
        observed.store(true, Ordering::SeqCst);
        Err(io::Error::other("must not adopt"))
    })
    .err()
    .unwrap();
    assert!(error.to_string().contains("max_flows"));
    assert!(!called.load(Ordering::SeqCst));
}

#[test]
fn failed_adoption_releases_the_transport_and_allows_a_new_owner() {
    let (host, guest) = tokio::io::duplex(4096);
    let error = VmNetwork::start_with_io::<tokio::io::DuplexStream, _>(config(), move || {
        drop(host);
        Err(io::Error::other("adoption contract"))
    })
    .err()
    .unwrap();
    assert!(format!("{error:#}").contains("adoption contract"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        use tokio::io::AsyncReadExt;
        let mut guest = guest;
        assert_eq!(guest.read(&mut [0; 1]).await.unwrap(), 0);
    });
    let (host, _guest) = tokio::io::duplex(4096);
    let network = VmNetwork::start_with_io(config(), move || {
        assert!(tokio::runtime::Handle::try_current().is_ok());
        Ok(host)
    })
    .unwrap();
    assert_eq!(network.shutdown().unwrap(), InterceptionSnapshot::default());
}

#[test]
fn drop_releases_the_owned_transport_and_metrics_are_independent_snapshots() {
    let config = config();
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
            assert_eq!(guest.read(&mut [0; 1]).await.unwrap(), 0);
        });
    assert_eq!(metrics.snapshot(), before);
}

#[test]
fn forwarding_headers_enforce_credentials_framing_and_connection_tokens() {
    let mut headers = HeaderMap::new();
    headers.insert("connection", "x-private-hop".parse().unwrap());
    headers.insert("upgrade", "h2c, WebSocket".parse().unwrap());
    for name in [
        "Authorization",
        "x-api-key",
        "x-private-hop",
        "content-length",
        "host",
    ] {
        assert!(headers.skip_upstream_forward_header(name));
    }
    assert!(!headers.skip_upstream_forward_header("x-request-id"));
    assert!(headers.skip_response_header_after_reframing("content-length", false));
    assert!(!headers.skip_response_header_after_reframing("content-type", false));
    assert!(headers.skip_response_header_after_reframing("content-type", true));
    assert!(headers.is_websocket_upgrade());
}

#[test]
fn malformed_transport_fails_closed_and_explicit_shutdown_reports_the_worker_error() {
    let (host, mut guest) = tokio::io::duplex(4096);
    let owner = VmNetwork::start_with_io(config(), move || Ok(host)).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            guest.write_all(&65_536_u32.to_be_bytes()).await.unwrap();
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(3), guest.read(&mut [0; 1]))
                    .await
                    .unwrap();
            assert_eq!(result.unwrap(), 0);
        });
    let error = owner.shutdown().unwrap_err();
    assert!(format!("{error:#}").contains("invalid libkrun Ethernet frame length"));
}
