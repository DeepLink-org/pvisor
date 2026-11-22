// SPDX-License-Identifier: MPL-2.0

use aster_util::printer::VmPrinter;
use spin::Once;

use crate::{
    events::IoEvents,
    fs::{
        file::{AccessMode, InodeType, PerOpenFileOps, StatusFlags, mkmod},
        procfs::{
            StaticEntry,
            template::{
                ProcDir, ProcDirOps, ProcFile, ProcFileOps, ProcFileOpsByHandle, ReaddirEntry,
                listed_entries_from_table, lookup_child_from_table, visit_listed_entries,
            },
        },
        vfs::inode::{FileOps, Inode},
    },
    prelude::*,
    process::signal::{PollHandle, Pollable},
};

pub(super) struct RandomDirOps;

impl RandomDirOps {
    pub(super) fn new_inode(parent: Weak<dyn Inode>) -> Arc<dyn Inode> {
        ProcDir::new(Self, parent, mkmod!(a + rx))
    }

    const ENTRIES: &'static [StaticEntry] = &[
        ("boot_id", InodeType::File, |parent| {
            ProcFile::new(BootId, parent, mkmod!(a + r))
        }),
        ("uuid", InodeType::File, |parent| {
            ProcFile::new(RandomUuid, parent, mkmod!(a + r))
        }),
    ];
}

impl ProcDirOps for RandomDirOps {
    fn lookup_child(&self, dir: &ProcDir<Self>, name: &str) -> Result<Arc<dyn Inode>> {
        lookup_child_from_table(name, Self::ENTRIES, |f| f(dir.this_weak().clone()))
            .ok_or_else(|| Error::with_message(Errno::ENOENT, "the file does not exist"))
    }

    fn visit_entries_from_offset<'a, F>(&'a self, offset: usize, visit_fn: F) -> Result<()>
    where
        F: FnMut(ReaddirEntry<'a>) -> Result<()>,
    {
        visit_listed_entries(offset, listed_entries_from_table(Self::ENTRIES), visit_fn)
    }
}

struct BootId;

impl ProcFileOps for BootId {
    fn read_at(&self, offset: usize, writer: &mut VmWriter) -> Result<usize> {
        static ID: Once<[u8; 16]> = Once::new();
        print_uuid(ID.call_once(new_uuid), offset, writer)
    }
}

struct RandomUuid;

impl ProcFileOpsByHandle for RandomUuid {
    fn open(&self, _: AccessMode, _: StatusFlags) -> Result<Box<dyn PerOpenFileOps>> {
        Ok(Box::new(UuidFile(new_uuid())))
    }
}

struct UuidFile([u8; 16]);

impl Pollable for UuidFile {
    fn poll(&self, mask: IoEvents, _: Option<&mut PollHandle>) -> IoEvents {
        mask & IoEvents::IN
    }
}

impl FileOps for UuidFile {
    fn read_at(&self, offset: usize, writer: &mut VmWriter, _: StatusFlags) -> Result<usize> {
        print_uuid(&self.0, offset, writer)
    }

    fn write_at(&self, _: usize, _: &mut VmReader, _: StatusFlags) -> Result<usize> {
        return_errno!(Errno::EPERM);
    }
}

impl PerOpenFileOps for UuidFile {
    fn check_seekable(&self) -> Result<()> {
        Ok(())
    }

    fn is_offset_aware(&self) -> bool {
        true
    }
}

fn new_uuid() -> [u8; 16] {
    let mut bytes = [0; 16];
    crate::util::random::getrandom(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes
}

fn print_uuid(id: &[u8; 16], offset: usize, writer: &mut VmWriter) -> Result<usize> {
    let mut printer = VmPrinter::new_skip(writer, offset);
    for (i, byte) in id.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            write!(printer, "-")?;
        }
        write!(printer, "{byte:02x}")?;
    }
    writeln!(printer)?;
    Ok(printer.bytes_written())
}
