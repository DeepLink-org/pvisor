//! Public network-policy contracts: address matching, approval and controllers.
//! Pure requests and immutable policies only; no resolver, process or socket.

use pvisor_core::{NetworkAccessRequest, NetworkAccessRule, NetworkTransport};

#[path = "network/addresses.rs"]
mod addresses;
#[path = "network/approval.rs"]
mod approval;
#[path = "network/controllers.rs"]
mod controllers;

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
