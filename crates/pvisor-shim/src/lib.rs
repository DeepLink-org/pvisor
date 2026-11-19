//! containerd Runtime v2 shim with one public executable entry.
#![deny(missing_docs)]
// Portable tests compile shared records without their Linux syscall consumers.
#![cfg_attr(all(test, not(target_os = "linux")), allow(dead_code))]

pub mod api;
#[cfg(any(target_os = "linux", test))]
mod caps;
#[cfg(any(target_os = "linux", test))]
mod cgroup;
#[cfg(target_os = "linux")]
mod child;
#[cfg(any(target_os = "linux", test))]
// Consumed by the VM feature path; kept compiled for tests without it.
#[cfg_attr(not(any(feature = "vm", test)), allow(dead_code))]
mod config;
#[cfg(any(target_os = "linux", test))]
mod device_policy;
mod entry;
#[cfg(target_os = "linux")]
mod fifo;
#[cfg(any(target_os = "linux", test))]
mod inject;
#[cfg(any(target_os = "linux", test))]
mod masked_path;
#[cfg(target_os = "linux")]
mod mount;
#[cfg(all(target_os = "linux", feature = "vm"))]
mod network;
#[cfg(any(target_os = "linux", test))]
mod plan;
#[cfg(target_os = "linux")]
mod service;
#[cfg(any(target_os = "linux", test))]
mod sockets;
#[cfg(any(target_os = "linux", test))]
mod spec;
#[cfg(any(target_os = "linux", test))]
mod state;
#[cfg(all(target_os = "linux", feature = "vm"))]
mod vm;

#[cfg(all(test, target_os = "linux"))]
mod internal_spawn_lifetime_tests;
#[cfg(test)]
mod policy_contract_tests;
