//! Streaming copies through guest control or a fenced stopped OverlayCore.
use super::*;
use pvisor_guest::api::{FileRequest, StdinFrame};
use pvisor_overlay_core::{OverlayCore, backend, sys};
use std::ffi::{CString, OsStr, OsString};
use std::os::{
    fd::{AsRawFd, FromRawFd},
    unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{FileExt, PermissionsExt},
    },
};

struct Entry {
    directory: bool,
    mode: u32,
}
trait Tree {
    fn stat(&self, path: &Path) -> anyhow::Result<Option<Entry>>;
    fn names(&self, path: &Path) -> anyhow::Result<Vec<OsString>>;
    fn read(&self, path: &Path, output: &mut File) -> anyhow::Result<()>;
    fn write(&self, path: &Path, input: &mut File, mode: u32) -> anyhow::Result<()>;
    fn mkdir(&self, path: &Path) -> anyhow::Result<()>;
    fn chmod(&self, path: &Path, mode: u32) -> anyhow::Result<()>;
}

fn normal(path: &Path) -> anyhow::Result<PathBuf> {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(name) => result.push(name),
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            _ => bail!("cp paths cannot contain '..'"),
        }
    }
    Ok(result)
}

fn entry(metadata: fs::Metadata) -> anyhow::Result<Entry> {
    ensure!(
        metadata.is_dir() || metadata.is_file(),
        "cp supports ordinary files/directories, not symlinks or special files"
    );
    Ok(Entry {
        directory: metadata.is_dir(),
        mode: metadata.mode() & 0o777,
    })
}

// Caller-selected host parent is canonicalized once, then pinned. All subsequent
// traversal uses openat/O_NOFOLLOW; guest-supplied names never become authority
// to follow a host link or truncate a FIFO/device.
struct Host {
    root: File,
}
impl Host {
    fn endpoint(path: &Path) -> anyhow::Result<(Self, PathBuf)> {
        let absolute = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        };
        // The caller explicitly selects this host parent. Resolve it using host
        // semantics (including ../ and symlink/../), then pin its descriptor.
        // Only recursively received entry names are restricted to no-follow
        // relative components; do not apply guest traversal rules to host argv.
        let absolute =
            if absolute.file_name().is_none() || absolute.as_os_str().as_bytes().ends_with(b"/.") {
                absolute.canonicalize()?
            } else {
                absolute
            };
        let name = absolute
            .file_name()
            .context("host copy endpoint needs a basename")?;
        let parent = absolute
            .parent()
            .context("host endpoint has no parent")?
            .canonicalize()?;
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(parent)?;
        Ok((Self { root }, name.into()))
    }
    fn open(&self, path: &Path, flags: i32) -> io::Result<File> {
        let mut directory = self.root.try_clone()?;
        let components: Vec<_> = path.components().collect();
        for (index, component) in components.iter().enumerate() {
            let std::path::Component::Normal(name) = component else {
                return Err(io::Error::from_raw_os_error(libc::EINVAL));
            };
            let name = CString::new(name.as_bytes())?;
            let flags = if index + 1 == components.len() {
                flags
            } else {
                libc::O_RDONLY | libc::O_DIRECTORY
            };
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                    0o600,
                )
            };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            directory = unsafe { File::from_raw_fd(fd) };
        }
        Ok(directory)
    }
}

impl Tree for Host {
    fn stat(&self, path: &Path) -> anyhow::Result<Option<Entry>> {
        match self.open(path, libc::O_RDONLY) {
            Ok(file) => Ok(Some(entry(file.metadata()?)?)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    fn names(&self, path: &Path) -> anyhow::Result<Vec<OsString>> {
        let directory = self.open(path, libc::O_RDONLY | libc::O_DIRECTORY)?;
        let fd = unsafe { libc::dup(directory.as_raw_fd()) };
        ensure!(fd >= 0, "{}", io::Error::last_os_error());
        let dir = unsafe { libc::fdopendir(fd) };
        if dir.is_null() {
            unsafe {
                libc::close(fd);
            }
            return Err(io::Error::last_os_error().into());
        }
        struct Guard(*mut libc::DIR);
        impl Drop for Guard {
            fn drop(&mut self) {
                unsafe {
                    libc::closedir(self.0);
                }
            }
        }
        let _guard = Guard(dir);
        let mut names = vec![];
        loop {
            #[cfg(target_os = "linux")]
            unsafe {
                *libc::__errno_location() = 0;
            }
            #[cfg(target_os = "macos")]
            unsafe {
                *libc::__error() = 0;
            }
            let item = unsafe { libc::readdir(dir) };
            if item.is_null() {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(0) {
                    return Err(error.into());
                }
                break;
            }
            let name = unsafe { std::ffi::CStr::from_ptr((*item).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                names.push(OsString::from_vec(name.to_vec()));
            }
            ensure!(
                names.len() <= 100_000,
                "cp directory exceeds 100000 entries"
            );
        }
        names.sort();
        Ok(names)
    }
    fn read(&self, path: &Path, output: &mut File) -> anyhow::Result<()> {
        let mut input = self.open(path, libc::O_RDONLY)?;
        ensure!(
            input.metadata()?.is_file(),
            "cp source is not a regular file"
        );
        io::copy(&mut input, output)?;
        Ok(())
    }
    fn write(&self, path: &Path, input: &mut File, mode: u32) -> anyhow::Result<()> {
        let mut output = self.open(path, libc::O_WRONLY | libc::O_CREAT)?;
        ensure!(
            output.metadata()?.is_file(),
            "cp destination is not a regular file"
        );
        ensure!(
            output.metadata()?.nlink() == 1,
            "cp refuses hard-linked host destinations"
        );
        output.set_len(0)?;
        io::copy(input, &mut output)?;
        output.set_permissions(fs::Permissions::from_mode(mode & 0o777))?;
        output.sync_all()?;
        Ok(())
    }
    fn mkdir(&self, path: &Path) -> anyhow::Result<()> {
        let parent = self.open(
            path.parent().unwrap_or(Path::new("")),
            libc::O_RDONLY | libc::O_DIRECTORY,
        )?;
        let name = CString::new(
            path.file_name()
                .context("invalid directory name")?
                .as_bytes(),
        )?;
        ensure!(
            unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } == 0,
            "{}",
            io::Error::last_os_error()
        );
        Ok(())
    }
    fn chmod(&self, path: &Path, mode: u32) -> anyhow::Result<()> {
        self.open(path, libc::O_RDONLY)?
            .set_permissions(fs::Permissions::from_mode(mode & 0o777))?;
        Ok(())
    }
}

struct Guest<'a> {
    store: &'a Store,
    instance: &'a Instance,
}
impl Guest<'_> {
    fn connect(&self, request: FileRequest) -> anyhow::Result<UnixStream> {
        let mut stream = self.store.connect(self.instance)?;
        GuestFrameWrite::write_frame(&mut stream, &Request::File { request })?;
        Ok(stream)
    }
    fn value(&self, request: FileRequest) -> anyhow::Result<Option<serde_json::Value>> {
        match GuestFrameRead::read_frame(&mut self.connect(request)?)? {
            Reply::Value { value } => Ok(Some(value)),
            Reply::Failure {
                errno: libc::ENOENT,
                ..
            } => Ok(None),
            Reply::Failure { message, .. } | Reply::Error { message } => bail!("{message}"),
            _ => bail!("invalid guest file reply"),
        }
    }
    fn path(path: &Path) -> PathBuf {
        Path::new("/").join(path)
    }
}
impl Tree for Guest<'_> {
    fn stat(&self, path: &Path) -> anyhow::Result<Option<Entry>> {
        let Some(value) = self.value(FileRequest::Stat {
            path: Self::path(path),
        })?
        else {
            return Ok(None);
        };
        ensure!(
            value["isSymlink"] == false,
            "cp does not follow symlink endpoints"
        );
        Ok(Some(Entry {
            directory: value["type"] == "dir",
            mode: value["mode"].as_u64().context("invalid guest mode")? as u32 & 0o777,
        }))
    }
    fn names(&self, path: &Path) -> anyhow::Result<Vec<OsString>> {
        let value = self
            .value(FileRequest::List {
                path: Self::path(path),
            })?
            .context("guest directory disappeared")?;
        value
            .as_array()
            .context("invalid directory reply")?
            .iter()
            .map(|value| {
                let name = value["name"].as_str().context("invalid guest filename")?;
                ensure!(
                    normal(Path::new(name))? == Path::new(name)
                        && Path::new(name).components().count() == 1,
                    "unsafe guest filename"
                );
                Ok(name.into())
            })
            .collect()
    }
    fn read(&self, path: &Path, output: &mut File) -> anyhow::Result<()> {
        let mut stream = self.connect(FileRequest::Read {
            path: Self::path(path),
        })?;
        loop {
            match GuestFrameRead::read_frame(&mut stream)? {
                Reply::Ready => {}
                Reply::Output { stream: 1, bytes } => output.write_all(&bytes)?,
                Reply::Exit { code: 0 } => return Ok(()),
                Reply::Failure { message, .. } | Reply::Error { message } => bail!("{message}"),
                _ => bail!("invalid guest read reply"),
            }
        }
    }
    fn write(&self, path: &Path, input: &mut File, mode: u32) -> anyhow::Result<()> {
        let mut stream = self.connect(FileRequest::Write {
            path: Self::path(path),
            mode: mode & 0o777,
        })?;
        match GuestFrameRead::read_frame(&mut stream)? {
            Reply::Ready => {}
            Reply::Failure { message, .. } | Reply::Error { message } => bail!("{message}"),
            _ => bail!("invalid guest write admission"),
        }
        let mut bytes = [0; 16384];
        loop {
            let count = input.read(&mut bytes)?;
            if count == 0 {
                break;
            }
            GuestFrameWrite::write_frame(
                &mut stream,
                &StdinFrame::Data {
                    bytes: bytes[..count].to_vec(),
                },
            )?;
        }
        GuestFrameWrite::write_frame(&mut stream, &StdinFrame::Eof {})?;
        match GuestFrameRead::read_frame(&mut stream)? {
            Reply::Value { .. } => Ok(()),
            Reply::Failure { message, .. } | Reply::Error { message } => bail!("{message}"),
            _ => bail!("invalid guest write completion"),
        }
    }
    fn mkdir(&self, path: &Path) -> anyhow::Result<()> {
        self.value(FileRequest::MakeDir {
            path: Self::path(path),
            mode: 0o700,
        })?
        .context("guest directory creation failed")?;
        Ok(())
    }
    fn chmod(&self, path: &Path, mode: u32) -> anyhow::Result<()> {
        self.value(FileRequest::Chmod {
            path: Self::path(path),
            mode: mode & 0o777,
        })?
        .context("guest chmod failed")?;
        Ok(())
    }
}

struct Offline {
    core: OverlayCore,
    _image: ImageRoot,
}
impl Offline {
    fn new(store: &Store, instance: &Instance, path: &Path) -> anyhow::Result<Self> {
        ensure!(
            instance.config.mounts.iter().all(|mount| {
                let target = mount.target.strip_prefix("/").unwrap_or(&mount.target);
                !path.starts_with(target) && !target.starts_with(path)
            }),
            "stopped cp cannot traverse mount targets; use a running container"
        );
        let image = store.open_image(&instance.image)?;
        let root = image.rootfs().to_owned();
        let directory = store.directory(&instance.id);
        // Reopen the existing backing model, just as the VM/build path does.
        // Core honors a persisted private marker; cp must never convert an
        // unmarked, nonempty upper into a different ownership model.
        let core = OverlayCore::new(
            vec![root],
            directory.join("upper"),
            Some(directory.join("work")),
        )?;
        Ok(Self {
            core,
            _image: image,
        })
    }
    fn check(&self, path: &Path) -> anyhow::Result<()> {
        for parent in path.ancestors().skip(1) {
            ensure!(
                self.core.metadata(parent)?.is_dir(),
                "cp cannot traverse guest symlinks"
            );
        }
        Ok(())
    }
}
impl Tree for Offline {
    fn stat(&self, path: &Path) -> anyhow::Result<Option<Entry>> {
        self.check(path)?;
        match self.core.metadata(path) {
            Ok(metadata) => {
                let mut attr = entry(metadata)?;
                let resolved = self
                    .core
                    .resolve_checked(path)?
                    .context("copy source disappeared")?;
                if let Ok(value) =
                    sys::get_xattr(&resolved.path, OsStr::new("user.containers.override_stat"))
                {
                    let value = std::str::from_utf8(&value)?;
                    if let Some(mode) = value.rsplit(':').next() {
                        attr.mode = u32::from_str_radix(mode, 8)? & 0o777;
                    }
                }
                Ok(Some(attr))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    fn names(&self, path: &Path) -> anyhow::Result<Vec<OsString>> {
        self.check(path)?;
        let mut names = vec![];
        for (name, _) in self.core.directory_candidates(path)? {
            if self.core.resolve_checked(&path.join(&name))?.is_some() {
                names.push(name);
            }
        }
        Ok(names)
    }
    fn read(&self, path: &Path, output: &mut File) -> anyhow::Result<()> {
        self.check(path)?;
        let resolved = self
            .core
            .resolve_checked(path)?
            .context("copy source missing")?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&resolved.path)?;
        ensure!(
            file.metadata()?.is_file(),
            "cp source must be a regular file"
        );
        let mut offset = 0;
        let mut buffer = [0; 16384];
        loop {
            let count = if let Some(bytes) = backend::read_at(&resolved.path, offset, 16384)? {
                output.write_all(&bytes)?;
                bytes.len()
            } else {
                let count = file.read_at(&mut buffer, offset)?;
                output.write_all(&buffer[..count])?;
                count
            };
            if count == 0 {
                return Ok(());
            }
            offset += count as u64;
        }
    }
    fn write(&self, path: &Path, input: &mut File, mode: u32) -> anyhow::Result<()> {
        self.check(path)?;
        let mut file = if self.stat(path)?.is_some() {
            let upper = self.core.copy_up(path)?;
            OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(upper)?
        } else {
            self.core
                .create_file(path, 0o600, libc::O_WRONLY | libc::O_NOFOLLOW)?
        };
        ensure!(
            file.metadata()?.is_file(),
            "cp destination must be a regular file"
        );
        file.set_len(0)?;
        io::copy(input, &mut file)?;
        file.sync_all()?;
        self.chmod(path, mode)
    }
    fn mkdir(&self, path: &Path) -> anyhow::Result<()> {
        self.check(path)?;
        self.core.create_dir(path, 0o700)?;
        Ok(())
    }
    fn chmod(&self, path: &Path, mode: u32) -> anyhow::Result<()> {
        let upper = self.core.copy_up(path)?;
        sys::set_xattr(
            &upper,
            OsStr::new("user.containers.override_stat"),
            format!("0:0:0{:o}", mode & 0o777).as_bytes(),
            0,
        )?;
        Ok(())
    }
}

struct CopyMode {
    contents: bool,
    destination_directory: bool,
}

fn directory_syntax(path: &Path) -> bool {
    let raw = path.as_os_str().as_bytes();
    raw.ends_with(b"/") || raw.ends_with(b"/.") || raw == b"." || raw == b".."
}

fn transfer(
    source: &dyn Tree,
    from: &Path,
    destination: &dyn Tree,
    to: &Path,
    mode: CopyMode,
    scratch: &Path,
) -> anyhow::Result<()> {
    let source_entry = source.stat(from)?.context("cp source does not exist")?;
    ensure!(
        !mode.contents || source_entry.directory,
        "source /. requires a directory"
    );
    let destination_entry = destination.stat(to)?;
    if !source_entry.directory && mode.destination_directory {
        ensure!(
            destination_entry
                .as_ref()
                .is_some_and(|entry| entry.directory),
            "cp destination ending in '/' must be an existing directory"
        );
    }
    let target = match destination_entry {
        Some(entry) if entry.directory && !mode.contents => {
            to.join(from.file_name().context("source needs a basename")?)
        }
        _ => to.to_owned(),
    };
    let parent = target.parent().unwrap_or(Path::new(""));
    ensure!(
        destination
            .stat(parent)?
            .is_some_and(|entry| entry.directory),
        "cp destination parent must already exist"
    );
    fn walk(
        source: &dyn Tree,
        from: &Path,
        destination: &dyn Tree,
        to: &Path,
        scratch: &Path,
        depth: u32,
        budget: &mut usize,
    ) -> anyhow::Result<()> {
        ensure!(
            depth <= 128 && *budget > 0,
            "cp tree exceeds depth/entry limits"
        );
        *budget -= 1;
        let attr = source
            .stat(from)?
            .context("copy source does not exist or disappeared")?;
        let existing = destination.stat(to)?;
        if attr.directory {
            ensure!(
                existing.as_ref().is_none_or(|entry| entry.directory),
                "cannot copy a directory over a file"
            );
            let created = existing.is_none();
            if created {
                destination.mkdir(to)?;
            }
            for name in source.names(from)? {
                ensure!(
                    Path::new(&name).components().count() == 1
                        && normal(Path::new(&name))? == Path::new(&name),
                    "invalid copy entry name"
                );
                let child = from.join(&name);
                walk(
                    source,
                    &child,
                    destination,
                    &to.join(name),
                    scratch,
                    depth + 1,
                    budget,
                )?;
            }
            if created {
                destination.chmod(to, attr.mode)?;
            }
        } else {
            ensure!(
                existing.is_none_or(|entry| !entry.directory),
                "cannot copy a file over a directory"
            );
            let mut file = tempfile::tempfile_in(scratch)?;
            source.read(from, &mut file)?;
            file.rewind()?;
            destination.write(to, &mut file, attr.mode)?;
        }
        Ok(())
    }
    walk(source, from, destination, &target, scratch, 0, &mut 100_000)
}

impl Store {
    /// Export a stopped container Session's merged rootfs as tar. Attached volume
    /// contents are excluded: the underlying image/upper directory is exported.
    /// The catalog fence prevents startup/removal for the whole operation.
    /// Live/staged Sessions and unsupported special files/capabilities fail
    /// explicitly; this operation never pauses, starts or signals a workload.
    /// Bytes are privately spooled before streaming, so source errors emit none;
    /// output I/O failures can follow partial output. No image metadata is saved.
    pub fn export_rootfs(&self, selector: &str, output: &mut impl Write) -> anyhow::Result<()> {
        let _guard = self.lock()?;
        let instance = self.resolve(selector)?;
        ensure!(
            !self.active(&instance)?
                && !matches!(instance.state.as_str(), "starting" | "restarting"),
            "live rootfs export is unsupported; stop the Session first"
        );
        ensure!(
            !instance.config.mounts.iter().any(|mount| mount.staged),
            "rootfs export refuses staged Sessions"
        );
        let image = self.open_image(&instance.image)?;
        let directory = self.directory(&instance.id);
        let core = OverlayCore::new(
            vec![image.rootfs().to_owned()],
            directory.join("upper"),
            Some(directory.join("work")),
        )?;
        let temporary = tempfile::tempdir_in(&self.root)?;
        let archive = temporary.path().join("rootfs.tar");
        build::export_filesystem(&core, &archive)?;
        io::copy(&mut File::open(archive)?, output)?;
        Ok(())
    }

    /// Copy ordinary files/directories out of a running or stopped container.
    /// Guest paths are root-relative, never host paths. Uses native guest file
    /// streams when live, or the merged OverlayCore under the catalog fence when
    /// stopped; never starts a workload. Host owners remain the invoking UID.
    /// Symlinks/special files, archive streams, ownership preservation, staged
    /// containers and stopped mount-target traversal are explicitly unsupported.
    /// Copies are not snapshots or transactions; failure can leave partial output.
    /// Each file is spooled privately before destination mutation. Directory
    /// copies are bounded to 100000 entries/depth 128; live lists allow 1024 per
    /// directory. Source `/.` copies contents rather than the directory itself.
    pub fn copy_from(
        &self,
        selector: &str,
        source: &Path,
        destination: &Path,
    ) -> anyhow::Result<()> {
        let contents = source.as_os_str().as_bytes().ends_with(b"/.");
        let destination_directory = directory_syntax(destination);
        let source = normal(source)?;
        let (host, target) = Host::endpoint(destination)?;
        let guard = self.lock()?;
        let instance = self.resolve(selector)?;
        ensure!(
            !instance.config.mounts.iter().any(|mount| mount.staged),
            "cp refuses staged containers"
        );
        if self.active(&instance)? {
            ensure!(instance.state == "running", "container is not ready for cp");
            drop(guard);
            transfer(
                &Guest {
                    store: self,
                    instance: &instance,
                },
                &source,
                &host,
                &target,
                CopyMode {
                    contents,
                    destination_directory,
                },
                &self.root,
            )
        } else {
            ensure!(
                !matches!(instance.state.as_str(), "starting" | "restarting"),
                "container lifecycle is unsettled"
            );
            transfer(
                &Offline::new(self, &instance, &source)?,
                &source,
                &host,
                &target,
                CopyMode {
                    contents,
                    destination_directory,
                },
                &self.root,
            )
        }
    }

    /// Copy ordinary host files/directories into a container, with the same
    /// fencing, bounds and unsupported cases as [`Self::copy_from`]. New guest
    /// files use root ownership; basic permission bits are retained, not
    /// setuid/setgid, xattrs or timestamps. Read-only live mounts reject writes.
    /// Destination parents must exist; omitted guest leading slash means root.
    pub fn copy_to(&self, source: &Path, selector: &str, destination: &Path) -> anyhow::Result<()> {
        let contents = source.as_os_str().as_bytes().ends_with(b"/.");
        let destination_directory = directory_syntax(destination);
        let (host, from) = Host::endpoint(source)?;
        let destination = normal(destination)?;
        let guard = self.lock()?;
        let instance = self.resolve(selector)?;
        ensure!(
            !instance.config.mounts.iter().any(|mount| mount.staged),
            "cp refuses staged containers"
        );
        if self.active(&instance)? {
            ensure!(instance.state == "running", "container is not ready for cp");
            drop(guard);
            transfer(
                &host,
                &from,
                &Guest {
                    store: self,
                    instance: &instance,
                },
                &destination,
                CopyMode {
                    contents,
                    destination_directory,
                },
                &self.root,
            )
        } else {
            ensure!(
                !matches!(instance.state.as_str(), "starting" | "restarting"),
                "container lifecycle is unsettled"
            );
            transfer(
                &host,
                &from,
                &Offline::new(self, &instance, &destination)?,
                &destination,
                CopyMode {
                    contents,
                    destination_directory,
                },
                &self.root,
            )
        }
    }
}
