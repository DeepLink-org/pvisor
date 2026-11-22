// SPDX-License-Identifier: MPL-2.0

mod config;
pub mod device;
mod control;
mod port;

pub use port::{NamedPort, named_ports};

pub const DEVICE_NAME: &str = "Virtio-Console";
