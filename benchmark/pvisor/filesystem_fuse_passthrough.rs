//! Benchmark-only host FUSE control: native files, no OverlayCore or journal.
//! One synchronous fuser loop and its default protocol flags match host staging.
//! Benchmark: B-FS-ENG (benchmark/README.md#b-fs-eng), role diagnostic; driven
//! by filesystem_fuse_ab.py as the transport-only lower bound.
use fuser::*;
use std::{
    collections::{BTreeSet, HashMap},
    ffi::{CString, OsStr, OsString},
    fs::{self, File, Metadata},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{FileExt, FileTypeExt, MetadataExt, PermissionsExt},
        },
    },
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
const LABELS: [&str; 8] = [
    "lookup",
    "getattr",
    "read",
    "write",
    "readdir",
    "readdirplus",
    "read_bytes",
    "write_bytes",
];
type Counts = Arc<[AtomicU64; 8]>;
fn errno(e: io::Error) -> i32 {
    e.raw_os_error().unwrap_or(libc::EIO)
}
fn cpath(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}
fn check(v: i32) -> io::Result<()> {
    if v < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn kind(m: &Metadata) -> FileType {
    let t = m.file_type();
    if t.is_dir() {
        FileType::Directory
    } else if t.is_symlink() {
        FileType::Symlink
    } else if t.is_fifo() {
        FileType::NamedPipe
    } else if t.is_socket() {
        FileType::Socket
    } else if t.is_char_device() {
        FileType::CharDevice
    } else if t.is_block_device() {
        FileType::BlockDevice
    } else {
        FileType::RegularFile
    }
}
fn attr(ino: u64, m: &Metadata) -> FileAttr {
    let ctime = if m.ctime() >= 0 {
        UNIX_EPOCH + Duration::new(m.ctime() as u64, m.ctime_nsec() as u32)
    } else {
        UNIX_EPOCH - Duration::from_secs(m.ctime().unsigned_abs())
    };
    FileAttr {
        ino,
        size: m.len(),
        blocks: m.blocks(),
        atime: m.accessed().unwrap_or(UNIX_EPOCH),
        mtime: m.modified().unwrap_or(UNIX_EPOCH),
        ctime,
        crtime: m.created().unwrap_or(ctime),
        kind: kind(m),
        perm: (m.mode() & 0o7777) as u16,
        nlink: m.nlink() as u32,
        uid: m.uid(),
        gid: m.gid(),
        rdev: m.rdev() as u32,
        blksize: m.blksize() as u32,
        flags: 0,
    }
}
struct Native {
    root: PathBuf,
    ttl: Duration,
    next_ino: u64,
    next_handle: u64,
    objects: HashMap<(u64, u64), u64>,
    paths: HashMap<u64, BTreeSet<PathBuf>>,
    files: HashMap<u64, File>,
    dirs: HashMap<u64, Vec<(OsString, PathBuf)>>,
    counts: Counts,
    trace: bool,
}
impl Native {
    fn new(root: PathBuf, ttl: Duration, counts: Counts) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(&root)?;
        Ok(Self {
            objects: HashMap::from([((metadata.dev(), metadata.ino()), FUSE_ROOT_ID)]),
            paths: HashMap::from([(FUSE_ROOT_ID, BTreeSet::from([root.clone()]))]),
            root,
            ttl,
            next_ino: 2,
            next_handle: 1,
            files: HashMap::new(),
            dirs: HashMap::new(),
            counts,
            trace: std::env::var_os("PVISOR_PASSTHROUGH_TRACE").is_some(),
        })
    }
    fn count(&self, i: usize, n: u64) {
        self.counts[i].fetch_add(n, Ordering::Relaxed);
    }
    fn path(&self, ino: u64) -> io::Result<PathBuf> {
        self.paths
            .get(&ino)
            .and_then(|p| p.first())
            .cloned()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))
    }
    fn child(&self, parent: u64, name: &OsStr) -> io::Result<PathBuf> {
        if name.as_bytes().contains(&b'/') || name == OsStr::new("..") {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        Ok(self.path(parent)?.join(name))
    }
    fn entry(&mut self, path: PathBuf) -> io::Result<FileAttr> {
        let m = fs::symlink_metadata(&path)?;
        let key = (m.dev(), m.ino());
        let ino = *self.objects.entry(key).or_insert_with(|| {
            let ino = self.next_ino;
            self.next_ino += 1;
            ino
        });
        self.paths.entry(ino).or_default().insert(path);
        Ok(attr(ino, &m))
    }
    fn open_native(&mut self, p: &Path, flags: i32, mode: u32) -> io::Result<u64> {
        let p = cpath(p)?;
        let fd = unsafe { libc::open(p.as_ptr(), flags | libc::O_CLOEXEC, mode) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let h = self.next_handle;
        self.next_handle += 1;
        self.files.insert(h, unsafe { File::from_raw_fd(fd) });
        Ok(h)
    }
    fn file(&self, h: u64) -> io::Result<&File> {
        self.files
            .get(&h)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))
    }
    fn remove_path(&mut self, path: &Path) {
        for paths in self.paths.values_mut() {
            paths.retain(|p| p != path && !p.starts_with(path));
        }
        // A backing inode may be reused after unlink. Give the new object a
        // fresh FUSE inode while retained file descriptors keep the old one.
        self.objects
            .retain(|_, ino| self.paths.get(ino).is_some_and(|p| !p.is_empty()));
    }
    fn directory(&self, h: u64) -> io::Result<Vec<(OsString, PathBuf)>> {
        self.dirs
            .get(&h)
            .cloned()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))
    }
}
impl Filesystem for Native {
    fn lookup(&mut self, _: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        self.count(0, 1);
        if self.trace && name.to_string_lossy().starts_with("fixture") {
            eprintln!(
                "native-fuse lookup parent={parent} {:?} path={:?}",
                name,
                self.path(parent)
            );
        }
        match self.child(parent, name).and_then(|p| self.entry(p)) {
            Ok(a) => reply.entry(&self.ttl, &a, 0),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn getattr(&mut self, _: &Request<'_>, ino: u64, fh: Option<u64>, reply: ReplyAttr) {
        self.count(1, 1);
        let result = if let Some(h) = fh {
            self.file(h).and_then(File::metadata)
        } else {
            self.path(ino).and_then(fs::symlink_metadata)
        };
        match result {
            Ok(m) => reply.attr(&self.ttl, &attr(ino, &m)),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn setattr(
        &mut self,
        _: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _: Option<SystemTime>,
        fh: Option<u64>,
        _: Option<SystemTime>,
        _: Option<SystemTime>,
        _: Option<SystemTime>,
        _: Option<u32>,
        reply: ReplyAttr,
    ) {
        let result = (|| -> io::Result<FileAttr> {
            let p = self.path(ino)?;
            let c = cpath(&p)?;
            if let Some(mode) = mode {
                fs::set_permissions(&p, fs::Permissions::from_mode(mode))?;
            }
            if uid.is_some() || gid.is_some() {
                check(unsafe {
                    libc::lchown(c.as_ptr(), uid.unwrap_or(u32::MAX), gid.unwrap_or(u32::MAX))
                })?;
            }
            if let Some(size) = size {
                if let Some(h) = fh {
                    self.file(h)?.set_len(size)?;
                } else {
                    fs::OpenOptions::new().write(true).open(&p)?.set_len(size)?;
                }
            }
            if atime.is_some() || mtime.is_some() {
                let ts = |v: Option<TimeOrNow>| -> io::Result<libc::timespec> {
                    Ok(match v {
                        None => libc::timespec {
                            tv_sec: 0,
                            tv_nsec: libc::UTIME_OMIT,
                        },
                        Some(TimeOrNow::Now) => libc::timespec {
                            tv_sec: 0,
                            tv_nsec: libc::UTIME_NOW,
                        },
                        Some(TimeOrNow::SpecificTime(t)) => {
                            let d = t
                                .duration_since(UNIX_EPOCH)
                                .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
                            libc::timespec {
                                tv_sec: d.as_secs() as _,
                                tv_nsec: d.subsec_nanos() as _,
                            }
                        }
                    })
                };
                let times = [ts(atime)?, ts(mtime)?];
                check(unsafe {
                    libc::utimensat(
                        libc::AT_FDCWD,
                        c.as_ptr(),
                        times.as_ptr(),
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                })?;
            }
            let m = if let Some(h) = fh {
                self.file(h)?.metadata()?
            } else {
                fs::symlink_metadata(&p)?
            };
            Ok(attr(ino, &m))
        })();
        match result {
            Ok(a) => reply.attr(&self.ttl, &a),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn readlink(&mut self, _: &Request<'_>, ino: u64, reply: ReplyData) {
        match self.path(ino).and_then(fs::read_link) {
            Ok(p) => reply.data(p.as_os_str().as_bytes()),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn mkdir(
        &mut self,
        _: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let result = (|| {
            let p = self.child(parent, name)?;
            let c = cpath(&p)?;
            check(unsafe { libc::mkdir(c.as_ptr(), mode & !umask) })?;
            self.entry(p)
        })();
        match result {
            Ok(a) => reply.entry(&self.ttl, &a, 0),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn unlink(&mut self, _: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let result = self.child(parent, name).and_then(|p| {
            if self.trace {
                eprintln!("native-fuse unlink {:?}", p);
            }
            fs::remove_file(&p)?;
            self.remove_path(&p);
            Ok(())
        });
        match result {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn rmdir(&mut self, _: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let result = self.child(parent, name).and_then(|p| {
            fs::remove_dir(&p)?;
            self.remove_path(&p);
            Ok(())
        });
        match result {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn rename(
        &mut self,
        _: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        let result = (|| -> io::Result<()> {
            if flags & !1 != 0 {
                return Err(io::Error::from_raw_os_error(libc::EINVAL));
            }
            let old = self.child(parent, name)?;
            let new = self.child(newparent, newname)?;
            if self.trace {
                eprintln!("native-fuse rename {:?} -> {:?}", old, new);
            }
            if old == new {
                return Ok(());
            }
            let (a, b) = (cpath(&old)?, cpath(&new)?);
            check(unsafe {
                libc::renameat2(
                    libc::AT_FDCWD,
                    a.as_ptr(),
                    libc::AT_FDCWD,
                    b.as_ptr(),
                    flags,
                )
            })?;
            self.remove_path(&new);
            for paths in self.paths.values_mut() {
                *paths = paths
                    .iter()
                    .map(|p| {
                        if let Ok(rel) = p.strip_prefix(&old) {
                            // Joining an empty relative path appends a slash;
                            // getattr on the renamed regular file then fails
                            // with ENOTDIR and cargo omits its final hardlink.
                            if rel.as_os_str().is_empty() {
                                new.clone()
                            } else {
                                new.join(rel)
                            }
                        } else {
                            p.clone()
                        }
                    })
                    .collect();
            }
            Ok(())
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn link(&mut self, _: &Request<'_>, ino: u64, parent: u64, name: &OsStr, reply: ReplyEntry) {
        if self.trace {
            eprintln!(
                "native-fuse link request ino={ino} parent={parent} name={name:?} source={:?} parentpath={:?}",
                self.path(ino),
                self.path(parent)
            );
        }
        let result = (|| {
            let old = self.path(ino)?;
            let new = self.child(parent, name)?;
            if self.trace {
                eprintln!("native-fuse link ino={ino} {:?} -> {:?}", old, new);
            }
            fs::hard_link(old, &new)?;
            self.entry(new)
        })();
        match result {
            Ok(a) => reply.entry(&self.ttl, &a, 0),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn symlink(
        &mut self,
        _: &Request<'_>,
        parent: u64,
        name: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        let result = self.child(parent, name).and_then(|p| {
            if self.trace {
                eprintln!("native-fuse symlink {p:?} -> {target:?}");
            }
            std::os::unix::fs::symlink(target, &p)?;
            self.entry(p)
        });
        match result {
            Ok(a) => reply.entry(&self.ttl, &a, 0),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn open(&mut self, _: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        match self.path(ino).and_then(|p| self.open_native(&p, flags, 0)) {
            Ok(h) => reply.opened(h, 0),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn create(
        &mut self,
        _: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let result = (|| {
            let p = self.child(parent, name)?;
            let h = self.open_native(&p, flags | libc::O_CREAT, mode & !umask)?;
            let a = self.entry(p)?;
            Ok::<_, io::Error>((a, h))
        })();
        match result {
            Ok((a, h)) => reply.created(&self.ttl, &a, 0, h, 0),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn read(
        &mut self,
        _: &Request<'_>,
        _: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _: i32,
        _: Option<u64>,
        reply: ReplyData,
    ) {
        self.count(2, 1);
        let mut buffer = vec![0; size as usize];
        match self
            .file(fh)
            .and_then(|f| f.read_at(&mut buffer, offset as u64))
        {
            Ok(n) => {
                self.count(6, n as u64);
                reply.data(&buffer[..n]);
            }
            Err(e) => reply.error(errno(e)),
        }
    }
    fn write(
        &mut self,
        _: &Request<'_>,
        _: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _: u32,
        _: i32,
        _: Option<u64>,
        reply: ReplyWrite,
    ) {
        self.count(3, 1);
        match self.file(fh).and_then(|f| f.write_at(data, offset as u64)) {
            Ok(n) => {
                self.count(7, n as u64);
                reply.written(n as u32);
            }
            Err(e) => reply.error(errno(e)),
        }
    }
    fn flush(&mut self, _: &Request<'_>, _: u64, fh: u64, _: u64, reply: ReplyEmpty) {
        match self.file(fh) {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn release(
        &mut self,
        _: &Request<'_>,
        _: u64,
        fh: u64,
        _: i32,
        _: Option<u64>,
        _: bool,
        reply: ReplyEmpty,
    ) {
        if self.files.remove(&fh).is_some() {
            reply.ok();
        } else {
            reply.error(libc::EBADF);
        }
    }
    fn fsync(&mut self, _: &Request<'_>, _: u64, fh: u64, datasync: bool, reply: ReplyEmpty) {
        match self.file(fh).and_then(|f| {
            if datasync {
                f.sync_data()
            } else {
                f.sync_all()
            }
        }) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn opendir(&mut self, _: &Request<'_>, ino: u64, _: i32, reply: ReplyOpen) {
        let result = (|| -> io::Result<u64> {
            let path = self.path(ino)?;
            let parent = if path == self.root {
                path.clone()
            } else {
                path.parent().unwrap().to_owned()
            };
            let mut items = vec![
                (OsString::from("."), path.clone()),
                (OsString::from(".."), parent),
            ];
            for e in fs::read_dir(path)? {
                let e = e?;
                items.push((e.file_name(), e.path()));
            }
            let h = self.next_handle;
            self.next_handle += 1;
            self.dirs.insert(h, items);
            Ok(h)
        })();
        match result {
            Ok(h) => reply.opened(h, 0),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn readdir(
        &mut self,
        _: &Request<'_>,
        _: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        self.count(4, 1);
        let items = match self.directory(fh) {
            Ok(v) => v,
            Err(e) => {
                reply.error(errno(e));
                return;
            }
        };
        for (i, (name, path)) in items.iter().enumerate().skip(offset as usize) {
            match self.entry(path.clone()) {
                Ok(a) => {
                    if reply.add(a.ino, (i + 1) as i64, a.kind, name) {
                        break;
                    }
                }
                Err(e) => {
                    reply.error(errno(e));
                    return;
                }
            }
        }
        reply.ok();
    }
    fn readdirplus(
        &mut self,
        _: &Request<'_>,
        _: u64,
        fh: u64,
        offset: i64,
        mut reply: ReplyDirectoryPlus,
    ) {
        self.count(5, 1);
        let items = match self.directory(fh) {
            Ok(v) => v,
            Err(e) => {
                reply.error(errno(e));
                return;
            }
        };
        for (i, (name, path)) in items.iter().enumerate().skip(offset as usize) {
            match self.entry(path.clone()) {
                Ok(a) => {
                    if reply.add(a.ino, (i + 1) as i64, name, &self.ttl, &a, 0) {
                        break;
                    }
                }
                Err(e) => {
                    reply.error(errno(e));
                    return;
                }
            }
        }
        reply.ok();
    }
    fn releasedir(&mut self, _: &Request<'_>, _: u64, fh: u64, _: i32, reply: ReplyEmpty) {
        if self.dirs.remove(&fh).is_some() {
            reply.ok();
        } else {
            reply.error(libc::EBADF);
        }
    }
    fn access(&mut self, _: &Request<'_>, ino: u64, mask: i32, reply: ReplyEmpty) {
        let result = self
            .path(ino)
            .and_then(|p| cpath(&p))
            .and_then(|p| check(unsafe { libc::access(p.as_ptr(), mask) }));
        match result {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn getxattr(&mut self, _: &Request<'_>, ino: u64, name: &OsStr, size: u32, reply: ReplyXattr) {
        let result = (|| -> io::Result<(usize, Vec<u8>)> {
            let path = cpath(&self.path(ino)?)?;
            let name = CString::new(name.as_bytes())
                .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
            let mut buf = vec![0; size as usize];
            let n = unsafe {
                libc::lgetxattr(
                    path.as_ptr(),
                    name.as_ptr(),
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                )
            };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok((n as usize, buf))
        })();
        match result {
            Ok((n, buf)) => {
                if size == 0 {
                    reply.size(n as u32)
                } else {
                    reply.data(&buf[..n])
                }
            }
            Err(e) => reply.error(errno(e)),
        }
    }
    fn lseek(
        &mut self,
        _: &Request<'_>,
        _: u64,
        fh: u64,
        offset: i64,
        whence: i32,
        reply: ReplyLseek,
    ) {
        let result = self.file(fh).and_then(|f| {
            let n = unsafe { libc::lseek(f.as_raw_fd(), offset, whence) };
            if n < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(n)
            }
        });
        match result {
            Ok(n) => reply.offset(n),
            Err(e) => reply.error(errno(e)),
        }
    }
}
fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() < 6 || args[4] != "--" {
        return Err(io::Error::other(
            "backing mountpoint ttl-seconds -- command args",
        ));
    }
    let backing = PathBuf::from(&args[1]).canonicalize()?;
    let mountpoint = PathBuf::from(&args[2]);
    fs::create_dir(&mountpoint)?;
    let ttl: u64 = args[3]
        .to_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::other("invalid TTL"))?;
    let counts = Arc::new(std::array::from_fn(|_| AtomicU64::new(0)));
    let fs = Native::new(backing, Duration::from_secs(ttl), counts.clone())?;
    let options = [
        MountOption::FSName("pvisor-bench-native".into()),
        MountOption::DefaultPermissions,
        MountOption::NoAtime,
        MountOption::RW,
    ];
    let session = Session::new(fs, &mountpoint, &options)?;
    let session = BackgroundSession::new(session)?;
    let mounts = fs::read_to_string("/proc/self/mountinfo")?;
    let line = mounts
        .lines()
        .find(|line| line.split_whitespace().nth(4) == mountpoint.to_str())
        .ok_or_else(|| io::Error::other("FUSE mount missing"))?;
    if !line.contains(" - fuse") {
        return Err(io::Error::other("control did not mount FUSE"));
    }
    println!("PASSTHROUGH_MOUNT {}", line);
    let status = Command::new(&args[5])
        .args(&args[6..])
        .current_dir(&mountpoint)
        .status()?;
    session.unmount()?;
    let fields: Vec<_> = LABELS
        .iter()
        .zip(counts.iter())
        .map(|(k, v)| format!("\"{k}\":{}", v.load(Ordering::Relaxed)))
        .collect();
    println!("PASSTHROUGH_STATS {{{}}}", fields.join(","));
    std::process::exit(status.code().unwrap_or(125));
}
