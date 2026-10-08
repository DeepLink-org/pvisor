//! Direct lazy lower attachment for virtio-fs: metadata projections, no FUSE mount.
use super::{
    CacheClient,
    backend::{ContentReader, Node, RemoteFs},
    client::ClientBinding,
};
use anyhow::{Context, ensure};
use fs2::FileExt;
use pvisor_journal::api::{DurableFiles, Persistence};
use pvisor_overlay_core::{
    backend::{BackendAttachment, FileAttr, FileType, ReadOnlyBackend},
    sys,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    ffi::OsStr,
    fs::{self, OpenOptions},
    io,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileExt as UnixFileExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const DESCRIPTOR: &str = "backend.json";
const PREFIX: &str = ".pvisor-direct-image-";

pub(crate) fn private_owner(root: &Path) -> Option<&Path> {
    root.parent().filter(|parent| {
        root.file_name() == Some(OsStr::new("lower"))
            && parent
                .file_name()
                .is_some_and(|name| name.as_bytes().starts_with(PREFIX.as_bytes()))
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    version: u32,
    client: ClientBinding,
    handle: String,
    cache: PathBuf,
    metadata_cache: Option<PathBuf>,
}

struct Projection {
    root: PathBuf,
    objects: PathBuf,
    source: Mutex<RemoteFs>,
    reader: Arc<ContentReader>,
    projected: Mutex<HashSet<PathBuf>>,
    complete: Mutex<HashSet<u64>>,
}

fn io_error(error: anyhow::Error) -> io::Error {
    if let Some(error) = error.downcast_ref::<io::Error>() {
        if let Some(errno) = error.raw_os_error() {
            return io::Error::from_raw_os_error(errno);
        }
        return io::Error::new(error.kind(), error.to_string());
    }
    io::Error::other(error.to_string())
}

impl Projection {
    fn source(&self) -> io::Result<std::sync::MutexGuard<'_, RemoteFs>> {
        self.source
            .lock()
            .map_err(|_| io::Error::other("image metadata lock poisoned"))
    }
    fn node(&self, relative: &Path) -> io::Result<Node> {
        pvisor_overlay_core::OverlayCore::validate_rel(relative)?;
        // Resolve through parent names so a remote symlink is never an ancestor.
        let mut source = self.source()?;
        let mut node = source.node(1).map_err(io_error)?.clone();
        for component in relative.components() {
            node = source
                .child(node.attr.ino, component.as_os_str())
                .map_err(io_error)?;
        }
        Ok(node)
    }
    fn project(&self, relative: &Path) -> io::Result<Node> {
        let node = self.node(relative)?;
        if self.projected.lock().unwrap().contains(relative) {
            return Ok(node);
        }
        if let Some(parent) = relative.parent().filter(|p| !p.as_os_str().is_empty()) {
            self.project(parent)?;
        }
        let path = self.root.join(relative);
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.objects.join("projection.lock"))?;
        lock.lock_exclusive()?;
        let missing = match fs::symlink_metadata(&path) {
            Ok(_) => false,
            Err(e) if e.kind() == io::ErrorKind::NotFound => true,
            Err(e) => return Err(e),
        };
        if missing {
            match node.attr.kind {
                FileType::Directory => fs::create_dir(&path)?,
                FileType::RegularFile => {
                    let object = self.objects.join(node.object_id.to_string());
                    let file = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .truncate(false)
                        .mode(0o600)
                        .custom_flags(libc::O_NOFOLLOW)
                        .open(&object)?;
                    if !file.metadata()?.is_file() {
                        return Err(io::Error::from_raw_os_error(libc::EINVAL));
                    }
                    if file.metadata()?.len() != node.attr.size {
                        file.set_len(node.attr.size)?;
                    }
                    match fs::hard_link(&object, &path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(e) => return Err(e),
                    }
                }
                FileType::Symlink => std::os::unix::fs::symlink(
                    OsStr::from_bytes(
                        node.target
                            .as_deref()
                            .ok_or_else(|| io::Error::from_raw_os_error(libc::EIO))?,
                    ),
                    &path,
                )?,
                FileType::NamedPipe => sys::mknod(&path, FileType::NamedPipe.mode() | 0o600, 0)?,
                FileType::Socket => {
                    drop(std::os::unix::net::UnixListener::bind(&path)?);
                }
                _ => return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP)),
            }
        }
        // Projection permissions protect host cache files; guest Unix ownership
        // is supplied by the immutable metadata and the existing override contract.
        if node.attr.kind != FileType::Symlink {
            let permission = if node.attr.kind == FileType::Directory {
                0o700
            } else {
                0o600
            };
            if fs::symlink_metadata(&path)?.mode() & 0o7777 != permission {
                fs::set_permissions(&path, fs::Permissions::from_mode(permission))?;
            }
            let key = OsStr::new("user.containers.override_stat");
            if sys::get_xattr(&path, key).ok().as_deref() != Some(node.override_stat.as_slice()) {
                sys::set_xattr(&path, key, &node.override_stat, 0)?;
            }
        }
        #[cfg(target_os = "macos")]
        if node.attr.kind == FileType::Symlink {
            // macOS supports no-follow symlink xattrs; passthrough and owned
            // copies use this same contract for Linux guest ownership.
            let key = OsStr::new("user.containers.override_stat");
            if sys::get_xattr(&path, key).ok().as_deref() != Some(node.override_stat.as_slice()) {
                sys::set_xattr(&path, key, &node.override_stat, 0)?;
            }
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.modified()? != node.attr.mtime {
            sys::set_times(
                &path,
                Some(node.attr.atime),
                Some(node.attr.mtime),
                node.attr.kind == FileType::Symlink,
            )?;
        }
        self.projected.lock().unwrap().insert(relative.to_owned());
        Ok(node)
    }
    fn read_node(&self, node: &Node, offset: u64, size: u32) -> io::Result<Vec<u8>> {
        self.reader.read(node, offset, size).map_err(io_error)
    }
    fn restore_directory_time(&self, relative: &Path, node: &Node) -> io::Result<()> {
        let path = self.root.join(relative);
        if fs::symlink_metadata(&path)?.modified()? != node.attr.mtime {
            sys::set_times(&path, Some(node.attr.atime), Some(node.attr.mtime), false)?;
        }
        Ok(())
    }
}

impl ReadOnlyBackend for Projection {
    fn prepare_metadata(&self, relative: &Path) -> io::Result<()> {
        self.project(relative).map(|_| ())
    }
    fn attributes(&self, relative: &Path) -> io::Result<FileAttr> {
        self.node(relative).map(|node| node.attr)
    }
    fn prepare_directory(&self, relative: &Path) -> io::Result<()> {
        let node = self.project(relative)?;
        let entries = self
            .source()?
            .entries(node.attr.ino)
            .map_err(io_error)?
            .clone();
        for (_, _, name) in entries {
            if name != "." && name != ".." {
                self.project(&relative.join(name))?;
            }
        }
        // Creating children must not change the immutable directory's guest mtime.
        self.restore_directory_time(relative, &node)?;
        Ok(())
    }
    fn read_at(&self, relative: &Path, offset: u64, size: u32) -> io::Result<Vec<u8>> {
        let node = self.node(relative)?;
        self.read_node(&node, offset, size)
    }
    fn materialize_file(&self, relative: &Path) -> io::Result<()> {
        let node = self.project(relative)?;
        if node.attr.kind != FileType::RegularFile {
            return Ok(());
        }
        if self.complete.lock().unwrap().contains(&node.object_id) {
            return Ok(());
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(
                self.objects
                    .join(format!("{}.content.lock", node.object_id)),
            )?;
        lock.lock_exclusive()?;
        if self.complete.lock().unwrap().contains(&node.object_id) {
            return Ok(());
        }
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.root.join(relative))?;
        let metadata = file.metadata()?;
        let receipt = [
            metadata.dev().to_le_bytes(),
            metadata.ino().to_le_bytes(),
            node.attr.size.to_le_bytes(),
        ]
        .concat();
        let marker = self.objects.join(format!("{}.complete", node.object_id));
        if metadata.len() == node.attr.size
            && fs::read(&marker).ok().as_deref() == Some(receipt.as_slice())
        {
            self.complete.lock().unwrap().insert(node.object_id);
            return Ok(());
        }
        let mut offset = 0;
        while offset < node.attr.size {
            let bytes = self.read_node(&node, offset, super::MAX_READ)?;
            if bytes.is_empty() {
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
            file.write_all_at(&bytes, offset)?;
            offset += bytes.len() as u64;
        }
        sys::set_times(
            &self.root.join(relative),
            Some(node.attr.atime),
            Some(node.attr.mtime),
            false,
        )?;
        // Publish only after the complete file is durable. A different VM
        // runner can reuse it without rewriting a checkpoint's backing inode.
        file.sync_all()?;
        Persistence::atomic_write(&marker, &receipt, 0o600).map_err(io::Error::other)?;
        self.complete.lock().unwrap().insert(node.object_id);
        Ok(())
    }
    fn materialize_tree(&self, relative: &Path) -> io::Result<()> {
        let node = self.project(relative)?;
        if node.attr.kind == FileType::Directory {
            self.prepare_directory(relative)?;
            let entries = self
                .source()?
                .entries(node.attr.ino)
                .map_err(io_error)?
                .clone();
            for (_, _, name) in entries {
                if name != "." && name != ".." {
                    self.materialize_tree(&relative.join(name))?;
                }
            }
            self.restore_directory_time(relative, &node)?;
        } else {
            self.materialize_file(relative)?;
        }
        if relative.as_os_str().is_empty() {
            // Once every image name exists, aliases can use those native
            // links. Retire the private indexing links so an owned export
            // passes the ordinary no-external-hardlinks check unchanged.
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(self.objects.join("projection.lock"))?;
            lock.lock_exclusive()?;
            for entry in fs::read_dir(&self.objects)? {
                let entry = entry?;
                let name = entry.file_name();
                if !name.as_bytes().is_empty() && name.as_bytes().iter().all(u8::is_ascii_digit) {
                    fs::remove_file(entry.path())?;
                }
            }
        }
        Ok(())
    }
}

pub struct DirectImage {
    attachment: BackendAttachment,
    _directory: tempfile::TempDir,
}
impl DirectImage {
    pub(super) fn new(source: RemoteFs, store: &Path) -> anyhow::Result<Self> {
        let directory = tempfile::Builder::new().prefix(PREFIX).tempdir_in(store)?;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
        let root = directory.path().join("lower");
        fs::create_dir(&root)?;
        let binding = Binding {
            version: 1,
            client: source.client.binding(),
            handle: source.digest.clone(),
            cache: source.cache.clone(),
            metadata_cache: source.metadata_cache.clone(),
        };
        let descriptor = directory.path().join(DESCRIPTOR);
        Persistence::atomic_write(&descriptor, &serde_json::to_vec(&binding)?, 0o600)?;
        let attachment = attach(&root, source)?;
        Ok(Self {
            attachment,
            _directory: directory,
        })
    }
    pub(super) fn root(&self) -> &Path {
        self.attachment.root()
    }
}

fn attach(root: &Path, source: RemoteFs) -> anyhow::Result<BackendAttachment> {
    let objects = root
        .parent()
        .context("direct lower has no owner")?
        .join("objects");
    fs::create_dir_all(&objects)?;
    let projection = Arc::new(Projection {
        root: root.to_owned(),
        objects,
        reader: source.reader.clone(),
        source: Mutex::new(source),
        projected: Mutex::default(),
        complete: Mutex::default(),
    });
    projection.prepare_metadata(Path::new(""))?;
    BackendAttachment::new(root, projection).map_err(Into::into)
}

pub(crate) struct RunnerLowers {
    _attachments: Vec<BackendAttachment>,
    _network: Vec<super::network::NetworkAccess>,
    pub(crate) read_only: Vec<PathBuf>,
    pub(crate) read_write: Vec<PathBuf>,
}

/// Reopen explicit immutable sources in the self-exec VM runner before Landlock.
pub(crate) fn attach_runner_lowers<'a>(
    roots: impl Iterator<Item = &'a PathBuf>,
) -> anyhow::Result<RunnerLowers> {
    let mut owners = RunnerLowers {
        _attachments: vec![],
        _network: vec![],
        read_only: vec![],
        read_write: vec![],
    };
    let mut seen = HashSet::new();
    for root in roots {
        let root = root.canonicalize()?;
        if !seen.insert(root.clone()) {
            continue;
        }
        let Some(parent) = private_owner(&root) else {
            continue;
        };
        let parent_metadata = fs::symlink_metadata(parent)?;
        ensure!(
            parent_metadata.is_dir()
                && parent_metadata.uid() == unsafe { libc::geteuid() }
                && parent_metadata.mode() & 0o077 == 0,
            "unsafe direct image owner"
        );
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(parent.join(DESCRIPTOR))?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o077 == 0
                && metadata.len() < 1024 * 1024,
            "unsafe direct image descriptor"
        );
        let binding: Binding = serde_json::from_reader(file)?;
        ensure!(binding.version == 1, "unsupported direct image binding");
        let mut client_binding = binding.client;
        if cfg!(target_os = "linux") && client_binding.needs_host_network() {
            let (access, local_binding) =
                super::network::NetworkAccess::start(parent, client_binding, &binding.handle)?;
            owners._network.push(access);
            client_binding = local_binding;
        }
        let client = CacheClient::from_binding(client_binding)?;
        if let Some(location) = client.address().strip_prefix("file://") {
            owners.read_only.push(location.into());
        }
        owners.read_write.push(parent.to_owned());
        fs::create_dir_all(&binding.cache)?;
        owners.read_write.push(binding.cache.clone());
        if let Some(metadata) = &binding.metadata_cache {
            fs::create_dir_all(metadata)?;
            owners.read_write.push(metadata.clone());
        }
        if let Some(objects) = client.local_objects_directory() {
            fs::create_dir_all(&objects)?;
            owners.read_write.push(objects);
        }
        let source = RemoteFs::new(
            client,
            binding.handle,
            binding.cache,
            binding.metadata_cache,
        )?;
        owners._attachments.push(attach(&root, source)?);
    }
    Ok(owners)
}

#[cfg(test)]
mod tests;
