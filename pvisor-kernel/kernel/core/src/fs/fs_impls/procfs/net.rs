// SPDX-License-Identifier: MPL-2.0

use aster_bigtcp::wire::IpAddress;
use aster_util::printer::VmPrinter;

use super::{
    StaticEntry,
    template::{
        ProcDir, ProcDirOps, ProcFile, ProcFileOps, ReaddirEntry, listed_entries_from_table,
        lookup_child_from_table, visit_listed_entries,
    },
};
use crate::{
    fs::{
        file::{InodeType, mkmod},
        vfs::inode::Inode,
    },
    prelude::*,
};

pub(super) struct NetDirOps;

impl NetDirOps {
    pub(super) fn new_inode(parent: Weak<dyn Inode>) -> Arc<dyn Inode> {
        ProcDir::new(Self, parent, mkmod!(a + rx))
    }

    const ENTRIES: &'static [StaticEntry] = &[("tcp", InodeType::File, |parent| {
        ProcFile::new(TcpListeners, parent, mkmod!(a + r))
    })];
}

impl ProcDirOps for NetDirOps {
    fn lookup_child(&self, dir: &ProcDir<Self>, name: &str) -> Result<Arc<dyn Inode>> {
        lookup_child_from_table(name, Self::ENTRIES, |f| f(dir.this_weak().clone()))
            .ok_or_else(|| Error::new(Errno::ENOENT))
    }

    fn visit_entries_from_offset<'a, F>(&'a self, offset: usize, visit: F) -> Result<()>
    where
        F: FnMut(ReaddirEntry<'a>) -> Result<()>,
    {
        visit_listed_entries(offset, listed_entries_from_table(Self::ENTRIES), visit)
    }
}

struct TcpListeners;

impl ProcFileOps for TcpListeners {
    fn read_at(&self, offset: usize, writer: &mut VmWriter) -> Result<usize> {
        let mut printer = VmPrinter::new_skip(writer, offset);
        writeln!(
            printer,
            "  sl  local_address rem_address   st tx_queue:rx_queue tr:tm->when retrnsmt   uid  timeout inode"
        )?;
        for (index, (addr, port)) in crate::net::tcp_listener_endpoints()
            .into_iter()
            .filter_map(|endpoint| match endpoint.addr {
                IpAddress::Ipv4(addr) => Some((u32::from_le_bytes(addr.octets()), endpoint.port)),
                IpAddress::Ipv6(_) => None,
            })
            .enumerate()
        {
            writeln!(
                printer,
                "{index:4}: {addr:08X}:{port:04X} 00000000:0000 0A 00000000:00000000 00:00000000 00000000 0 0 0"
            )?;
        }
        Ok(printer.bytes_written())
    }
}
