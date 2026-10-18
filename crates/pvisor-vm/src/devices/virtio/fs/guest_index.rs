// Guest-visible directory pages for the virtio-fs metadata cache.
//
// One `READINDEX` reply is a `ReadIndexOut` header followed by FUSE
// `fuse_direntplus` records. The guest asks for up to 1 MiB. Importing a
// page is a `readdirplus` preceded by the filesystem's synthetic names
// (injected files such as `init.krun`, which ordinary readdir hides). Each
// positive backing entry increases the host lookup count by one, and the
// guest accounts for that count when it instantiates the inode. A later page
// must not be treated as proof that a missing name does not exist; only the
// `COMPLETE` flag does that. Network and I/O errors stay errors.

use std::io;
use std::mem::size_of;
use std::time::Duration;

use super::super::linux_errno::linux_error;
use super::filesystem::{Context, DirEntry, Entry, FileSystem};
use super::fuse::{Attr, Dirent, EntryOut, ReadIndexOut, READINDEX_COMPLETE};
use vm_memory::ByteValued;

/// How long an imported name and its attributes stay valid in the guest.
/// Mutations update the guest image directly; this horizon only covers a
/// dentry that outlives the guest-side image.
pub const GUEST_META_VALID: Duration = Duration::from_secs(24 * 60 * 60);

// Keep backing cookies stable when one-shot synthetic names disappear.
// Bit 62 leaves the merged cookie representable as a guest signed loff_t.
const BACKING_COOKIE_BASE: u64 = 1 << 62;

/// One directory page, ready to place after the FUSE output header.
#[derive(Debug, PartialEq, Eq)]
pub struct DirectoryPage {
    /// Cookie the guest passes to fetch the following page.
    pub next_offset: u64,
    /// The reply includes every remaining entry. Absence is proven only then.
    pub complete: bool,
    /// `fuse_direntplus` records, 8-byte aligned, in directory order.
    pub records: Vec<u8>,
    // One acquired lookup per accepted backing record. This is not wire data:
    // successful reply publication transfers these references to the guest.
    lookup_inodes: Vec<u64>,
}

/// Cancel an unpublished page's backing lookups, exactly once.
///
/// Synthetic entries do not acquire backing lookups. A decoded page owns none;
/// wire node IDs must never become authority to release host references.
pub(super) fn cancel_directory_page<F: FileSystem>(fs: &F, ctx: Context, page: &mut DirectoryPage) {
    for inode in page.lookup_inodes.drain(..) {
        fs.forget(ctx, inode.into(), 1);
    }
}

struct PendingLookups<'a, F: FileSystem> {
    fs: &'a F,
    ctx: Context,
    inodes: Vec<u64>,
}

impl<F: FileSystem> Drop for PendingLookups<'_, F> {
    fn drop(&mut self) {
        for inode in self.inodes.drain(..) {
            self.fs.forget(self.ctx, inode.into(), 1);
        }
    }
}

/// Read one merged directory page from `inode` at `offset`.
///
/// `max_bytes` is the reply budget including [`ReadIndexOut`]. The filesystem
/// opens the directory for the duration of the read and closes it before
/// returning, including when reading entries fails. `readdirplus` publishes
/// the lookup counts the guest cache consumes. A successful page still owns
/// its unpublished lookups: the caller must cancel it if reply publication
/// fails. A completely published reply transfers the references to the guest.
pub fn read_directory_page<F: FileSystem>(
    fs: &F,
    ctx: Context,
    inode: F::Inode,
    offset: u64,
    max_bytes: u32,
) -> io::Result<DirectoryPage> {
    let header = size_of::<ReadIndexOut>();
    if (max_bytes as usize) < header {
        return Err(linux_error(io::Error::from_raw_os_error(libc::EINVAL)));
    }
    if !fs.supports_directory_image() {
        return Err(linux_error(io::Error::from_raw_os_error(libc::ENOSYS)));
    }
    let budget = (max_bytes as usize) - header;
    let inode = inode.into();
    let (handle, _) = fs.opendir(ctx, inode.into(), 0)?;
    let handle = handle.map(Into::into).unwrap_or(0);
    let page = collect_page(fs, ctx, inode, handle, offset, budget);
    let released = fs.releasedir(ctx, inode.into(), 0, handle.into());
    match (page, released) {
        (Err(error), _) => Err(error),
        (Ok(mut page), Err(error)) => {
            cancel_directory_page(fs, ctx, &mut page);
            Err(error)
        }
        (Ok(page), Ok(())) => Ok(page),
    }
}

fn collect_page<F: FileSystem>(
    fs: &F,
    ctx: Context,
    inode: u64,
    handle: u64,
    offset: u64,
    budget: usize,
) -> io::Result<DirectoryPage> {
    let mut synthetics = fs.synthetic_entries(inode.into());
    synthetics.sort_by_key(|(_, entry)| entry.inode);
    let synthetic_names: Vec<Vec<u8>> = synthetics.iter().map(|(name, _)| name.clone()).collect();
    let mut records = Vec::new();
    let mut stopped = false;
    let mut last_offset = offset;
    let mut wrote = false;
    let mut lookups = PendingLookups {
        fs,
        ctx,
        inodes: Vec::new(),
    };

    if offset < BACKING_COOKIE_BASE {
        for (name, attrs) in synthetics {
            if attrs.inode <= offset {
                continue;
            }
            if attrs.inode >= BACKING_COOKIE_BASE {
                return Err(linux_error(io::Error::from_raw_os_error(libc::EOVERFLOW)));
            }
            let entry = DirEntry {
                ino: attrs.attr.st_ino,
                offset: attrs.inode,
                type_: dir_type(attrs.attr.st_mode),
                name: &name,
            };
            match push_record(&mut records, budget, &entry, attrs, false) {
                Ok(0) => {
                    stopped = true;
                    break;
                }
                Ok(_) => {
                    wrote = true;
                    last_offset = entry.offset;
                }
                Err(error) => return Err(error),
            }
        }
    }

    if !stopped {
        let inner_offset = offset.saturating_sub(BACKING_COOKIE_BASE);
        fs.readdirplus(
            ctx,
            inode.into(),
            handle.into(),
            budget as u32,
            inner_offset,
            |mut entry, attrs| {
                if stopped {
                    return Ok(0);
                }
                if synthetic_names
                    .iter()
                    .any(|name| name.as_slice() == entry.name)
                {
                    // readdirplus acquired the current lookup before calling
                    // us, but the synthetic entry replaces it on the wire.
                    // Report progress so enumeration continues; returning zero
                    // would make the filesystem cancel the current entry too.
                    if attrs.inode != 0 {
                        fs.forget(ctx, attrs.inode.into(), 1);
                    }
                    return Ok(1);
                }
                if entry.offset >= BACKING_COOKIE_BASE {
                    return Err(linux_error(io::Error::from_raw_os_error(libc::EOVERFLOW)));
                }
                entry.offset += BACKING_COOKIE_BASE;
                let inode = attrs.inode;
                match push_record(&mut records, budget, &entry, attrs, true) {
                    Ok(0) => {
                        stopped = true;
                        Ok(0)
                    }
                    Ok(written) => {
                        if inode != 0 {
                            lookups.inodes.push(inode);
                        }
                        wrote = true;
                        last_offset = entry.offset;
                        Ok(written)
                    }
                    Err(error) => Err(error),
                }
            },
        )?;
    }
    if stopped && !wrote {
        return Err(linux_error(io::Error::from_raw_os_error(libc::EOVERFLOW)));
    }
    Ok(DirectoryPage {
        next_offset: last_offset,
        complete: !stopped,
        records,
        lookup_inodes: std::mem::take(&mut lookups.inodes),
    })
}

fn dir_type(mode: libc::mode_t) -> u32 {
    match mode & libc::S_IFMT {
        libc::S_IFDIR => libc::DT_DIR as u32,
        libc::S_IFLNK => libc::DT_LNK as u32,
        libc::S_IFREG => libc::DT_REG as u32,
        _ => libc::DT_UNKNOWN as u32,
    }
}

fn push_record(
    records: &mut Vec<u8>,
    budget: usize,
    entry: &DirEntry<'_>,
    attrs: Entry,
    cache_metadata: bool,
) -> io::Result<usize> {
    if entry.name.is_empty()
        || entry.name.contains(&0)
        || entry.name.contains(&b'/')
        || entry.name.len() > u32::MAX as usize
        || entry.offset == 0
    {
        return Err(linux_error(io::Error::from_raw_os_error(libc::EIO)));
    }
    let dirent_len = size_of::<Dirent>()
        .checked_add(entry.name.len())
        .ok_or_else(|| linux_error(io::Error::from_raw_os_error(libc::EOVERFLOW)))?;
    let padded = dirent_len
        .checked_add(7)
        .map(|len| len & !7)
        .ok_or_else(|| linux_error(io::Error::from_raw_os_error(libc::EOVERFLOW)))?;
    let total = padded
        .checked_add(size_of::<EntryOut>())
        .ok_or_else(|| linux_error(io::Error::from_raw_os_error(libc::EOVERFLOW)))?;
    if records.len().saturating_add(total) > budget {
        return Ok(0);
    }
    let mut out = EntryOut::from(attrs);
    if cache_metadata {
        out.entry_valid = GUEST_META_VALID.as_secs();
        out.entry_valid_nsec = 0;
        out.attr_valid = GUEST_META_VALID.as_secs();
        out.attr_valid_nsec = 0;
    }
    records.extend_from_slice(out.as_slice());
    let dirent = Dirent {
        ino: entry.ino,
        off: entry.offset,
        namelen: entry.name.len() as u32,
        type_: entry.type_,
    };
    records.extend_from_slice(dirent.as_slice());
    records.extend_from_slice(entry.name);
    records.resize(records.len() + (padded - dirent_len), 0);
    Ok(total)
}

/// Encode the page the guest receives after the FUSE output header.
pub fn encode_page(page: &DirectoryPage) -> Vec<u8> {
    let header = ReadIndexOut {
        next_offset: page.next_offset,
        flags: if page.complete { READINDEX_COMPLETE } else { 0 },
        padding: 0,
    };
    let mut bytes = Vec::with_capacity(size_of::<ReadIndexOut>() + page.records.len());
    bytes.extend_from_slice(header.as_slice());
    bytes.extend_from_slice(&page.records);
    bytes
}

/// Split a guest payload into its header and directory records.
pub fn decode_page(bytes: &[u8]) -> io::Result<DirectoryPage> {
    let header_len = size_of::<ReadIndexOut>();
    if bytes.len() < header_len {
        return Err(linux_error(io::Error::from_raw_os_error(libc::EINVAL)));
    }
    let header = ReadIndexOut::read_bytes(&bytes[..header_len])
        .ok_or_else(|| linux_error(io::Error::from_raw_os_error(libc::EINVAL)))?;
    Ok(DirectoryPage {
        next_offset: header.next_offset,
        complete: (header.flags & READINDEX_COMPLETE) != 0,
        records: bytes[header_len..].to_vec(),
        lookup_inodes: Vec::new(),
    })
}

/// Names carried by a page, in directory order.
pub fn record_names(records: &[u8]) -> io::Result<Vec<Vec<u8>>> {
    let mut names = Vec::new();
    let mut rest = records;
    while !rest.is_empty() {
        let entry_len = size_of::<EntryOut>();
        let dirent_len = size_of::<Dirent>();
        if rest.len() < entry_len + dirent_len {
            return Err(linux_error(io::Error::from_raw_os_error(libc::EIO)));
        }
        let dirent = Dirent::read_bytes(&rest[entry_len..entry_len + dirent_len])
            .ok_or_else(|| linux_error(io::Error::from_raw_os_error(libc::EIO)))?;
        let name_at = entry_len + dirent_len;
        let name_len = dirent.namelen as usize;
        let padded = (name_at + name_len)
            .checked_add(7)
            .map(|len| len & !7)
            .ok_or_else(|| linux_error(io::Error::from_raw_os_error(libc::EIO)))?;
        if name_len == 0 || padded > rest.len() {
            return Err(linux_error(io::Error::from_raw_os_error(libc::EIO)));
        }
        names.push(rest[name_at..name_at + name_len].to_vec());
        rest = &rest[padded..];
    }
    Ok(names)
}

trait ReadBytes: Copy {
    fn read_bytes(bytes: &[u8]) -> Option<Self>;
}

impl<T: ByteValued> ReadBytes for T {
    fn read_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < size_of::<T>() {
            return None;
        }
        // SAFETY: `ByteValued` types are plain data and may be read from any
        // alignment. Only the leading `size_of::<T>()` bytes are used.
        Some(unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<T>()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeFs {
        names: Vec<&'static [u8]>,
        synthetics: Vec<&'static [u8]>,
        opened: AtomicUsize,
        closed: AtomicUsize,
        fail_release: Mutex<bool>,
        fail_read_after: Option<usize>,
        lookup_refs: Mutex<BTreeMap<u64, u64>>,
        forgotten: Mutex<Vec<(u64, u64)>>,
    }

    impl FileSystem for FakeFs {
        type Inode = u64;
        type Handle = u64;

        fn supports_directory_image(&self) -> bool {
            true
        }

        fn forget(&self, _ctx: Context, inode: Self::Inode, count: u64) {
            let mut refs = self.lookup_refs.lock().unwrap();
            let remaining = refs
                .get_mut(&inode)
                .expect("forget without acquired lookup");
            assert!(*remaining >= count, "lookup reference released twice");
            *remaining -= count;
            if *remaining == 0 {
                refs.remove(&inode);
            }
            self.forgotten.lock().unwrap().push((inode, count));
        }

        fn opendir(
            &self,
            _ctx: Context,
            _inode: Self::Inode,
            _flags: u32,
        ) -> io::Result<(Option<Self::Handle>, super::super::fuse::OpenOptions)> {
            self.opened.fetch_add(1, Ordering::Relaxed);
            Ok((Some(7), super::super::fuse::OpenOptions::empty()))
        }

        fn releasedir(
            &self,
            _ctx: Context,
            _inode: Self::Inode,
            _flags: u32,
            handle: Self::Handle,
        ) -> io::Result<()> {
            assert_eq!(handle, 7);
            self.closed.fetch_add(1, Ordering::Relaxed);
            if *self.fail_release.lock().unwrap() {
                Err(io::Error::from_raw_os_error(libc::EIO))
            } else {
                Ok(())
            }
        }

        fn readdirplus<C>(
            &self,
            ctx: Context,
            _inode: Self::Inode,
            _handle: Self::Handle,
            _size: u32,
            offset: u64,
            mut add_entry: C,
        ) -> io::Result<()>
        where
            C: FnMut(DirEntry, Entry) -> io::Result<usize>,
        {
            for (index, name) in self.names.iter().enumerate().skip(offset as usize) {
                if self.fail_read_after == Some(index) {
                    return Err(io::Error::from_raw_os_error(libc::EIO));
                }
                let mut attr: super::super::super::bindings::stat64 = unsafe { std::mem::zeroed() };
                attr.st_ino = (index as u64) + 10;
                attr.st_mode = (libc::S_IFREG | 0o644) as _;
                let inode = (index as u64) + 10;
                *self.lookup_refs.lock().unwrap().entry(inode).or_default() += 1;
                let written = add_entry(
                    DirEntry {
                        ino: attr.st_ino,
                        offset: (index as u64) + 1,
                        type_: u32::from(libc::DT_REG),
                        name,
                    },
                    Entry {
                        inode: (index as u64) + 10,
                        generation: 0,
                        attr,
                        attr_flags: 0,
                        attr_timeout: Duration::from_secs(1),
                        entry_timeout: Duration::from_secs(1),
                    },
                );
                match written {
                    Ok(0) => {
                        self.forget(ctx, inode, 1);
                        break;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        self.forget(ctx, inode, 1);
                        return Err(error);
                    }
                }
            }
            Ok(())
        }

        fn synthetic_entries(&self, _inode: Self::Inode) -> Vec<(Vec<u8>, Entry)> {
            self.synthetics
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    let mut attr: super::super::super::bindings::stat64 =
                        unsafe { std::mem::zeroed() };
                    attr.st_ino = 100 + index as u64;
                    attr.st_mode = (libc::S_IFREG | 0o755) as _;
                    attr.st_size = 4;
                    (
                        name.to_vec(),
                        Entry {
                            inode: attr.st_ino,
                            generation: 0,
                            attr,
                            attr_flags: 0,
                            attr_timeout: Duration::from_secs(1),
                            entry_timeout: Duration::from_secs(1),
                        },
                    )
                })
                .collect()
        }
    }

    fn context() -> Context {
        Context {
            uid: 0,
            gid: 0,
            pid: 1,
        }
    }

    #[test]
    fn pages_cover_the_directory_and_only_the_last_is_complete() {
        let fs = FakeFs {
            names: vec![b"a", b"b", b"c"],
            synthetics: Vec::new(),
            opened: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
            fail_release: Mutex::new(false),
            ..Default::default()
        };
        let first = read_directory_page(&fs, context(), 1, 0, 256).unwrap();
        assert!(!first.complete);
        assert_eq!(record_names(&first.records).unwrap(), vec![b"a".to_vec()]);
        let encoded = encode_page(&first);
        let decoded = decode_page(&encoded).unwrap();
        assert_eq!(decoded.next_offset, first.next_offset);
        assert_eq!(decoded.complete, first.complete);
        assert_eq!(decoded.records, first.records);
        assert!(decoded.lookup_inodes.is_empty());

        let mut offset = first.next_offset;
        let mut seen = vec![b"a".to_vec()];
        let mut pages = 1;
        loop {
            let page = read_directory_page(&fs, context(), 1, offset, 256).unwrap();
            seen.extend(record_names(&page.records).unwrap());
            pages += 1;
            if page.complete {
                break;
            }
            offset = page.next_offset;
            assert!(pages < 8);
        }
        assert_eq!(seen, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
        assert_eq!(
            fs.opened.load(Ordering::Relaxed),
            fs.closed.load(Ordering::Relaxed)
        );
    }

    #[test]
    fn a_mib_budget_returns_one_complete_page() {
        let names: Vec<&[u8]> = (0..80).map(|_| b"file".as_slice()).collect();
        let fs = FakeFs {
            names: names.clone(),
            synthetics: Vec::new(),
            opened: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
            fail_release: Mutex::new(false),
            ..Default::default()
        };
        let small = read_directory_page(&fs, context(), 1, 0, 4096).unwrap();
        assert!(!small.complete);
        let page = read_directory_page(&fs, context(), 1, 0, 1 << 20).unwrap();
        assert!(page.complete);
        assert_eq!(record_names(&page.records).unwrap().len(), names.len());
        assert_eq!(fs.opened.load(Ordering::Relaxed), 2);
        assert_eq!(fs.closed.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn mib_replies_cross_guest_pages_and_resume_at_the_boundary() {
        use super::super::super::descriptor_utils::{Reader, Writer};
        use super::super::super::queue::DescriptorChain;
        use super::super::fuse::{
            InHeader, InitInCompat, InitInExt, InitOut, Opcode, OutHeader, ReadIn,
        };
        use super::super::server::Server;
        use std::sync::{atomic::AtomicI32, Arc};
        use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};

        let fs = FakeFs {
            names: vec![b"file"; 9000],
            synthetics: Vec::new(),
            opened: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
            fail_release: Mutex::new(false),
            ..Default::default()
        };
        let server = Server::new(fs, true);
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 2 << 20)]).unwrap();
        let request = |opcode: Opcode, payload: &[u8]| {
            mem.write_slice(payload, GuestAddress(0x10000)).unwrap();
            // Match virtio-fs: separate reply header and 256 writable 4 KiB pages.
            let descriptors = std::iter::once((0x10000u64, payload.len() as u32, 1u16))
                .chain(std::iter::once((0x20000, size_of::<OutHeader>() as u32, 3)))
                .chain((0..256).map(|i| (0x21000 + i * 4096, 4096, 3)));
            for (i, (address, len, flags)) in descriptors.enumerate() {
                let base = (i * 16) as u64;
                mem.write_obj(address.to_le(), GuestAddress(base)).unwrap();
                mem.write_obj(len.to_le(), GuestAddress(base + 8)).unwrap();
                mem.write_obj(
                    (if i == 257 { 2u16 } else { flags }).to_le(),
                    GuestAddress(base + 12),
                )
                .unwrap();
                mem.write_obj(((i + 1) as u16).to_le(), GuestAddress(base + 14))
                    .unwrap();
            }
            let chain = DescriptorChain::checked_new(&mem, GuestAddress(0), 1024, 0).unwrap();
            let count = server
                .handle_message(
                    (
                        InHeader {
                            opcode: opcode as u32,
                            nodeid: 1,
                            unique: 7,
                            ..Default::default()
                        },
                        Reader::new(&mem, chain.clone()).unwrap(),
                    ),
                    Writer::new(&mem, chain).unwrap(),
                    false,
                    &None,
                    &Arc::new(AtomicI32::new(0)),
                    #[cfg(target_os = "macos")]
                    &None,
                )
                .unwrap();
            let header: OutHeader = mem.read_obj(GuestAddress(0x20000)).unwrap();
            assert_eq!(header.error, 0);
            assert_eq!(header.unique, 7);
            assert_eq!(header.len as usize, count);
            let mut bytes = vec![0; count - size_of::<OutHeader>()];
            mem.read_slice(&mut bytes, GuestAddress(0x21000)).unwrap();
            bytes
        };
        let flags = super::super::fuse::FsOptions::INIT_EXT
            | super::super::fuse::FsOptions::MAX_PAGES
            | super::super::fuse::FsOptions::PVISOR_META;
        let mut init = InitInCompat {
            major: 7,
            minor: 42,
            flags: flags.bits() as u32,
            ..Default::default()
        }
        .as_slice()
        .to_vec();
        init.extend_from_slice(
            InitInExt {
                flags2: (flags.bits() >> 32) as u32,
                ..Default::default()
            }
            .as_slice(),
        );
        let initialized = request(Opcode::Init, &init);
        let out = InitOut::read_bytes(&initialized).unwrap();
        assert_eq!(out.max_pages, 256);
        let mut offset = 0;
        let mut names = Vec::new();
        for page_number in 0..2 {
            let bytes = request(
                Opcode::ReadIndex,
                ReadIn {
                    offset,
                    size: 1 << 20,
                    ..Default::default()
                }
                .as_slice(),
            );
            assert!(bytes.len() <= 1 << 20);
            let page = decode_page(&bytes).unwrap();
            if page_number == 0 {
                assert!(bytes.len() > (1 << 20) - 256);
                assert!(!page.complete);
            } else {
                assert!(page.complete);
            }
            assert!(page.next_offset > offset);
            offset = page.next_offset;
            names.extend(record_names(&page.records).unwrap());
        }
        assert_eq!(names, vec![b"file".to_vec(); 9000]);
    }

    #[test]
    fn synthetic_names_lead_the_page_and_survive_a_short_read() {
        let fs = FakeFs {
            names: vec![b"real", b"init.krun"],
            synthetics: vec![b".pvisor-guest.json", b"init.krun"],
            opened: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
            fail_release: Mutex::new(false),
            ..Default::default()
        };
        let page = read_directory_page(&fs, context(), 1, 0, 1 << 20).unwrap();
        assert!(page.complete);
        assert_eq!(
            record_names(&page.records).unwrap(),
            vec![
                b".pvisor-guest.json".to_vec(),
                b"init.krun".to_vec(),
                b"real".to_vec()
            ]
        );

        let first_len = record_span(&page.records, 1);
        let short = read_directory_page(
            &fs,
            context(),
            1,
            0,
            (size_of::<ReadIndexOut>() + first_len) as u32,
        )
        .unwrap();
        assert!(!short.complete);
        assert_eq!(
            record_names(&short.records).unwrap(),
            vec![b".pvisor-guest.json".to_vec()]
        );
        let rest = read_directory_page(&fs, context(), 1, short.next_offset, 1 << 20).unwrap();
        assert!(rest.complete);
        assert_eq!(
            record_names(&rest.records).unwrap(),
            vec![b"init.krun".to_vec(), b"real".to_vec()]
        );
    }

    #[test]
    fn imported_one_shot_entries_keep_zero_validity_and_remain_lookupable() {
        use super::super::augment_fs::AugmentFs;
        use super::super::inode_alloc::InodeAllocator;
        use super::super::virtual_entry::{VirtualDirEntry, VirtualEntry, VirtualEntryContent};
        use std::ffi::CString;
        use std::sync::Arc;

        let fs = AugmentFs::new(
            FakeFs {
                names: vec![b"real", b"next"],
                synthetics: Vec::new(),
                opened: AtomicUsize::new(0),
                closed: AtomicUsize::new(0),
                fail_release: Mutex::new(false),
                ..Default::default()
            },
            &InodeAllocator::new(),
            [".pvisor-guest.json", "init.krun"]
                .map(|name| VirtualDirEntry {
                    name: CString::new(name).unwrap(),
                    entry: VirtualEntry {
                        mode: 0o755,
                        one_shot: true,
                        content: VirtualEntryContent::File {
                            data: Arc::from(&b"init"[..]),
                        },
                    },
                })
                .to_vec(),
        );
        let page = read_directory_page(&fs, context(), 1, 0, 1 << 20).unwrap();
        assert!(page.complete);
        assert_eq!(
            record_names(&page.records).unwrap(),
            [
                b".pvisor-guest.json".to_vec(),
                b"init.krun".to_vec(),
                b"real".to_vec(),
                b"next".to_vec()
            ]
        );
        let partial = read_directory_page(
            &fs,
            context(),
            1,
            0,
            (size_of::<ReadIndexOut>() + record_span(&page.records, 3)) as u32,
        )
        .unwrap();
        assert!(!partial.complete);
        let mut start = 0;
        for name in [c".pvisor-guest.json", c"init.krun"] {
            let entry =
                EntryOut::read_bytes(&page.records[start..start + size_of::<EntryOut>()]).unwrap();
            assert_eq!(
                (
                    entry.entry_valid,
                    entry.entry_valid_nsec,
                    entry.attr_valid,
                    entry.attr_valid_nsec
                ),
                (0, 0, 0, 0)
            );
            assert_eq!(entry.nodeid, fs.lookup(context(), 1, name).unwrap().inode);
            assert!(fs.lookup(context(), 1, name).is_err());
            start += record_span(&page.records[start..], 1);
        }
        assert!(fs.synthetic_entries(1).is_empty());
        let rest = read_directory_page(&fs, context(), 1, partial.next_offset, 1 << 20).unwrap();
        assert!(rest.complete);
        assert_eq!(record_names(&rest.records).unwrap(), [b"next".to_vec()]);
    }

    fn record_span(records: &[u8], count: usize) -> usize {
        let mut rest = records;
        let mut end = 0;
        for _ in 0..count {
            let entry_len = size_of::<EntryOut>();
            let dirent_len = size_of::<Dirent>();
            let dirent = Dirent::read_bytes(&rest[entry_len..entry_len + dirent_len]).unwrap();
            let padded = (entry_len + dirent_len + dirent.namelen as usize + 7) & !7;
            rest = &rest[padded..];
            end += padded;
        }
        end
    }

    #[test]
    fn an_empty_directory_is_complete_without_records() {
        let fs = FakeFs {
            names: Vec::new(),
            synthetics: Vec::new(),
            opened: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
            fail_release: Mutex::new(false),
            ..Default::default()
        };
        let page = read_directory_page(&fs, context(), 1, 0, 256).unwrap();
        assert!(page.complete);
        assert!(page.records.is_empty());
        assert_eq!(page.next_offset, 0);
    }

    #[test]
    fn mutable_backing_rejects_import_before_enumeration() {
        struct Mutable;
        impl FileSystem for Mutable {
            type Inode = u64;
            type Handle = u64;
            fn opendir(
                &self,
                _: Context,
                _: u64,
                _: u32,
            ) -> io::Result<(Option<u64>, super::super::fuse::OpenOptions)> {
                panic!("mutable backing must not be enumerated for a permanent image");
            }
        }
        let error = read_directory_page(&Mutable, context(), 1, 0, 1 << 20).unwrap_err();
        assert_eq!(
            error.raw_os_error(),
            Some(super::super::bindings::LINUX_ENOSYS)
        );
    }

    #[test]
    fn release_runs_when_reading_entries_fails() {
        struct Broken;
        impl FileSystem for Broken {
            type Inode = u64;
            type Handle = u64;
            fn supports_directory_image(&self) -> bool {
                true
            }
            fn opendir(
                &self,
                _ctx: Context,
                _inode: Self::Inode,
                _flags: u32,
            ) -> io::Result<(Option<Self::Handle>, super::super::fuse::OpenOptions)> {
                Ok((Some(1), super::super::fuse::OpenOptions::empty()))
            }
            fn releasedir(
                &self,
                _ctx: Context,
                _inode: Self::Inode,
                _flags: u32,
                _handle: Self::Handle,
            ) -> io::Result<()> {
                Err(io::Error::from_raw_os_error(libc::EBUSY))
            }
            fn readdirplus<C>(
                &self,
                _ctx: Context,
                _inode: Self::Inode,
                _handle: Self::Handle,
                _size: u32,
                _offset: u64,
                _add_entry: C,
            ) -> io::Result<()>
            where
                C: FnMut(DirEntry, Entry) -> io::Result<usize>,
            {
                Err(io::Error::from_raw_os_error(libc::EIO))
            }
        }
        let error = read_directory_page(&Broken, context(), 1, 0, 256).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EIO));
    }

    #[test]
    fn imported_attributes_outlive_the_filesystem_timeout() {
        let fs = FakeFs {
            names: vec![b"stat"],
            synthetics: Vec::new(),
            opened: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
            fail_release: Mutex::new(false),
            ..Default::default()
        };
        let page = read_directory_page(&fs, context(), 1, 0, 4096).unwrap();
        let entry = EntryOut::read_bytes(&page.records[..size_of::<EntryOut>()]).unwrap();
        assert_eq!(entry.entry_valid, GUEST_META_VALID.as_secs());
        assert_eq!(entry.attr_valid, GUEST_META_VALID.as_secs());
        assert_eq!(entry.nodeid, 10);
        let start = size_of::<EntryOut>() - size_of::<Attr>();
        let attr = Attr::read_bytes(&page.records[start..size_of::<EntryOut>()]).unwrap();
        assert_eq!(u64::from(attr.mode), u64::from(libc::S_IFREG) | 0o644);
    }

    #[test]
    fn shadowed_backing_lookup_is_cancelled_without_touching_synthetics() {
        let fs = FakeFs {
            names: vec![b"hidden", b"visible"],
            synthetics: vec![b"hidden"],
            ..Default::default()
        };
        let mut page = read_directory_page(&fs, context(), 1, 0, 4096).unwrap();
        assert_eq!(
            record_names(&page.records).unwrap(),
            [b"hidden".to_vec(), b"visible".to_vec()]
        );
        assert_eq!(*fs.forgotten.lock().unwrap(), [(10, 1)]);
        assert_eq!(*fs.lookup_refs.lock().unwrap(), BTreeMap::from([(11, 1)]));
        assert_eq!(page.lookup_inodes, [11]);
        cancel_directory_page(&fs, context(), &mut page);
        cancel_directory_page(&fs, context(), &mut page);
        assert!(fs.lookup_refs.lock().unwrap().is_empty());
        assert_eq!(*fs.forgotten.lock().unwrap(), [(10, 1), (11, 1)]);
    }

    #[test]
    fn failed_enumeration_rolls_back_previously_accepted_lookups() {
        let fs = FakeFs {
            names: vec![b"a", b"b", b"c"],
            fail_read_after: Some(2),
            ..Default::default()
        };
        let error = read_directory_page(&fs, context(), 1, 0, 4096).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EIO));
        assert!(fs.lookup_refs.lock().unwrap().is_empty());
        assert_eq!(*fs.forgotten.lock().unwrap(), [(10, 1), (11, 1)]);
        assert_eq!(fs.closed.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn malformed_later_record_cancels_current_and_previous_lookups_once() {
        let fs = FakeFs {
            names: vec![b"accepted", b"invalid/name"],
            ..Default::default()
        };
        let error = read_directory_page(&fs, context(), 1, 0, 4096).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EIO));
        assert!(fs.lookup_refs.lock().unwrap().is_empty());
        // The filesystem cancels the rejected current callback; the page guard
        // separately cancels its earlier accepted entry.
        assert_eq!(*fs.forgotten.lock().unwrap(), [(11, 1), (10, 1)]);
        assert_eq!(fs.closed.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn failed_directory_close_cancels_every_unpublished_backing_lookup() {
        let fs = FakeFs {
            names: vec![b"a", b"b"],
            fail_release: Mutex::new(true),
            ..Default::default()
        };
        let error = read_directory_page(&fs, context(), 1, 0, 4096).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EIO));
        assert!(fs.lookup_refs.lock().unwrap().is_empty());
        assert_eq!(*fs.forgotten.lock().unwrap(), [(10, 1), (11, 1)]);
        assert_eq!(fs.closed.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn page_budget_retains_only_emitted_refs_and_decode_owns_none() {
        let fs = FakeFs {
            names: vec![b"a", b"b", b"c"],
            ..Default::default()
        };
        let mut page = read_directory_page(&fs, context(), 1, 0, 256).unwrap();
        assert!(!page.complete);
        assert_eq!(record_names(&page.records).unwrap(), [b"a".to_vec()]);
        assert_eq!(page.lookup_inodes, [10]);
        assert_eq!(*fs.forgotten.lock().unwrap(), [(11, 1)]);
        let mut decoded = decode_page(&encode_page(&page)).unwrap();
        cancel_directory_page(&fs, context(), &mut decoded);
        assert_eq!(*fs.lookup_refs.lock().unwrap(), BTreeMap::from([(10, 1)]));
        cancel_directory_page(&fs, context(), &mut page);
        assert!(fs.lookup_refs.lock().unwrap().is_empty());
        assert_eq!(*fs.forgotten.lock().unwrap(), [(11, 1), (10, 1)]);
    }
}
