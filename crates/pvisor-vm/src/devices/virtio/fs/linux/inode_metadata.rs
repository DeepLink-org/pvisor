//! Guest identities for native inodes that cannot portably store user xattrs.
//! Callers reserve and hide the catalog directory from their guest namespace.
//! All access is relative to pinned directories; no guest link is followed.
use super::statx::{LinuxStatx, STATX_BASIC_STATS, STATX_BTIME};
use std::{
    ffi::{CStr, CString},
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
};

pub(super) const DIRECTORY: &CStr = c".pvisor-managed";
// Preserve the marker used by existing managed-volume symlink catalogs.
const MARKER: &CStr = c"user.pvisor.managed_symlinks";
const VERSION: &[u8] = b"1";

pub(in crate::devices::virtio::fs) struct Catalog {
    root: File,
    name: &'static CStr,
}

fn open_at(parent: &File, name: &CStr, flags: i32) -> io::Result<File> {
    // SAFETY: bounded C name, pinned parent, owned descriptor on success.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid private inode metadata")
}

impl Catalog {
    pub(in crate::devices::virtio::fs) fn new(
        root: &Path,
        name: &'static CStr,
    ) -> io::Result<Self> {
        let root = CString::new(root.as_os_str().as_encoded_bytes()).map_err(|_| invalid())?;
        let fd = unsafe {
            libc::open(
                root.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let this = Self {
            root: unsafe { File::from_raw_fd(fd) },
            name,
        };
        let _guard = super::managed::lock(&this.root)?;
        this.directory(false)?;
        Ok(this)
    }

    // The root flock serializes directory initialization and read/modify/write
    // across VMs. A pre-existing unmarked name is a collision, never adopted.
    fn directory(&self, create: bool) -> io::Result<Option<File>> {
        let directory = match open_at(&self.root, self.name, libc::O_RDONLY | libc::O_DIRECTORY) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !create {
                    return Ok(None);
                }
                if unsafe { libc::mkdirat(self.root.as_raw_fd(), self.name.as_ptr(), 0o700) } < 0 {
                    return Err(io::Error::last_os_error());
                }
                let file = open_at(&self.root, self.name, libc::O_RDONLY | libc::O_DIRECTORY)?;
                if unsafe {
                    libc::fsetxattr(
                        file.as_raw_fd(),
                        MARKER.as_ptr(),
                        VERSION.as_ptr().cast(),
                        VERSION.len(),
                        libc::XATTR_CREATE,
                    )
                } < 0
                {
                    return Err(io::Error::last_os_error());
                }
                file.sync_all()?;
                self.root.sync_all()?;
                file
            }
            Err(error) => return Err(error),
        };
        let st = directory.metadata()?;
        let mut marker = [0u8; 8];
        let size = unsafe {
            libc::fgetxattr(
                directory.as_raw_fd(),
                MARKER.as_ptr(),
                marker.as_mut_ptr().cast(),
                marker.len(),
            )
        };
        if st.uid() != unsafe { libc::geteuid() }
            || st.mode() & 0o7777 != 0o700
            || size != VERSION.len() as isize
            || &marker[..VERSION.len()] != VERSION
        {
            return Err(invalid());
        }
        Ok(Some(directory))
    }

    fn key(file: &File) -> io::Result<CString> {
        let mut st = std::mem::MaybeUninit::<LinuxStatx>::zeroed();
        // SAFETY: the pinned descriptor and Linux wire-layout output stay live.
        // Avoid glibc-only statx bindings so static musl builds use the same ABI.
        if unsafe {
            libc::syscall(
                libc::SYS_statx,
                file.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
                STATX_BASIC_STATS | STATX_BTIME,
                st.as_mut_ptr(),
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        let st = unsafe { st.assume_init() };
        if st.stx_mask & STATX_BTIME == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "private inode metadata requires inode birth times",
            ));
        }
        if !matches!(
            u32::from(st.stx_mode) & libc::S_IFMT,
            libc::S_IFLNK | libc::S_IFSOCK
        ) {
            return Err(invalid());
        }
        // Birth time survives chmod/chown/rename/hardlinks and disambiguates
        // recycled inode numbers. Creation always seeds a fresh record too.
        Ok(CString::new(format!(
            "{:x}-{:x}-{:x}-{:x}-{:x}",
            st.stx_dev_major,
            st.stx_dev_minor,
            st.stx_ino,
            st.stx_btime.tv_sec,
            st.stx_btime.tv_nsec
        ))
        .unwrap())
    }

    fn read(
        directory: &File,
        key: &CStr,
        mut attr: libc::stat64,
    ) -> io::Result<Option<libc::stat64>> {
        let file = match open_at(directory, key, libc::O_RDONLY | libc::O_NONBLOCK) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let st = file.metadata()?;
        if !st.is_file()
            || st.uid() != unsafe { libc::geteuid() }
            || st.mode() & 0o7777 != 0o600
            || st.nlink() != 1
            || st.len() > 64
        {
            return Err(invalid());
        }
        let mut text = String::new();
        file.take(65).read_to_string(&mut text)?;
        let mut fields = text.split(':');
        attr.st_uid = fields
            .next()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;
        attr.st_gid = fields
            .next()
            .ok_or_else(invalid)?
            .parse()
            .map_err(|_| invalid())?;
        // Old symlink records contain only UID:GID; keep their native mode.
        if let Some(mode) = fields.next() {
            let mode = u32::from_str_radix(mode, 8).map_err(|_| invalid())?;
            if mode & libc::S_IFMT != attr.st_mode & libc::S_IFMT
                || mode & !(libc::S_IFMT | 0o7777) != 0
            {
                return Err(invalid());
            }
            attr.st_mode = mode;
        }
        if fields.next().is_some() {
            return Err(invalid());
        }
        Ok(Some(attr))
    }

    fn write(directory: &File, key: &CStr, attr: &libc::stat64) -> io::Result<()> {
        let path = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
        let mut temporary = tempfile::NamedTempFile::new_in(&path)?;
        write!(
            temporary,
            "{}:{}:0{:o}",
            attr.st_uid, attr.st_gid, attr.st_mode
        )?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(path.join(key.to_str().map_err(|_| invalid())?))
            .map_err(|error| error.error)?;
        directory.sync_all()
    }

    pub(in crate::devices::virtio::fs) fn attributes(
        &self,
        file: &File,
        attr: libc::stat64,
    ) -> io::Result<Option<libc::stat64>> {
        let _guard = super::managed::lock(&self.root)?;
        let Some(directory) = self.directory(false)? else {
            return Ok(None);
        };
        Self::read(&directory, &Self::key(file)?, attr)
    }

    // The closure runs under the same volume lock as the write, so concurrent
    // UID-only and GID-only updates cannot lose each other's fields.
    pub(in crate::devices::virtio::fs) fn update(
        &self,
        file: &File,
        attr: libc::stat64,
        change: impl FnOnce(&mut libc::stat64) -> io::Result<()>,
    ) -> io::Result<libc::stat64> {
        let _guard = super::managed::lock(&self.root)?;
        let directory = self.directory(true)?.ok_or_else(invalid)?;
        let key = Self::key(file)?;
        let mut attr = Self::read(&directory, &key, attr)?.unwrap_or(attr);
        change(&mut attr)?;
        Self::write(&directory, &key, &attr)?;
        Ok(attr)
    }
}
