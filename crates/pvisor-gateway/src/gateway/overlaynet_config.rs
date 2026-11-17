//! Adapt capture configuration to overlaynet's egress policy.

use pvisor_core::network::PolicyConfig;

use crate::config::ProxyConfig;

impl PolicyConfig for ProxyConfig {
    fn network(&self) -> &crate::config::NetworkConfig {
        &self.network
    }
}
