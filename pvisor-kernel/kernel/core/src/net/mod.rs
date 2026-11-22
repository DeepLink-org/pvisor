// SPDX-License-Identifier: MPL-2.0

mod iface;
mod route;
pub(crate) mod socket;
pub(crate) mod uts_ns;

pub(crate) fn tcp_listener_endpoints() -> alloc::vec::Vec<aster_bigtcp::wire::IpEndpoint> {
    iface::iter_all_ifaces()
        .flat_map(|iface| iface.tcp_listener_endpoints())
        .collect()
}

pub(crate) fn init() {
    iface::init();
    route::init();
    socket::netlink::init();
    socket::vsock::init();
}

/// Lazy init should be called after spawning init thread.
pub(crate) fn init_in_first_kthread() {
    iface::init_in_first_kthread();
}
