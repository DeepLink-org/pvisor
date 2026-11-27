//! Conservative qualification of a sealed local OCI prefix view. A false answer
//! always delegates to the ordinary complete scan of the mounted image.
use super::{Response, metadata_at, open_child};
use crate::image::cache::MAX_READ;
use anyhow::{Context, Result, ensure};
use pvisor_overlay_core::{Resolved, ResolvedMetadata};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

#[derive(Default)]
struct Bounds {
    entries: usize,
    spans: u64,
    linked_file_paths: u64,
    largest_file: u64,
}

fn fd_path(file: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

fn check_attributes(file: &File, stat: &fs::Metadata, upper: bool) -> Result<()> {
    let mut names = [0u8; 256];
    let n = unsafe { libc::flistxattr(file.as_raw_fd(), names.as_mut_ptr().cast(), names.len()) };
    if n == 0 {
        return Ok(());
    }
    if n < 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ENOTSUP) {
            return Ok(());
        }
        return Err(e.into());
    }
    ensure!(upper && stat.is_dir(), "backing has extended attributes");
    let backing = ResolvedMetadata {
        // The trailing dot addresses the pinned directory itself, including for
        // lgetxattr, which otherwise inspects the procfs magic link instead.
        resolved: Resolved {
            path: fd_path(file).join("."),
            is_upper: true,
        },
        metadata: stat.clone(),
        layer: 0,
    };
    for name in names[..n as usize]
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
    {
        ensure!(
            backing.is_upper_opacity_hint(OsStr::from_bytes(name))?,
            "visible upper xattr"
        );
    }
    Ok(())
}

struct Scan {
    counts: Bounds,
    upper: bool,
    device: u64,
    max_entries: usize,
    max_spans: u64,
    longest_root: usize,
}

impl Scan {
    fn visit(
        &mut self,
        parent: &File,
        name: &[u8],
        depth: usize,
        relative_len: usize,
    ) -> Result<()> {
        // Bound open directory fds and the native lookup paths built by OverlayCore,
        // including a whiteout name below a directory. Exceeding either just falls back.
        ensure!(
            depth <= 128 && self.longest_root + relative_len + 32 < libc::PATH_MAX as usize,
            "deep image tree"
        );
        self.counts.entries += 1;
        ensure!(self.counts.entries <= self.max_entries, "entry limit");
        let Response::Metadata {
            kind,
            inode,
            nlink,
            size,
            mtime_nsec,
            ..
        } = metadata_at(parent, name)?
        else {
            anyhow::bail!("not a source metadata response");
        };
        ensure!(
            inode > 0 && nlink > 0 && (0..1_000_000_000).contains(&mtime_nsec),
            "invalid entry metadata"
        );
        if kind == "symlink" {
            // metadata_at read the link without following it and checked protocol length.
            // Linux validate_pool_attributes likewise skips symlink attributes.
            return Ok(());
        }
        let directory = kind == "directory";
        ensure!(directory || kind == "file", "special backing entry");
        let handle = open_child(parent, OsStr::from_bytes(name), directory)?;
        let stat = handle.metadata()?;
        let required = if directory { 0o500 } else { 0o400 };
        ensure!(
            stat.dev() == self.device
                && stat.ino() == inode
                && stat.nlink() == nlink
                && stat.len() == size
                && stat.uid() == unsafe { libc::geteuid() }
                && stat.mode() & required == required,
            "changed or inaccessible backing entry"
        );
        check_attributes(&handle, &stat, self.upper)?;
        if directory {
            // Only enumerate immediate names on the anchored FD. Never open an entry
            // using DirEntry::path(), or follow a symlink from the source tree.
            for child in fs::read_dir(fd_path(&handle))? {
                let name = child?.file_name();
                self.visit(
                    &handle,
                    name.as_bytes(),
                    depth + 1,
                    relative_len + name.len() + 1,
                )?;
            }
        } else {
            let spans = size.div_ceil(u64::from(MAX_READ));
            self.counts.spans = self
                .counts
                .spans
                .checked_add(spans)
                .context("span overflow")?;
            ensure!(self.counts.spans <= self.max_spans, "span limit");
            self.counts.largest_file = self.counts.largest_file.max(spans);
            if nlink > 1 {
                self.counts.linked_file_paths += 1;
            }
        }
        Ok(())
    }
}

/// Only for the OCI adapter's private, immutable native lower and finished private
/// upper, without additional backing providers, injected entries, or further writers.
/// Both FDs must be pinned by the caller from that same view. A positive result
/// applies to eligibility only: publication still validates metadata and file bytes.
pub(crate) fn supports_private_view(
    base: &File,
    upper: &File,
    max_entries: usize,
    max_spans: usize,
) -> bool {
    let check = || -> Result<bool> {
        let longest_root = fs::read_link(fd_path(base))?
            .as_os_str()
            .len()
            .max(fs::read_link(fd_path(upper))?.as_os_str().len());
        let scan = |dir: &File, upper: bool| -> Result<Bounds> {
            let mut scan = Scan {
                counts: Bounds::default(),
                upper,
                device: dir.metadata()?.dev(),
                max_entries,
                max_spans: max_spans as u64,
                longest_root,
            };
            scan.visit(dir, b".", 0, 0)?;
            Ok(scan.counts)
        };
        let base = scan(base, false)?;
        let upper = scan(upper, true)?;
        // Every visible path has a physical entry at its relative name in at least
        // one backing. Whiteouts, opaque dirs, replacements and the repeated root
        // only overcount. An unmaterialized copied-up alias is possible only for a
        // multiply-linked native lower regular file; each such lower path could
        // expose the largest upper file. This bound needs no cross-layer inode match.
        let entries = base
            .entries
            .checked_add(upper.entries)
            .context("entry overflow")?;
        let alias_spans = base
            .linked_file_paths
            .checked_mul(upper.largest_file)
            .context("alias span overflow")?;
        let spans = base
            .spans
            .checked_add(upper.spans)
            .and_then(|s| s.checked_add(alias_spans))
            .context("span overflow")?;
        Ok(entries <= max_entries && spans <= max_spans as u64)
    };
    check().unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn counts_each_physical_path_and_checks_copied_alias_upper_bounds() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let lower = temp.path().join("base");
        let top = temp.path().join("upper");
        fs::create_dir(&lower)?;
        fs::create_dir(&top)?;
        fs::write(lower.join("file"), b"a")?;
        fs::hard_link(lower.join("file"), lower.join("alias"))?;
        symlink("/etc/passwd", lower.join("sym"))?;
        File::create(top.join("file"))?.set_len(u64::from(MAX_READ) + 1)?;
        let base = File::open(lower)?;
        let upper = File::open(top)?;
        // 2 roots + 4 names; 2 lower spans + 2 upper + 2 aliases * 2 spans.
        assert!(supports_private_view(&base, &upper, 6, 8));
        assert!(!supports_private_view(&base, &upper, 5, 8));
        assert!(!supports_private_view(&base, &upper, 6, 7));
        Ok(())
    }

    #[test]
    fn special_entries_unreadable_files_and_real_attributes_fall_back() -> Result<()> {
        use std::os::unix::net::UnixListener;
        let temp = tempfile::tempdir()?;
        let base = File::open(temp.path())?;
        let top = tempfile::tempdir()?;
        let upper = File::open(top.path())?;
        let socket = temp.path().join("special");
        let _listener = UnixListener::bind(&socket)?;
        assert!(!supports_private_view(&base, &upper, 100, 100));
        fs::remove_file(socket)?;
        let path = top.path().join("data");
        fs::write(&path, b"data")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o100))?;
        assert!(!supports_private_view(&base, &upper, 100, 100));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        pvisor_overlay_core::sys::set_xattr(&path, OsStr::new("user.real"), b"required", 0)?;
        assert!(!supports_private_view(&base, &upper, 100, 100));
        Ok(())
    }

    #[test]
    fn opacity_hints_are_ignored_only_on_core_marked_upper_directories() -> Result<()> {
        let bottom = tempfile::tempdir()?;
        let top = tempfile::tempdir()?;
        let base = File::open(bottom.path())?;
        let upper = File::open(top.path())?;
        let dir = top.path().join("directory");
        fs::create_dir(&dir)?;
        let attribute = OsStr::new("user.overlay.opaque");
        pvisor_overlay_core::sys::set_xattr(&dir, attribute, b"y", 0)?;
        assert!(!supports_private_view(&base, &upper, 100, 100));
        fs::write(dir.join(pvisor_overlay_core::OPAQUE_NAME), b"")?;
        assert!(supports_private_view(&base, &upper, 100, 100));
        assert!(!supports_private_view(&upper, &base, 100, 100));
        pvisor_overlay_core::sys::set_xattr(&dir, attribute, b"n", 0)?;
        assert!(!supports_private_view(&base, &upper, 100, 100));
        Ok(())
    }
}
