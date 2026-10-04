//! Reopen Linux O_PATH inodes and ordinary handles against an owned tree copy.
use super::super::super::snapshot::{
    self, DirectoryEntrySnapshot, FileIdentity, FsSnapshot, HandleSnapshot, InodeSnapshot,
    PassthroughSnapshot,
};
use super::*;
use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
use std::path::Path;

fn supported(fs: &PassthroughFs) -> io::Result<()> {
    if fs.writeback.load(Ordering::Relaxed)
        || fs.cfg.export_table.is_some()
        || fs.cfg.proc_sfd_rawfd.is_some()
    {
        return Err(snapshot::unsupported(
            "filesystem writeback or exported descriptors",
        ));
    }
    Ok(())
}

fn directory_entries(file: &File) -> io::Result<Vec<DirectoryEntrySnapshot>> {
    let fd = file.as_raw_fd();
    let previous = unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) };
    if previous < 0 || unsafe { libc::lseek(fd, 0, libc::SEEK_SET) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let mut entries = Vec::new();
        let mut bytes = vec![0u8; 65536];
        loop {
            let n =
                unsafe { libc::syscall(libc::SYS_getdents64, fd, bytes.as_mut_ptr(), bytes.len()) };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            if n == 0 {
                return Ok(entries);
            }
            let mut offset = 0;
            while offset < n as usize {
                let b = &bytes[offset..n as usize];
                if b.len() < 20 {
                    return Err(snapshot::invalid("short directory record"));
                }
                let len = u16::from_ne_bytes(b[16..18].try_into().unwrap()) as usize;
                if len < 20 || len > b.len() {
                    return Err(snapshot::invalid("invalid directory record"));
                }
                let name = &b[19..len];
                let end = name
                    .iter()
                    .position(|c| *c == 0)
                    .ok_or_else(|| snapshot::invalid("unterminated directory record"))?;
                entries.push(DirectoryEntrySnapshot {
                    ino: u64::from_ne_bytes(b[..8].try_into().unwrap()),
                    offset: u64::from_ne_bytes(b[8..16].try_into().unwrap()),
                    type_: b[18],
                    name: name[..end].to_vec(),
                });
                offset += len;
            }
        }
    })();
    if unsafe { libc::lseek(fd, previous, libc::SEEK_SET) } < 0 {
        return Err(io::Error::last_os_error());
    }
    result
}

pub(super) fn capture(fs: &PassthroughFs) -> io::Result<FsSnapshot> {
    supported(fs)?;
    let root = Path::new(&fs.cfg.root_dir).canonicalize()?;
    let mut inodes = Vec::new();
    for data in fs.inodes.read().unwrap().values() {
        let path = std::fs::read_link(format!("/proc/self/fd/{}", data.file.as_raw_fd()))?;
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| snapshot::invalid("inode outside filesystem root"))?;
        let identity = FileIdentity::read(&data.file)?;
        if identity.nlink == 0 {
            let open_handles = fs
                .handles
                .read()
                .unwrap()
                .values()
                .any(|handle| handle.inode == data.inode);
            return Err(snapshot::unsupported(&format!(
                "unlinked inode {} (application handle: {open_handles}) at {}",
                data.inode,
                path.display()
            )));
        }
        let pin = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(snapshot::pin_flags())
            .open(&path)?;
        if identity != FileIdentity::read(&pin)? || identity.nlink == 0 {
            return Err(snapshot::unsupported("unlinked or replaced inode"));
        }
        let regular = identity.mode & libc::S_IFMT == libc::S_IFREG;
        let link = identity.mode & libc::S_IFMT == libc::S_IFLNK;
        inodes.push(InodeSnapshot {
            inode: data.inode,
            refs: data.refcount.load(Ordering::Relaxed),
            path: Some(relative.as_os_str().as_bytes().to_vec()),
            identity,
            digest: if regular {
                Some(snapshot::file_digest(
                    &std::fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(&path)?,
                )?)
            } else {
                None
            },
            link_target: if link {
                Some(std::fs::read_link(&path)?.as_os_str().as_bytes().to_vec())
            } else {
                None
            },
        });
    }
    let mut handles = Vec::new();
    for (&handle, data) in fs.handles.read().unwrap().iter() {
        if data.exported.load(Ordering::Relaxed) {
            return Err(snapshot::unsupported("exported handle"));
        }
        #[expect(clippy::readonly_write_lock, reason = "directory_entries changes and restores the shared descriptor cursor through OS calls")]
        let file = data.file.write().unwrap();
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        let offset = unsafe { libc::lseek(file.as_raw_fd(), 0, libc::SEEK_CUR) };
        if flags < 0 || offset < 0 {
            return Err(io::Error::last_os_error());
        }
        let entries = if file.metadata()?.is_dir() {
            Some(match &data.directory_snapshot {
                Some(e) => e.clone(),
                None => directory_entries(&file)?,
            })
        } else {
            None
        };
        handles.push(HandleSnapshot {
            handle,
            inode: data.inode,
            flags,
            offset: offset as u64,
            entries: Vec::new(),
            directory_ready: false,
            directory_entries: entries,
        });
    }
    Ok(FsSnapshot::Passthrough(PassthroughSnapshot {
        root: root.as_os_str().as_bytes().to_vec(),
        semantics: fs.cfg.semantics as u8,
        entry_timeout: fs.cfg.entry_timeout,
        attr_timeout: fs.cfg.attr_timeout,
        cache_policy: fs.cfg.cache_policy.clone(),
        xattr: fs.cfg.xattr,
        inodes,
        handles,
        next_handle: fs.next_handle.load(Ordering::Relaxed),
        submounts: fs.announce_submounts.load(Ordering::Relaxed),
    }))
}

pub(super) fn restore(fs: &PassthroughFs, state: &FsSnapshot) -> io::Result<()> {
    supported(fs)?;
    let FsSnapshot::Passthrough(state) = state else {
        return Err(snapshot::invalid("filesystem type mismatch"));
    };
    let root = Path::new(&fs.cfg.root_dir).canonicalize()?;
    if root.as_os_str().as_bytes() != state.root
        || fs.cfg.semantics as u8 != state.semantics
        || fs.cfg.entry_timeout != state.entry_timeout
        || fs.cfg.attr_timeout != state.attr_timeout
        || fs.cfg.cache_policy != state.cache_policy
        || fs.cfg.xattr != state.xattr
    {
        return Err(snapshot::invalid("filesystem binding mismatch"));
    }
    let mut inodes = MultikeyBTreeMap::new();
    let mut paths = BTreeMap::new();
    for saved in &state.inodes {
        if saved.inode == 0 || saved.refs == 0 || inodes.get(&saved.inode).is_some() {
            return Err(snapshot::invalid("invalid inode"));
        }
        let relative = saved
            .path
            .as_deref()
            .ok_or_else(|| snapshot::invalid("missing inode path"))?;
        if (saved.inode == fuse::ROOT_ID) != relative.is_empty() {
            return Err(snapshot::invalid("invalid root inode"));
        }
        let path = snapshot::relative_path(&root, relative)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(snapshot::pin_flags())
            .open(&path)?;
        let identity = FileIdentity::read(&file)?;
        let regular = identity.mode & libc::S_IFMT == libc::S_IFREG;
        let link = identity.mode & libc::S_IFMT == libc::S_IFLNK;
        if identity != saved.identity
            || regular != saved.digest.is_some()
            || link != saved.link_target.is_some()
            || (link
                && Some(std::fs::read_link(&path)?.as_os_str().as_bytes().to_vec())
                    != saved.link_target)
            || (regular
                && Some(snapshot::file_digest(
                    &std::fs::OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(&path)?,
                )?) != saved.digest)
        {
            return Err(snapshot::invalid("filesystem changed since snapshot"));
        }
        let (_, mnt_id) = statx(&file)?;
        let key = InodeAltKey {
            ino: identity.ino,
            dev: identity.dev,
            mnt_id,
        };
        if inodes.get_alt(&key).is_some() {
            return Err(snapshot::invalid("duplicate host inode"));
        }
        paths.insert(saved.inode, path);
        inodes.insert(
            saved.inode,
            key,
            Arc::new(InodeData {
                inode: saved.inode,
                file,
                dev: identity.dev,
                mnt_id,
                refcount: AtomicU64::new(saved.refs),
            }),
        );
    }
    if state.next_handle == 0 || state.next_handle == u64::MAX {
        return Err(snapshot::invalid("invalid next handle"));
    }
    let mut handles = BTreeMap::new();
    // Linux F_GETFL includes the kernel O_LARGEFILE bit even on 64-bit hosts.
    let allowed = libc::O_ACCMODE
        | libc::O_APPEND
        | libc::O_NONBLOCK
        | libc::O_SYNC
        | libc::O_DIRECTORY
        | libc::O_NOFOLLOW
        | libc::O_DIRECT
        | 0o100000;
    for saved in &state.handles {
        if saved.handle == 0
            || saved.handle >= state.next_handle
            || handles.contains_key(&saved.handle)
            || saved.flags & !allowed != 0
            || saved.flags & libc::O_ACCMODE == libc::O_ACCMODE
            || saved.offset > i64::MAX as u64
        {
            return Err(snapshot::invalid("invalid handle flags/id/offset"));
        }
        let path = paths
            .get(&saved.inode)
            .ok_or_else(|| snapshot::invalid("handle without inode"))?;
        let name = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| snapshot::invalid("NUL in path"))?;
        let fd = unsafe {
            libc::open(
                name.as_ptr(),
                saved.flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if FileIdentity::read(&file)?
            != state
                .inodes
                .iter()
                .find(|i| i.inode == saved.inode)
                .unwrap()
                .identity
        {
            return Err(snapshot::invalid("replaced handle"));
        }
        if file.metadata()?.is_dir() != saved.directory_entries.is_some() {
            return Err(snapshot::invalid("directory stream missing"));
        }
        if let Some(entries) = &saved.directory_entries {
            let mut cookies = std::collections::BTreeSet::new();
            for e in entries {
                if e.offset == 0
                    || !cookies.insert(e.offset)
                    || e.name.is_empty()
                    || e.name.contains(&0)
                    || e.name.contains(&b'/')
                {
                    return Err(snapshot::invalid("invalid directory cookie"));
                }
            }
        } else if unsafe { libc::lseek(fd, saved.offset as i64, libc::SEEK_SET) } < 0 {
            return Err(io::Error::last_os_error());
        }
        handles.insert(
            saved.handle,
            Arc::new(HandleData {
                inode: saved.inode,
                file: RwLock::new(file),
                exported: AtomicBool::new(false),
                directory_snapshot: saved.directory_entries.clone(),
            }),
        );
    }
    *fs.inodes.write().unwrap() = inodes;
    *fs.handles.write().unwrap() = handles;
    fs.next_handle.store(state.next_handle, Ordering::Relaxed);
    fs.announce_submounts
        .store(state.submounts, Ordering::Relaxed);
    Ok(())
}
