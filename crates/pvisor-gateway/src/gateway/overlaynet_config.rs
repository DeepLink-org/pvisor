//! Adapt capture configuration to overlaynet's egress policy.

pub use pvisor_overlaynet::policy::*;

use crate::config::ProxyConfig;

impl PolicyConfig for ProxyConfig {
    fn network(&self) -> &crate::config::NetworkConfig {
        &self.network
    }
}
