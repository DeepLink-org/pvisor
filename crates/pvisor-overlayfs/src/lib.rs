//! Embeddable cross-platform FUSE overlay implementation.
//!
//! pVisor owns [`OverlaySession`] directly, so the pVisor process is also the
//! FUSE userspace server. The `pvisor-overlayfs` binary is only a debugging
//! and manual-mount CLI wrapper around this library.

mod fs;
mod observation;
use anyhow::{Context, Result, bail};
use fs::OverlayFs;
use fuser::{BackgroundSession, MountOption, Session};
pub use observation::FsMetrics;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct OverlayMountConfig {
    pub apply_target: Option<PathBuf>,
    pub baseline_lower: Option<PathBuf>,
    pub lower_dirs: Vec<PathBuf>,
    pub upper_dir: PathBuf,
    pub work_dir: Option<PathBuf>,
    pub mountpoint: PathBuf,
    pub allow_other: bool,
    pub allow_root: bool,
    pub default_permissions: bool,
    pub read_only: bool,
    pub fsname: String,
    /// macFUSE backend (`kernel` or `fskit`); defaults to FSKit on macOS.
    pub backend: Option<String>,
    pub debug: bool,
    /// Optional durable first-touch journal used to reject apply conflicts.
    pub preimage_dir: Option<PathBuf>,
    /// Select compact first observations when this caller exclusively owns
    /// initialization of a fresh stage. Existing journals/nonempty uppers keep
    /// their format; inspection mounts never select a new format.
    pub compact_preimages: bool,
    /// Paths relative to the overlay root that are absent from the mounted
    /// namespace. Exclusions apply to every lower and the writable upper and
    /// cannot be recreated from inside the mount.
    pub excluded_paths: Vec<PathBuf>,
    pub access_policy: pvisor_overlay_core::FileAccessPolicy,
    /// Optional Run-scoped observation sink; a stand-alone mount leaves it unset.
    pub observation: Option<FsMetrics>,
}

impl OverlayMountConfig {
    pub fn new(
        lower_dirs: Vec<PathBuf>,
        upper_dir: PathBuf,
        work_dir: Option<PathBuf>,
        mountpoint: PathBuf,
    ) -> Self {
        Self {
            apply_target: lower_dirs.last().cloned(),
            baseline_lower: None,
            lower_dirs,
            upper_dir,
            work_dir,
            mountpoint,
            allow_other: false,
            allow_root: false,
            default_permissions: true,
            read_only: false,
            fsname: "pvisor-overlayfs".into(),
            backend: cfg!(target_os = "macos").then(|| "fskit".into()),
            debug: false,
            preimage_dir: None,
            compact_preimages: false,
            excluded_paths: Vec::new(),
            access_policy: Default::default(),
            observation: None,
        }
    }
}

#[derive(Debug)]
pub struct OverlaySession {
    background: Option<BackgroundSession>,
    mountpoint: PathBuf,
}

impl OverlaySession {
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    pub fn has_exited(&self) -> bool {
        self.background
            .as_ref()
            .is_none_or(|session| session.guard.is_finished())
    }

    /// Unmount by dropping the libfuse mount owned by this process.
    pub fn unmount(mut self) -> Result<()> {
        self.unmount_inner()
    }

    fn unmount_inner(&mut self) -> Result<()> {
        if let Some(background) = self.background.take() {
            background
                .unmount()
                .context("unmount FUSE session and stop request loop")?;
            for _ in 0..250 {
                if !is_mountpoint(&self.mountpoint) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            if is_mountpoint(&self.mountpoint) {
                bail!("FUSE mount did not detach: {}", self.mountpoint.display());
            }
        }
        Ok(())
    }
}

impl Drop for OverlaySession {
    fn drop(&mut self) {
        let _ = self.unmount_inner();
    }
}

pub fn mount(config: OverlayMountConfig) -> Result<OverlaySession> {
    #[cfg(target_os = "macos")]
    check_fskit_version(&config)?;
    let (filesystem, mountpoint, options) = prepare(config)?;
    let session = Session::new(filesystem, &mountpoint, &options)
        .with_context(|| format!("mount {}", mountpoint.display()))?;
    let background = BackgroundSession::new(session).context("start FUSE request loop")?;
    log::info!("pvisor-overlayfs mounted at {}", mountpoint.display());
    Ok(OverlaySession {
        background: Some(background),
        mountpoint,
    })
}

pub fn run_foreground(config: OverlayMountConfig) -> Result<()> {
    #[cfg(target_os = "macos")]
    check_fskit_version(&config)?;
    let (filesystem, mountpoint, options) = prepare(config)?;
    log::info!("pvisor-overlayfs mounted at {}", mountpoint.display());
    let mut session = Session::new(filesystem, &mountpoint, &options)
        .with_context(|| format!("mount {}", mountpoint.display()))?;
    session.run().context("FUSE session")?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn check_fskit_version(config: &OverlayMountConfig) -> Result<()> {
    if config.backend.as_deref() != Some("fskit") {
        return Ok(());
    }
    let output = std::process::Command::new("/usr/bin/defaults")
        .args([
            "read",
            "/Library/Filesystems/macfuse.fs/Contents/Info",
            "CFBundleVersion",
        ])
        .output()
        .context("read installed macFUSE version")?;
    if !output.status.success() {
        bail!("cannot read installed macFUSE version; FSKit requires macFUSE >= 5.4.0");
    }
    require_fskit_version(std::str::from_utf8(&output.stdout)?.trim())
}

#[cfg(target_os = "macos")]
fn require_fskit_version(version: &str) -> Result<()> {
    let parts = version
        .split('.')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()?;
    if parts.as_slice() < [5, 4, 0].as_slice() {
        bail!(
            "macFUSE {version} FSKit can corrupt small writes with zero-filled data; macFUSE >= 5.4.0 is required (brew upgrade --cask macfuse)"
        );
    }
    Ok(())
}

fn prepare(mut config: OverlayMountConfig) -> Result<(OverlayFs, PathBuf, Vec<MountOption>)> {
    if config.lower_dirs.is_empty() {
        bail!("lowerdir must list at least one path");
    }
    std::fs::create_dir_all(&config.upper_dir)
        .with_context(|| format!("create upperdir {}", config.upper_dir.display()))?;
    if let Some(work) = &config.work_dir {
        std::fs::create_dir_all(work)
            .with_context(|| format!("create workdir {}", work.display()))?;
    }
    let fskit = config.backend.as_deref() == Some("fskit");
    if let Some(backend) = &config.backend
        && !matches!(backend.as_str(), "kernel" | "fskit")
    {
        bail!("unsupported macFUSE backend: {backend}");
    }
    if fskit && (!config.default_permissions || config.allow_root) {
        bail!(
            "FSKit requires default_permissions and does not support allow_root caller filtering"
        );
    }
    if !fskit {
        std::fs::create_dir_all(&config.mountpoint)
            .with_context(|| format!("create mountpoint {}", config.mountpoint.display()))?;
    }

    config.upper_dir = std::fs::canonicalize(config.upper_dir)?;
    config.work_dir = config
        .work_dir
        .map(std::fs::canonicalize)
        .transpose()
        .context("canonicalize workdir")?;
    config.lower_dirs = config
        .lower_dirs
        .into_iter()
        .map(std::fs::canonicalize)
        .collect::<std::io::Result<Vec<_>>>()
        .context("canonicalize lowerdir")?;
    let mountpoint = if fskit && !config.mountpoint.exists() {
        let parent = config
            .mountpoint
            .parent()
            .context("FSKit mountpoint must have a parent")?;
        let name = config
            .mountpoint
            .file_name()
            .context("FSKit mountpoint must have a final component")?;
        std::fs::canonicalize(parent)?.join(name)
    } else {
        std::fs::canonicalize(&config.mountpoint)?
    };
    if fskit && !mountpoint.starts_with("/Volumes") {
        bail!("macFUSE FSKit mountpoints must be under /Volumes");
    }

    let hidden_from_lower = |lower: &Path, candidate: &Path| {
        candidate.strip_prefix(lower).is_ok_and(|relative| {
            !relative.as_os_str().is_empty()
                && config
                    .excluded_paths
                    .iter()
                    .any(|hidden| relative == hidden || relative.starts_with(hidden))
        })
    };
    for lower in &config.lower_dirs {
        if !lower.is_dir() {
            bail!("lowerdir is not a directory: {}", lower.display());
        }
        let upper_dir = &config.upper_dir;
        let upper_overlaps = (upper_dir.starts_with(lower) && !hidden_from_lower(lower, upper_dir))
            || lower.starts_with(upper_dir);
        let mount_overlaps = (mountpoint.starts_with(lower)
            && !hidden_from_lower(lower, &mountpoint))
            || lower.starts_with(&mountpoint);
        if upper_overlaps || mount_overlaps {
            bail!(
                "lowerdir must not overlap upperdir or mountpoint: {}",
                lower.display()
            );
        }
    }
    {
        let upper_dir = &config.upper_dir;
        let work_dir = &config.work_dir;
        if mountpoint.starts_with(upper_dir) || upper_dir.starts_with(&mountpoint) {
            bail!("upperdir and mountpoint must not overlap");
        }
        if let Some(work) = work_dir {
            if std::fs::metadata(upper_dir)?.dev() != std::fs::metadata(work)?.dev() {
                bail!(
                    "upperdir and workdir must be on the same filesystem: {} and {}",
                    upper_dir.display(),
                    work.display()
                );
            }
            if upper_dir == work {
                bail!("upperdir and workdir must be different directories");
            }
            if mountpoint.starts_with(work)
                || work.starts_with(&mountpoint)
                || config.lower_dirs.iter().any(|lower| {
                    (work.starts_with(lower) && !hidden_from_lower(lower, work))
                        || lower.starts_with(work)
                })
            {
                bail!("workdir must not overlap lowerdir or mountpoint");
            }
        }
    }

    let target = config
        .apply_target
        .clone()
        .or_else(|| config.lower_dirs.last().cloned())
        .ok_or_else(|| anyhow::anyhow!("overlay has no apply target"))?;
    let initialize = if config.compact_preimages && !config.read_only {
        pvisor_overlay_core::OverlayCore::new_for_layout_with_compact_preimages
    } else {
        pvisor_overlay_core::OverlayCore::new_for_layout
    };
    let filesystem = OverlayFs::from_core(initialize(
        pvisor_overlay_core::OverlayLayout::with_baseline(
            config.lower_dirs,
            target,
            config.baseline_lower.as_deref(),
        )?,
        config.upper_dir,
        config.work_dir,
        config.excluded_paths,
        config.preimage_dir,
    )?)?
    .with_private_root(fskit && !config.allow_other)
    .with_read_only(config.read_only)
    .with_access_policy(&config.access_policy)
    .with_observation(config.observation.clone());
    // Access time is not part of a pVisor changeset. Disabling it also avoids
    // macFUSE issuing read-induced SETATTR requests that would otherwise force
    // lower files into the writable upper.
    let mut options = vec![MountOption::FSName(config.fsname), MountOption::NoAtime];
    if config.debug {
        options.push(MountOption::CUSTOM("debug".into()));
    }
    if let Some(backend) = config.backend {
        options.push(MountOption::CUSTOM(format!("backend={backend}")));
    }
    if config.default_permissions {
        options.push(MountOption::DefaultPermissions);
    }
    if config.allow_other {
        options.push(MountOption::AllowOther);
    }
    if config.allow_root {
        options.push(MountOption::AllowRoot);
    }
    if config.read_only {
        options.push(MountOption::RO);
    }
    Ok((filesystem, mountpoint, options))
}

/// Check the mount table without issuing requests to an unresponsive FSKit server.
#[cfg(target_os = "macos")]
pub fn is_mountpoint(path: &Path) -> bool {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count < 0 {
        return true;
    } // Fail closed when checking whether it is safe to clean up.
    let mut mounts = vec![unsafe { std::mem::zeroed::<libc::statfs>() }; count as usize + 8];
    let count = unsafe {
        libc::getfsstat(
            mounts.as_mut_ptr(),
            std::mem::size_of_val(mounts.as_slice()) as i32,
            libc::MNT_NOWAIT,
        )
    };
    if count < 0 {
        return true;
    }
    mounts.iter().take(count as usize).any(|mount| unsafe {
        CStr::from_ptr(mount.f_mntonname.as_ptr()).to_bytes() == path.as_os_str().as_bytes()
    })
}

#[cfg(not(target_os = "macos"))]
fn is_mountpoint(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Some(parent) = path.parent() else {
        return true;
    };
    let Ok(parent_metadata) = std::fs::metadata(parent) else {
        return false;
    };
    metadata.dev() != parent_metadata.dev()
        || (metadata.dev() == parent_metadata.dev() && metadata.ino() == parent_metadata.ino())
}

#[cfg(test)]
mod mount_config_tests {
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires a working host FUSE mount"]
    fn checkpoint_mount_orders_explicit_fsync_and_requires_sealed_completion() {
        use pvisor_overlay_core::{load_preimages, stage};
        let temp = tempfile::tempdir().unwrap();
        let mut config = journal_config(temp.path());
        config.compact_preimages = true;
        let journal = temp.path().join("preimages");
        stage::begin(&journal, Default::default()).unwrap();
        let session = super::mount(config.clone()).unwrap();
        assert_eq!(
            std::fs::read(config.mountpoint.join("value")).unwrap(),
            b"original"
        );
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(config.mountpoint.join("value"))
            .unwrap();
        use std::io::Write;
        (&file).write_all(b"modified").unwrap();
        file.sync_all().unwrap();
        std::fs::File::open(&config.mountpoint)
            .unwrap()
            .sync_all()
            .unwrap();
        assert!(!load_preimages(&journal).unwrap().is_empty());
        assert!(stage::require_sealed(&journal).is_err());
        drop(file);
        session.unmount().unwrap();
        stage::seal(&config.upper_dir, &journal).unwrap();
        stage::require_sealed(&journal).unwrap();
        assert_eq!(
            std::fs::read(config.lower_dirs[0].join("value")).unwrap(),
            b"original"
        );
        assert_eq!(
            std::fs::read(config.upper_dir.join("value")).unwrap(),
            b"modified"
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires a working host FUSE mount"]
    fn compact_mount_roundtrip_preserves_apply_and_read_conflicts() {
        use pvisor_core::overlay::{OverlayRecord, OverlayState, OverlayUpper};
        for conflict in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let mut config = journal_config(temp.path());
            config.compact_preimages = true;
            let lower = config.lower_dirs[0].clone();
            std::fs::write(lower.join("deleted"), b"delete me").unwrap();
            let merged = config.mountpoint.clone();
            let session = mount(config.clone()).unwrap();
            assert_eq!(std::fs::read(merged.join("value")).unwrap(), b"original");
            std::fs::write(merged.join("value"), b"replacement").unwrap();
            std::fs::write(merged.join("created"), b"new bytes").unwrap();
            std::fs::hard_link(merged.join("created"), merged.join("alias")).unwrap();
            std::fs::rename(merged.join("alias"), merged.join("renamed")).unwrap();
            std::fs::remove_file(merged.join("deleted")).unwrap();
            session.unmount().unwrap();
            assert_eq!(std::fs::read(lower.join("value")).unwrap(), b"original");
            assert!(!lower.join("created").exists());
            assert!(
                config
                    .preimage_dir
                    .as_ref()
                    .unwrap()
                    .join("entries/format-v2.json")
                    .exists()
            );
            let before =
                pvisor_overlay_core::load_preimages(config.preimage_dir.as_ref().unwrap()).unwrap();
            assert!(
                before
                    .iter()
                    .any(|p| p.relative_path() == Path::new("value"))
            );
            let session = mount(config.clone()).unwrap();
            assert_eq!(std::fs::read(merged.join("value")).unwrap(), b"replacement");
            std::fs::write(merged.join("after-reopen"), b"second mount").unwrap();
            session.unmount().unwrap();
            let after =
                pvisor_overlay_core::load_preimages(config.preimage_dir.as_ref().unwrap()).unwrap();
            assert!(before.iter().all(|old| after.contains(old)));
            let mut record = OverlayRecord {
                id: "compact-host-roundtrip".into(),
                generation: 0,
                target: lower.clone(),
                baseline_lower: None,
                upper: OverlayUpper {
                    upper_dir: config.upper_dir,
                    work_dir: config.work_dir.unwrap(),
                },
                merged_dir: merged,
                stage_dir: temp.path().to_path_buf(),
                excluded_paths: vec![],
                access_policy: Default::default(),
                auto_apply: false,
                auto_discard: false,
                protect_target: false,
                state: OverlayState::Staged,
            };
            if conflict {
                std::fs::write(lower.join("value"), b"external edit").unwrap();
                assert!(pvisor_overlay_core::apply::apply_overlay(&mut record).is_err());
                assert_eq!(
                    std::fs::read(lower.join("value")).unwrap(),
                    b"external edit"
                );
                assert!(!lower.join("created").exists());
            } else {
                pvisor_overlay_core::apply::apply_overlay(&mut record).unwrap();
                assert_eq!(std::fs::read(lower.join("value")).unwrap(), b"replacement");
                assert_eq!(
                    std::fs::read(lower.join("after-reopen")).unwrap(),
                    b"second mount"
                );
                assert!(!lower.join("deleted").exists());
                assert_eq!(
                    std::fs::metadata(lower.join("created")).unwrap().ino(),
                    std::fs::metadata(lower.join("renamed")).unwrap().ino()
                );
            }
        }
    }

    fn journal_config(root: &Path) -> OverlayMountConfig {
        let lower = root.join("lower");
        std::fs::create_dir(&lower).unwrap();
        std::fs::write(lower.join("value"), b"original").unwrap();
        let mut config = OverlayMountConfig::new(
            vec![lower],
            root.join("upper"),
            Some(root.join("work")),
            root.join("merged"),
        );
        // prepare() validates configuration without mounting either backend.
        config.backend = None;
        config.preimage_dir = Some(root.join("preimages"));
        config
    }

    #[test]
    fn compact_observations_require_explicit_writable_initialization() {
        for (compact, read_only, expected) in [
            (false, false, false),
            (true, false, true),
            (true, true, false),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let mut config = journal_config(temp.path());
            config.compact_preimages = compact;
            config.read_only = read_only;
            let (filesystem, _, _) = prepare(config).unwrap();
            drop(filesystem);
            assert_eq!(
                temp.path()
                    .join("preimages/entries/format-v2.json")
                    .exists(),
                expected,
            );
            assert!(
                pvisor_overlay_core::load_preimages(&temp.path().join("preimages"))
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                std::fs::read(temp.path().join("lower/value")).unwrap(),
                b"original"
            );
        }
    }

    #[test]
    fn compact_selection_preserves_legacy_first_observations() {
        let temp = tempfile::tempdir().unwrap();
        let config = journal_config(temp.path());
        let (filesystem, _, _) = prepare(config.clone()).unwrap();
        drop(filesystem);
        let core = pvisor_overlay_core::OverlayCore::open_existing(
            config.lower_dirs.clone(),
            config.upper_dir.clone(),
            config.work_dir.clone(),
            vec![],
            config.preimage_dir.clone(),
        )
        .unwrap();
        core.observe_read(Path::new("value")).unwrap();
        drop(core);
        let directory = config.preimage_dir.clone().unwrap();
        let before = pvisor_overlay_core::load_preimages(&directory).unwrap();
        assert_eq!(before.len(), 1);
        std::fs::write(temp.path().join("lower/value"), b"external edit").unwrap();
        let mut config = config;
        config.compact_preimages = true;
        let (filesystem, _, _) = prepare(config).unwrap();
        drop(filesystem);
        assert!(!directory.join("entries/format-v2.json").exists());
        assert_eq!(
            pvisor_overlay_core::load_preimages(&directory).unwrap(),
            before
        );
    }

    #[test]
    fn compact_selection_preserves_nonempty_upper_without_migration() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = journal_config(temp.path());
        std::fs::create_dir(&config.upper_dir).unwrap();
        std::fs::write(config.upper_dir.join("pending"), b"staged change").unwrap();
        config.compact_preimages = true;
        let (filesystem, _, _) = prepare(config).unwrap();
        drop(filesystem);
        assert!(
            !temp
                .path()
                .join("preimages/entries/format-v2.json")
                .exists()
        );
        assert_eq!(
            std::fs::read(temp.path().join("upper/pending")).unwrap(),
            b"staged change"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fskit_rejects_versions_with_small_write_corruption() {
        for version in ["5.0.0", "5.3.3", "unknown"] {
            assert!(super::require_fskit_version(version).is_err());
        }
        for version in ["5.4.0", "5.10.0", "6.0.0"] {
            assert!(super::require_fskit_version(version).is_ok());
        }
    }

    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn mount_table_probe_does_not_need_to_access_the_volume() {
        assert!(is_mountpoint(Path::new("/")));
        assert!(!is_mountpoint(Path::new(
            "/Volumes/pvisor-nonexistent-mount-test"
        )));
    }

    #[test]
    fn platform_default_backend_applies_to_mounts() {
        let expected = if cfg!(target_os = "macos") {
            Some("fskit")
        } else {
            None
        };
        let directory = OverlayMountConfig::new(vec![], "upper".into(), None, "merged".into());
        assert_eq!(directory.backend.as_deref(), expected);
    }
}
