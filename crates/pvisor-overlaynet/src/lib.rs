//! Network interception and egress policy drivers for pVisor.
//!
//! Host/container execution uses the cooperative HTTP proxy. libkrun VM
//! execution uses a non-bypassable virtio-net/smoltcp IPv4 TCP and DNS data
//! plane; unsupported egress protocols fail closed. Explicit same-host peer
//! networks forward IPv4 TCP/UDP/ICMP before smoltcp and can resolve live member
//! names from private discovery records.

#![deny(missing_docs)]

pub mod api;
mod bandwidth;
mod egress;
mod forward;
mod headers;
mod interception;
#[cfg_attr(windows, path = "peer_windows.rs")]
mod peer;
#[cfg(unix)]
mod peer_batch;
mod peer_protocol;
#[cfg(any(windows, test))]
mod peer_udp;
mod policy;
mod resolver;
mod server;
mod vm;
