// SPDX-License-Identifier: MPL-2.0

use aster_bigtcp::{
    iface::{InterfaceFlags, InterfaceType},
    wire::{IpCidr, Ipv4Address, Ipv4Cidr},
};

use crate::net::{
    iface::iter_all_ifaces,
    route::configure_ipv4,
    socket::util::{ioctl::CSocketAddr, options::check_current_privileged},
};
use crate::{dispatch_ioctl, net::socket::util::ioctl::CIfReq, prelude::*, util::ioctl::RawIoctl};

/// Linux `struct rtentry` on the supported 64-bit architectures.
#[padding_struct]
#[repr(C)]
#[derive(Clone, Copy, Pod)]
struct RouteReq {
    pad1: usize,
    dst: CSocketAddr,
    gateway: CSocketAddr,
    mask: CSocketAddr,
    flags: u16,
    pad2: i16,
    pad3: usize,
    tos: u8,
    class: u8,
    pad4: [i16; 3],
    metric: i16,
    dev: Vaddr,
    mtu: usize,
    window: usize,
    irtt: u16,
}

const _: () = {
    assert!(size_of::<RouteReq>() == 120);
    assert!(core::mem::offset_of!(RouteReq, dev) == 88);
};

impl RouteReq {
    fn add_default(&self) -> Result<()> {
        const RTF_UP: u16 = 1;
        const RTF_GATEWAY: u16 = 2;
        if self.flags != (RTF_UP | RTF_GATEWAY)
            || self.dst.ipv4()? != Ipv4Address::UNSPECIFIED
            || self.mask.ipv4()? != Ipv4Address::UNSPECIFIED
            || self.dev != 0
            || self.metric != 0
            || self.tos != 0
            || self.class != 0
            || self.mtu != 0
            || self.window != 0
            || self.irtt != 0
        {
            return_errno!(Errno::EOPNOTSUPP);
        }
        let gw = self.gateway.ipv4()?;
        configure_ipv4(|| {
            let iface = iter_all_ifaces()
                .find(|i| {
                    i.type_() == InterfaceType::ETHER
                        && i.ipv4_cidr().is_some_and(|c| {
                            c.contains_addr(&gw)
                                && gw != c.address()
                                && gw != c.network().address()
                                && Some(gw) != c.broadcast()
                        })
                })
                .ok_or_else(|| Error::new(Errno::ENETUNREACH))?;
            if iface
                .routes()
                .iter()
                .any(|r| matches!(r.cidr, IpCidr::Ipv4(c) if c.prefix_len() == 0))
            {
                return_errno!(Errno::EEXIST);
            }
            if !iface.add_ipv4_gateway(gw) {
                return_errno!(Errno::ENOBUFS);
            }
            Ok(())
        })
    }
}

fn set_ifaddr(mut req: CIfReq, mask: bool) -> Result<i32> {
    let ipv4 = req.ipv4()?;
    let iface = req.get_iface_by_name()?;
    if iface.type_() != InterfaceType::ETHER {
        return_errno!(Errno::EOPNOTSUPP);
    }
    configure_ipv4(|| {
        let old = iface
            .ipv4_cidr()
            .ok_or_else(|| Error::new(Errno::EADDRNOTAVAIL))?;
        let next = if mask {
            Ipv4Cidr::from_netmask(old.address(), ipv4).map_err(|_| Error::new(Errno::EINVAL))?
        } else {
            if ipv4.is_multicast() || ipv4.is_broadcast() || ipv4.is_unspecified() {
                return_errno!(Errno::EINVAL);
            }
            Ipv4Cidr::new(ipv4, old.prefix_len())
        };
        iface.set_ipv4_cidr(next);
        Ok(())
    })?;
    Ok(0)
}

mod ioctl_defs {
    use super::{CIfReq, RouteReq};
    use crate::{
        ioc,
        util::ioctl::{InData, InOutData},
    };

    // Reference: <https://elixir.bootlin.com/linux/v7.1/source/include/uapi/linux/sockios.h#L62>.
    pub(super) type GetIfAddr = ioc!(SIOCGIFADDR, 0x8915, InOutData<CIfReq>);
    pub(super) type GetIfDstAddr = ioc!(SIOCGIFDSTADDR, 0x8917, InOutData<CIfReq>);
    pub(super) type GetIfBrdAddr = ioc!(SIOCGIFBRDADDR, 0x8919, InOutData<CIfReq>);
    pub(super) type GetIfNetmask = ioc!(SIOCGIFNETMASK, 0x891B, InOutData<CIfReq>);
    pub(super) type SetIfAddr = ioc!(SIOCSIFADDR, 0x8916, InData<CIfReq>);
    pub(super) type SetIfMask = ioc!(SIOCSIFNETMASK, 0x891C, InData<CIfReq>);
    pub(super) type AddRoute = ioc!(SIOCADDRT, 0x890B, InData<RouteReq>);
}

pub(super) fn ipv4_ioctl(raw_ioctl: RawIoctl) -> Result<i32> {
    use ioctl_defs::*;

    dispatch_ioctl!(match raw_ioctl {
        cmd @ SetIfAddr => {
            check_current_privileged()?;
            set_ifaddr(cmd.read()?, false)
        }
        cmd @ SetIfMask => {
            check_current_privileged()?;
            set_ifaddr(cmd.read()?, true)
        }
        cmd @ AddRoute => {
            check_current_privileged()?;
            cmd.read()?.add_default()?;
            Ok(0)
        }
        cmd @ GetIfAddr => {
            let mut ifreq = cmd.read()?;
            let iface = ifreq.get_iface_by_name()?;
            let ipv4_addr = iface
                .ipv4_cidr()
                .ok_or_else(|| Error::with_message(Errno::EADDRNOTAVAIL, "no IPv4 address found"))?
                .address();
            ifreq.set_sockaddr_ipv4(ipv4_addr);
            cmd.write(&ifreq)?;
            Ok(0)
        }
        cmd @ GetIfDstAddr => {
            let mut ifreq = cmd.read()?;
            let iface = ifreq.get_iface_by_name()?;
            // Rust native kernel does not yet support point-to-point interfaces,
            // so we report the local IPv4 address instead, consistent with Linux's behavior.
            let ipv4_addr = iface
                .ipv4_cidr()
                .ok_or_else(|| Error::with_message(Errno::EADDRNOTAVAIL, "no IPv4 address found"))?
                .address();
            ifreq.set_sockaddr_ipv4(ipv4_addr);
            cmd.write(&ifreq)?;
            Ok(0)
        }
        cmd @ GetIfBrdAddr => {
            let mut ifreq = cmd.read()?;
            let iface = ifreq.get_iface_by_name()?;
            let broadcast_addr = if iface.flags().contains(InterfaceFlags::BROADCAST)
                && let Some(broadcast_addr) = iface.broadcast_addr()
            {
                broadcast_addr
            } else {
                Ipv4Address::UNSPECIFIED
            };
            ifreq.set_sockaddr_ipv4(broadcast_addr);
            cmd.write(&ifreq)?;
            Ok(0)
        }
        cmd @ GetIfNetmask => {
            let mut ifreq = cmd.read()?;
            let iface = ifreq.get_iface_by_name()?;
            let netmask = iface
                .ipv4_cidr()
                .ok_or_else(|| Error::with_message(Errno::EADDRNOTAVAIL, "no IPv4 address found"))?
                .netmask();
            ifreq.set_sockaddr_ipv4(netmask);
            cmd.write(&ifreq)?;
            Ok(0)
        }
        _ => return_errno_with_message!(Errno::ENOTTY, "the socket ioctl command is unknown"),
    })
}
