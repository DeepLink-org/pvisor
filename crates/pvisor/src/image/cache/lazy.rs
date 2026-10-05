//! Lazy image preparation with separate host FUSE and direct VM ownership.
use super::backend::RemoteFs;
use super::{CacheClient, Request as CacheRequest, Response, architecture, hash};
use crate::image::oci::{ImageStore, PreparedImage};
use anyhow::{Context, ensure};
use fuser::{
    BackgroundSession, FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyData,
    ReplyDirectory, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyXattr, Request, Session,
};
use std::ffi::OsStr;
use std::fs;
#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const TTL: Duration = Duration::from_secs(3600);

pub(crate) struct FuseMount {
    session: Option<BackgroundSession>,
    path: PathBuf,
}
impl Drop for FuseMount {
    fn drop(&mut self) {
        crate::util::startup_mark("image.unmount_begin");
        if let Some(session) = self.session.take()
            && let Err(error) = session.unmount()
        {
            crate::diagnostics::diagnostic(format_args!(
                "unmount lazy image {}: {error}",
                self.path.display()
            ));
        }
        #[cfg(target_os = "linux")]
        let _ = fs::remove_dir(&self.path);
        crate::util::startup_mark("image.unmount_ready");
    }
}

pub(crate) fn prepare_vm_image(
    image: &str,
    store: Option<PathBuf>,
) -> anyhow::Result<(PreparedImage, Option<super::direct::DirectImage>)> {
    let client = CacheClient::discover()?;
    let store = ImageStore::new(store)?;
    let Some(client) = client else {
        return Ok((
            super::progress::loading("resolving and preparing local image", || {
                store.prepare(image)
            })?,
            None,
        ));
    };
    let downloads = super::progress::Downloads::new(image);
    crate::diagnostics::diagnostic(format_args!(
        "pVisor image: lazy loading from {}",
        client.address()
    ));
    let (response, _) =
        super::progress::loading("waiting for cache to resolve and prepare image", || {
            client.request(CacheRequest::Prepare {
                image: image.into(),
                architecture: architecture().into(),
                refresh: false,
            })
        })?;
    let (prepared, mount) = direct_prepared(client, response, &store.root, downloads, None)?;
    Ok((prepared, Some(mount)))
}

/// Retain the lazy backend for the complete native Run lifetime.
/// Host consumers own a FUSE mount; VM consumers own a direct attachment.
pub struct LazyImage {
    digest: String,
    backing: ImageBacking,
}

enum ImageBacking {
    Host(FuseMount),
    Vm(super::direct::DirectImage),
}

impl LazyImage {
    pub fn rootfs(&self) -> &Path {
        match &self.backing {
            ImageBacking::Host(mount) => &mount.path,
            ImageBacking::Vm(image) => image.root(),
        }
    }
    pub fn manifest_digest(&self) -> &str {
        &self.digest
    }
}

pub fn open_image_handle_for_host(
    config: super::CacheConfig,
    handle: &str,
) -> anyhow::Result<LazyImage> {
    let store = ImageStore::new(config.image_store.clone())?;
    let client = CacheClient::from_config(config)?;
    let (response, _) = client.request(CacheRequest::Open {
        handle: handle.into(),
        architecture: architecture().into(),
    })?;
    let (prepared, mount) = mount_prepared(
        client,
        response,
        &store.root,
        super::progress::Downloads::new(handle),
        Some(handle),
    )?;
    Ok(LazyImage {
        digest: prepared.digest,
        backing: ImageBacking::Host(mount),
    })
}

fn mount_prepared(
    client: CacheClient,
    response: Response,
    store: &Path,
    downloads: super::progress::Downloads,
    expected: Option<&str>,
) -> anyhow::Result<(PreparedImage, FuseMount)> {
    let (mut prepared, filesystem) = prepare_remote(client, response, downloads, expected)?;
    let mount = super::progress::loading("mounting lazy rootfs", || mount(filesystem, store))?;
    prepared.rootfs = mount.path.clone();
    Ok((prepared, mount))
}

fn direct_prepared(
    client: CacheClient,
    response: Response,
    store: &Path,
    downloads: super::progress::Downloads,
    expected: Option<&str>,
) -> anyhow::Result<(PreparedImage, super::direct::DirectImage)> {
    let (mut prepared, filesystem) = prepare_remote(client, response, downloads, expected)?;
    let direct = super::direct::DirectImage::new(filesystem, store)?;
    prepared.rootfs = direct.root().to_owned();
    Ok((prepared, direct))
}

pub fn open_image_handle_for_vm(
    config: super::CacheConfig,
    handle: &str,
) -> anyhow::Result<LazyImage> {
    let store = ImageStore::new(config.image_store.clone())?;
    let client = CacheClient::from_config(config)?;
    let (response, _) = client.request(CacheRequest::Open {
        handle: handle.into(),
        architecture: architecture().into(),
    })?;
    let (prepared, owner) = direct_prepared(
        client,
        response,
        &store.root,
        super::progress::Downloads::new(handle),
        Some(handle),
    )?;
    Ok(LazyImage {
        digest: prepared.digest,
        backing: ImageBacking::Vm(owner),
    })
}

fn prepare_remote(
    client: CacheClient,
    response: Response,
    downloads: super::progress::Downloads,
    expected: Option<&str>,
) -> anyhow::Result<(PreparedImage, RemoteFs)> {
    let Response::Prepared {
        image_handle,
        metadata_generation,
        totals,
        digest,
        architecture: platform,
        env,
        entrypoint,
        cmd,
    } = response
    else {
        anyhow::bail!("expected cache prepared response");
    };
    ensure!(
        platform == architecture(),
        "cache returned the wrong image architecture"
    );
    crate::image::oci::digest_hex(&digest)?;
    if let Some(expected) = expected {
        ensure!(
            image_handle == expected,
            "cache changed the requested immutable revision"
        );
    }
    let read_handle = image_handle;
    let cache = dirs::cache_dir()
        .context("cannot find user cache directory")?
        .join("pvisor/blocks")
        .join(&hash(client.address().as_bytes())[7..])
        .join(&hash(read_handle.as_bytes())[7..]);
    fs::create_dir_all(&cache)?;
    downloads.totals(totals);
    if let Some(totals) = totals {
        crate::diagnostics::diagnostic(format_args!(
            "pVisor image: prepared {digest}; {} files, {:.1} MiB (contents fetched on demand)",
            totals.files,
            totals.bytes as f64 / (1024.0 * 1024.0)
        ));
    }
    let metadata_cache = Some({
        let generation = metadata_generation;
        dirs::cache_dir()
            .expect("cache directory already resolved")
            .join("pvisor/metadata/v1")
            .join(&hash(client.address().as_bytes())[7..])
            .join(&digest[7..])
            .join(&hash(generation.as_bytes())[7..])
    });
    let filesystem = super::progress::loading("loading root metadata", || {
        RemoteFs::new(client, read_handle, cache, metadata_cache)
    })?;
    *filesystem.downloads.lock().unwrap() = downloads;
    Ok((
        PreparedImage {
            rootfs: PathBuf::new(),
            digest,
            env,
            entrypoint,
            cmd,
        },
        filesystem,
    ))
}

fn mount(filesystem: RemoteFs, _store: &Path) -> anyhow::Result<FuseMount> {
    #[cfg(target_os = "macos")]
    let mountpoint =
        PathBuf::from("/Volumes").join(format!("pvisor-image-{}", uuid::Uuid::new_v4()));
    #[cfg(target_os = "linux")]
    let mountpoint = {
        let path = _store.join(format!(".lazy-mount-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        path
    };
    #[allow(unused_mut)]
    let mut options = vec![
        MountOption::FSName("pvisor-image".into()),
        MountOption::RO,
        MountOption::NoAtime,
    ];
    #[cfg(target_os = "macos")]
    options.push(MountOption::DefaultPermissions);
    #[cfg(target_os = "macos")]
    options.push(MountOption::CUSTOM("backend=fskit".into()));
    let session = Session::new(filesystem, &mountpoint, &options)
        .context("mount lazy image lower (FUSE is required); unset PVISOR_CACHE_BACKEND/PVISOR_CACHE_LOCATION and set PVISOR_CACHE_SERVER=off to use local OCI extraction")?;
    #[cfg(target_os = "linux")]
    let session = BackgroundSession::new_interruptible(session)?;
    #[cfg(not(target_os = "linux"))]
    let session = BackgroundSession::new(session)?;
    let mount = FuseMount {
        session: Some(session),
        path: mountpoint.clone(),
    };
    // FSKit attaches asynchronously after its request loop starts.
    #[cfg(target_os = "macos")]
    for _ in 0..250 {
        if pvisor_overlayfs::is_mountpoint(&mountpoint) {
            break;
        }
        if mount
            .session
            .as_ref()
            .is_some_and(|s| s.guard.is_finished())
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    ensure!(
        mountpoint.is_dir(),
        "lazy image mount did not become available"
    );
    Ok(mount)
}

fn errno(error: anyhow::Error) -> i32 {
    let code = error
        .chain()
        .filter_map(|e| e.downcast_ref::<std::io::Error>())
        .find_map(|e| {
            e.raw_os_error().or(match e.kind() {
                std::io::ErrorKind::NotFound => Some(libc::ENOENT),
                std::io::ErrorKind::PermissionDenied => Some(libc::EACCES),
                _ => None,
            })
        })
        .unwrap_or(libc::EIO);
    if code != libc::ENOENT {
        crate::diagnostics::diagnostic(format_args!("lazy image I/O: {error:#}"));
    }
    code
}
// Host FUSE adapter; VM consumers access RemoteFs through the direct backend.
impl Filesystem for RemoteFs {
    fn getxattr(&mut self, _: &Request<'_>, ino: u64, name: &OsStr, size: u32, reply: ReplyXattr) {
        match self.node(ino) {
            Ok(node) if name == OsStr::new("user.containers.override_stat") => {
                if size == 0 {
                    reply.size(node.override_stat.len() as u32);
                } else if node.override_stat.len() <= size as usize {
                    reply.data(&node.override_stat);
                } else {
                    reply.error(libc::ERANGE);
                }
            }
            Ok(_) => reply.error(libc::ENODATA),
            Err(error) => reply.error(errno(error)),
        }
    }
    fn listxattr(&mut self, _: &Request<'_>, ino: u64, size: u32, reply: ReplyXattr) {
        if let Err(error) = self.node(ino) {
            reply.error(errno(error));
            return;
        }
        let name = b"user.containers.override_stat\0";
        if size == 0 {
            reply.size(name.len() as u32);
        } else if size as usize >= name.len() {
            reply.data(name);
        } else {
            reply.error(libc::ERANGE);
        }
    }

    fn lookup(&mut self, _: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        match self.child(parent, name) {
            Ok(node) => reply.entry(&TTL, &fuse_attr(&node.attr), 0),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn getattr(&mut self, _: &Request<'_>, ino: u64, _: Option<u64>, reply: ReplyAttr) {
        match self.node(ino) {
            Ok(node) => reply.attr(&TTL, &fuse_attr(&node.attr)),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn readlink(&mut self, _: &Request<'_>, ino: u64, reply: ReplyData) {
        match self
            .node(ino)
            .and_then(|node| node.target.as_deref().context("not a symlink"))
        {
            Ok(target) => reply.data(target),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn open(&mut self, _: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        // FSKit can request O_RDWR for a read on a read-only volume.
        if flags & (libc::O_TRUNC | libc::O_APPEND) != 0 {
            reply.error(libc::EROFS);
            return;
        }
        match self.node(ino) {
            Ok(node)
                if matches!(
                    node.attr.kind,
                    pvisor_overlay_core::backend::FileType::RegularFile
                        | pvisor_overlay_core::backend::FileType::Symlink
                ) =>
            {
                reply.opened(ino, 0)
            }
            _ => reply.error(libc::EINVAL),
        }
    }
    fn read(
        &mut self,
        _: &Request<'_>,
        ino: u64,
        _: u64,
        offset: i64,
        size: u32,
        _: i32,
        _: Option<u64>,
        reply: ReplyData,
    ) {
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        match self.read_range(ino, offset as u64, size) {
            Ok(bytes) => reply.data(&bytes),
            Err(e) => reply.error(errno(e)),
        }
    }
    fn opendir(&mut self, _: &Request<'_>, ino: u64, _: i32, reply: ReplyOpen) {
        match self.node(ino) {
            Ok(node) if node.attr.kind == pvisor_overlay_core::backend::FileType::Directory => {
                reply.opened(ino, 0)
            }
            _ => reply.error(libc::ENOTDIR),
        }
    }
    fn readdir(
        &mut self,
        _: &Request<'_>,
        ino: u64,
        _: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        if offset < 0 {
            reply.error(libc::EINVAL);
            return;
        }
        match self.entries(ino) {
            Ok(entries) => {
                for (index, (ino, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
                    if reply.add(*ino, (index + 1) as i64, fuse_kind(*kind), name) {
                        break;
                    }
                }
                reply.ok();
            }
            Err(e) => reply.error(errno(e)),
        }
    }
    fn statfs(&mut self, _: &Request<'_>, _: u64, reply: ReplyStatfs) {
        reply.statfs(0, 0, 0, 0, 0, 4096, 255, 4096);
    }
}

fn fuse_kind(kind: pvisor_overlay_core::backend::FileType) -> FileType {
    use pvisor_overlay_core::backend::FileType as Kind;
    match kind {
        Kind::Directory => FileType::Directory,
        Kind::RegularFile => FileType::RegularFile,
        Kind::Symlink => FileType::Symlink,
        Kind::NamedPipe => FileType::NamedPipe,
        Kind::Socket => FileType::Socket,
        Kind::BlockDevice => FileType::BlockDevice,
        Kind::CharDevice => FileType::CharDevice,
    }
}
fn fuse_attr(a: &pvisor_overlay_core::backend::FileAttr) -> FileAttr {
    FileAttr {
        ino: a.ino,
        size: a.size,
        blocks: a.blocks,
        atime: a.atime,
        mtime: a.mtime,
        ctime: a.ctime,
        crtime: a.crtime,
        kind: fuse_kind(a.kind),
        perm: if cfg!(target_os = "macos") && a.ino == 1 {
            0o700
        } else {
            a.perm
        },
        uid: if cfg!(target_os = "macos") {
            unsafe { libc::geteuid() }
        } else {
            a.uid
        },
        gid: if cfg!(target_os = "macos") {
            unsafe { libc::getegid() }
        } else {
            a.gid
        },
        nlink: a.nlink,
        rdev: a.rdev,
        blksize: a.blksize,
        flags: a.flags,
    }
}

#[cfg(test)]
mod tests;
