//! Host-visible merged root for the lifetime of a cold import.

use anyhow::{Context, Result, ensure};
use pvisor_overlay_core::sys;
use pvisor_overlayfs::api::{
    OverlayConfiguration, OverlayFs, OverlayMountConfig, OverlayMounting, OverlaySession,
    OverlaySessionControl,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

pub(crate) const FSNAME: &str = "pvisor-oci-prefix-v1";

const HARDLINK_INDEX: &str = "hardlinks-v1.json";

// Native Unix path bytes; the index is stored next to, never inside, the OCI tree.
type Hardlinks = Vec<Vec<Vec<u8>>>;

pub(crate) struct PrefixView {
    session: Option<OverlaySession>,
    merged: PathBuf,
    #[cfg(target_arch = "x86_64")]
    lower: PathBuf,
    #[cfg(target_arch = "x86_64")]
    upper: PathBuf,
    mountpoint_id: (u64, u64),
}

impl PrefixView {
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn backing_directories(&self) -> io::Result<(fs::File, fs::File)> {
        let open = |path: &Path| {
            fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                .open(path)
        };
        Ok((open(&self.lower)?, open(&self.upper)?))
    }

    pub(crate) fn has_exited(&self) -> bool {
        self.session
            .as_ref()
            .is_none_or(OverlaySessionControl::has_exited)
    }

    pub(crate) fn unmount(mut self) -> Result<()> {
        self.detach()
    }

    fn detach(&mut self) -> Result<()> {
        if let Some(session) = self.session.take() {
            session.unmount()?;
            let stat = fs::symlink_metadata(&self.merged)?;
            ensure!(
                (stat.dev(), stat.ino()) == self.mountpoint_id,
                "OCI prefix mountpoint changed"
            );
            fs::remove_dir(&self.merged)?;
        }
        Ok(())
    }
}

impl Drop for PrefixView {
    fn drop(&mut self) {
        if let Err(error) = self.detach() {
            tracing::warn!(%error, "cannot detach OCI prefix view");
        }
    }
}

fn collect_hardlinks(root: &Path) -> io::Result<Option<Hardlinks>> {
    let mut groups = BTreeMap::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(directory) = pending.pop() {
        for item in fs::read_dir(root.join(&directory))? {
            let item = item?;
            let path = directory.join(item.file_name());
            let metadata = item.metadata()?;
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.nlink() > 1 {
                if !metadata.is_file() {
                    return Ok(None);
                }
                let key = (metadata.dev(), metadata.ino(), metadata.nlink());
                groups
                    .entry(key)
                    .or_insert_with(Vec::new)
                    .push(path.as_os_str().as_bytes().to_vec());
            }
        }
    }
    let mut links = vec![];
    for ((_, _, count), mut paths) in groups {
        if paths.len() as u64 != count {
            return Ok(None);
        }
        paths.sort();
        links.push(paths);
    }
    Ok(Some(links))
}

/// Record whole hardlink groups while the extracted prefix is exclusively owned.
/// Trees the serving account cannot enumerate take the ordinary extraction path.
pub(super) fn index_hardlinks(root: &Path) -> Result<()> {
    let paths = match collect_hardlinks(root) {
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => None,
        result => result?,
    };
    let index = root
        .parent()
        .context("prefix has no owner")?
        .join(HARDLINK_INDEX);
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(index)?;
    serde_json::to_writer(file, &paths)?;
    Ok(())
}

fn mirror_metadata(source: &Path, target: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    let actual = fs::symlink_metadata(target)?;
    if (metadata.uid(), metadata.gid()) != (actual.uid(), actual.gid()) {
        sys::chown(target, metadata.uid(), metadata.gid(), false)?;
    }
    let source_xattrs = sys::list_xattrs(source)?;
    for extra in sys::list_xattrs(target)? {
        if !source_xattrs.contains(&extra) {
            sys::remove_xattr(target, OsStr::from_bytes(&extra))?;
        }
    }
    for key in source_xattrs {
        let name = OsStr::from_bytes(&key);
        let value = sys::get_xattr(source, name)?;
        if sys::get_xattr(target, name).ok().as_ref() != Some(&value) {
            sys::set_xattr(target, name, &value, 0)?;
        }
    }
    fs::set_permissions(target, metadata.permissions())?;
    sys::set_times(
        target,
        Some(metadata.accessed()?),
        Some(metadata.modified()?),
        false,
    )
}

fn seed_hardlinks(lower: &Path, upper: &Path, groups: Hardlinks) -> Result<()> {
    let lower = lower.canonicalize()?;
    let mut directories = BTreeSet::from([PathBuf::new()]);
    for group in groups {
        ensure!(group.len() >= 2, "invalid prefix hardlink group");
        let mut first = None;
        for encoded in &group {
            let relative = Path::new(OsStr::from_bytes(encoded));
            ensure!(
                !relative.as_os_str().is_empty()
                    && relative
                        .components()
                        .all(|c| matches!(c, Component::Normal(_))),
                "invalid prefix hardlink path"
            );
            let source = lower.join(relative);
            ensure!(
                source.canonicalize()? == source,
                "prefix hardlink traverses symlink"
            );
            let stat = fs::symlink_metadata(&source)?;
            ensure!(
                stat.is_file() && stat.nlink() == group.len() as u64,
                "prefix hardlink topology changed"
            );
            let parent = relative.parent().context("hardlink has no parent")?;
            directories.extend(parent.ancestors().map(Path::to_owned));
            fs::create_dir_all(upper.join(parent))?;
            let target = upper.join(relative);
            let key = (stat.dev(), stat.ino());
            if let Some((other, original)) = &first {
                ensure!(key == *original, "prefix hardlink inode changed");
                fs::hard_link(other, &target)?;
            } else {
                // Keep this entire inode group private. The overlay otherwise learns
                // aliases lazily and would expose a transiently incorrect st_nlink.
                let mut input = fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&source)?;
                let mut output = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&target)?;
                io::copy(&mut input, &mut output)?;
                mirror_metadata(&source, &target, &stat)?;
                first = Some((target, key));
            }
        }
    }
    // Adding children changes directory times; finalise children before parents.
    for relative in directories.iter().rev() {
        let source = lower.join(relative);
        mirror_metadata(
            &source,
            &upper.join(relative),
            &fs::symlink_metadata(&source)?,
        )?;
    }
    Ok(())
}

/// Check the FUSE device before spending work building a shared prefix.
pub(super) fn available() -> Result<bool> {
    match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fuse")
    {
        Ok(_) => Ok(true),
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(libc::ENODEV | libc::ENOENT | libc::EACCES | libc::EPERM)
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(e).context("open OCI prefix FUSE device"),
    }
}

fn mount_unavailable(error: &anyhow::Error, mountpoint: &Path) -> bool {
    // Only Session::new errors get this context in OverlayFs::mount. Backing
    // preparation and dispatch errors must survive even if their errno matches.
    if error.to_string() != format!("mount {}", mountpoint.display()) {
        return false;
    }
    let Some(io) = error.downcast_ref::<io::Error>() else {
        return false;
    };
    if matches!(
        io.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
    ) || matches!(io.raw_os_error(), Some(libc::ENODEV | libc::ENOSYS))
    {
        return true;
    }
    // The unprivileged helper returns diagnostics on stderr instead of an errno.
    io.kind() == io::ErrorKind::Other
        && io.to_string().lines().any(|line| {
            let Some((command, reason)) = line.split_once(": ") else {
                return false;
            };
            matches!(
                Path::new(command).file_name().and_then(OsStr::to_str),
                Some("fusermount3" | "fusermount")
            ) && matches!(
                reason,
                "mount failed: Operation not permitted" | "mount failed: Permission denied"
            )
        })
}

/// Caller exclusively owns the private import and keeps the lower fixed until
/// detachment. Its collector owns backing removal, including after server death.
pub(crate) fn mount(lower: &Path, import: &Path, merged: &Path) -> Result<Option<PrefixView>> {
    if !available()? {
        return Ok(None);
    }
    let path = lower
        .parent()
        .context("prefix has no owner")?
        .join(HARDLINK_INDEX);
    let index = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(index.metadata()?.is_file(), "invalid prefix hardlink index");
    let Some(groups): Option<Hardlinks> = serde_json::from_reader(index)? else {
        return Ok(None);
    };
    let staging = import.join(format!("prefix-view-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&staging)?;
    let upper = staging.join("upper");
    fs::create_dir(&upper)?;
    if let Err(e) = seed_hardlinks(lower, &upper, groups) {
        if e.downcast_ref::<io::Error>()
            .is_some_and(|e| e.kind() == io::ErrorKind::PermissionDenied)
        {
            super::super::store::remove_import(&staging)?;
            return Ok(None);
        }
        return Err(e);
    }
    fs::create_dir(merged)
        .with_context(|| format!("create fresh OCI view {}", merged.display()))?;
    let stat = fs::symlink_metadata(merged)?;
    let mountpoint = merged.canonicalize()?;
    let mut config = OverlayMountConfig::new(
        vec![lower.to_owned()],
        upper.clone(),
        Some(staging.join("work")),
        merged.to_owned(),
    );
    config.fsname = FSNAME.into();
    match OverlayFs::mount(config) {
        Ok(session) => Ok(Some(PrefixView {
            session: Some(session),
            #[cfg(target_arch = "x86_64")]
            lower: lower.to_owned(),
            #[cfg(target_arch = "x86_64")]
            upper,
            merged: merged.to_owned(),
            mountpoint_id: (stat.dev(), stat.ino()),
        })),
        Err(e) => {
            // Never follow a failed or disconnected mount into its backing.
            // rmdir fails with EBUSY if a FUSE session is still attached.
            fs::remove_dir(merged)
                .with_context(|| format!("remove empty OCI mountpoint after {e:#}"))?;
            if mount_unavailable(&e, &mountpoint) {
                super::super::store::remove_import(&staging)?;
                return Ok(None);
            }
            Err(e).context("mount OCI prefix view")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, UNIX_EPOCH};

    #[rstest::rstest]
    fn capability_fallback_is_limited_to_mount_device_and_helper_errors() {
        let mountpoint = Path::new("/import/merged");
        let failure = |error: io::Error| anyhow::Error::new(error).context("mount /import/merged");
        assert!(mount_unavailable(
            &failure(io::Error::from_raw_os_error(libc::EPERM)),
            mountpoint
        ));
        assert!(mount_unavailable(
            &failure(io::Error::other(
                "fusermount3: mount failed: Operation not permitted\n"
            )),
            mountpoint
        ));
        assert!(!mount_unavailable(
            &failure(io::Error::from_raw_os_error(libc::ENOSPC)),
            mountpoint
        ));
        let backing = anyhow::Error::new(io::Error::from_raw_os_error(libc::EACCES))
            .context("canonicalize lowerdir");
        assert!(!mount_unavailable(&backing, mountpoint));
    }

    #[rstest::rstest]
    fn seed_preserves_complete_private_inode_groups_and_parent_metadata() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let lower = temp.path().join("lower");
        let upper = temp.path().join("upper");
        fs::create_dir_all(lower.join("bin"))?;
        fs::create_dir(&upper)?;
        let binary = lower.join("bin/tool");
        let alias = OsStr::from_bytes(b"alias-\xff");
        fs::write(&binary, b"tool")?;
        fs::hard_link(&binary, lower.join(alias))?;
        fs::write(lower.join("ordinary"), b"lower only")?;
        let reference = UNIX_EPOCH + Duration::from_nanos(4_000_000_001);
        for rel in ["", "bin", "bin/tool"] {
            let path = lower.join(rel);
            sys::set_xattr(&path, OsStr::new("user.prefix-test"), b"value", 0)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
            sys::set_times(&path, Some(reference), Some(reference), false)?;
        }
        index_hardlinks(&lower)?;
        let groups = serde_json::from_reader(fs::File::open(temp.path().join(HARDLINK_INDEX))?)?;
        seed_hardlinks(&lower, &upper, groups)?;
        for rel in ["", "bin", "bin/tool"] {
            let path = upper.join(rel);
            let stat = fs::metadata(&path)?;
            assert_eq!(stat.modified()?, reference);
            assert_eq!(stat.mode() & 0o777, 0o755);
            assert_eq!(
                sys::get_xattr(&path, OsStr::new("user.prefix-test"))?,
                b"value"
            );
        }
        let upper_tool = upper.join("bin/tool");
        assert_eq!(fs::metadata(&upper_tool)?.nlink(), 2);
        assert_eq!(
            fs::metadata(&upper_tool)?.ino(),
            fs::metadata(upper.join(alias))?.ino()
        );
        assert_ne!(
            fs::metadata(&upper_tool)?.ino(),
            fs::metadata(&binary)?.ino()
        );
        assert!(!upper.join("ordinary").try_exists()?);
        fs::remove_file(upper.join(alias))?;
        assert_eq!(fs::metadata(&upper_tool)?.nlink(), 1);
        assert_eq!(fs::metadata(&binary)?.nlink(), 2);
        Ok(())
    }

    #[rstest::rstest]
    #[ignore = "requires a working Linux FUSE device and permission to mount"]
    fn mounted_suffix_preserves_oci_whiteouts_hardlinks_and_metadata() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let lower = temp.path().join("lower");
        fs::create_dir(&lower)?;
        fs::create_dir(lower.join("dir"))?;
        fs::write(lower.join("dir/old"), b"hidden by opaque")?;
        fs::create_dir(lower.join("rebuilt"))?;
        fs::write(lower.join("rebuilt/gone"), b"removed")?;
        fs::write(lower.join("removed"), b"whiteout")?;
        fs::write(lower.join("a"), b"original")?;
        fs::hard_link(lower.join("a"), lower.join("b"))?;
        let attr = OsStr::new("user.test-prefix");
        sys::set_xattr(&lower.join("a"), attr, b"lower", 0)?;
        let opaque = OsStr::new("user.overlay.opaque");
        sys::set_xattr(&lower.join("dir"), opaque, b"y", 0)?;

        index_hardlinks(&lower)?;

        let make_view = |name| -> Result<_> {
            let owner = temp.path().join(name);
            fs::create_dir(&owner)?;
            let root = owner.join("merged");
            let guard = mount(&lower, &owner, &root)?.context("test requires FUSE")?;
            Ok((root, guard))
        };
        let (one, first) = make_view("one")?;
        let (two, second) = make_view("two")?;
        let layer = temp.path().join("suffix.tar");
        let mut tar = tar::Builder::new(fs::File::create(&layer)?);
        for (path, bytes) in [
            ("dir/new", b"suffix".as_slice()),
            ("dir/.wh..wh..opq", b""),
            (".wh.removed", b""),
            (".wh.rebuilt", b""),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_size(bytes.len() as u64);
            h.set_mode(0o644);
            h.set_mtime(100);
            h.set_cksum();
            tar.append_data(&mut h, path, bytes)?;
        }
        let mut rebuilt = tar::Header::new_gnu();
        rebuilt.set_entry_type(tar::EntryType::Directory);
        rebuilt.set_size(0);
        rebuilt.set_mode(0o755);
        rebuilt.set_mtime(100);
        rebuilt.set_cksum();
        tar.append_data(&mut rebuilt, "rebuilt", &[][..])?;
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Link);
        h.set_size(0);
        h.set_mode(0o640);
        h.set_mtime(100);
        tar.append_link(&mut h, "from-suffix", "a")?;
        tar.finish()?;
        super::super::apply_layer(&layer, "application/vnd.oci.image.layer.v1.tar", &one)?;
        let staging = fs::read_dir(temp.path().join("one"))?
            .filter_map(Result::ok)
            .find(|e| e.file_name().as_bytes().starts_with(b"prefix-view-"))
            .context("missing private prefix backing")?
            .path();
        let rebuilt_upper = staging.join("upper/rebuilt");
        assert!(
            rebuilt_upper
                .join(pvisor_overlay_core::OPAQUE_NAME)
                .is_file()
        );
        let hints = [
            "trusted.overlay.opaque",
            "user.overlay.opaque",
            "user.fuseoverlayfs.opaque",
        ];
        let visible = one.join("rebuilt");
        let listed = sys::list_xattrs(&visible)?;
        // A privileged mount may generate trusted.overlay.opaque instead.
        let generated = hints
            .into_iter()
            .find(|hint| sys::get_xattr(&rebuilt_upper, OsStr::new(hint)).is_ok_and(|v| v == b"y"))
            .context("expected a real, on-disk opacity hint")?;
        assert!(!listed.iter().any(|n| n == generated.as_bytes()));
        assert_eq!(
            sys::get_xattr(&visible, OsStr::new(generated))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ENODATA)
        );
        assert!(!one.join("rebuilt/gone").try_exists()?);
        assert_eq!(sys::get_xattr(&two.join("dir"), opaque)?, b"y");
        assert!(
            sys::list_xattrs(&two.join("dir"))?
                .iter()
                .any(|n| n == opaque.as_bytes())
        );
        assert_eq!(fs::read(one.join("dir/new"))?, b"suffix");
        for name in ["removed", "dir/old"] {
            assert!(!one.join(name).try_exists()?);
            assert!(two.join(name).is_file());
        }
        assert_eq!(sys::get_xattr(&one.join("from-suffix"), attr)?, b"lower");
        let attrs = sys::list_xattrs(&one.join("a"))?;
        assert!(attrs.iter().any(|a| a == b"user.test-prefix"));
        fs::write(one.join("from-suffix"), b"private")?;
        fs::set_permissions(one.join("a"), fs::Permissions::from_mode(0o600))?;
        sys::set_xattr(&one.join("a"), attr, b"private", 0)?;
        let first_ino = fs::metadata(one.join("a"))?.ino();
        for name in ["a", "b", "from-suffix"] {
            let path = one.join(name);
            let stat = fs::metadata(&path)?;
            assert_eq!(stat.ino(), first_ino);
            assert_eq!(stat.nlink(), 3, "{name}");
            assert_eq!(stat.mode() & 0o777, 0o600, "{name}");
            assert_eq!(fs::read(&path)?, b"private");
            assert_eq!(sys::get_xattr(&path, attr)?, b"private");
        }
        for root in [two, lower] {
            assert_eq!(fs::read(root.join("b"))?, b"original");
            assert_eq!(fs::metadata(root.join("b"))?.nlink(), 2);
            assert_eq!(sys::get_xattr(&root.join("b"), attr)?, b"lower");
            assert!(!root.join("from-suffix").try_exists()?);
        }
        assert!(!first.has_exited() && !second.has_exited());
        first.unmount()?;
        assert!(!one.try_exists()?);
        // Exercise the cancellation/failed-extraction path through Drop.
        let second_path = temp.path().join("two/merged");
        drop(second);
        assert!(!second_path.try_exists()?);
        Ok(())
    }
}
