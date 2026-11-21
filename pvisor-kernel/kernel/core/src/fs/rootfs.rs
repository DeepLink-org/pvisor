// SPDX-License-Identifier: MPL-2.0

//! Root filesystem mounting during boot.
//!
//! Rust native kernel first tries to boot from the initramfs. It checks the path specified by `rdinit`, or
//! `/init` when `rdinit` is not provided. If the selected path is accessible, Rust native kernel runs it as
//! the first userspace process. Otherwise, Rust native kernel boots from the root filesystem.
//!
//! Root filesystem boot mounts the `/dev/root` virtio-fs tag, not a block device.
//! The mounted filesystem replaces the bootstrap root in the initial mount
//! namespace and becomes the root and working directory of the first userspace process. The
//! bootstrap root is then detached. The `root` parameter is required when this boot path is
//! selected, and [`SUPPORTED_ROOTFS_TYPES`] lists the supported filesystem types.
//!
//! The following kernel parameters configure root filesystem boot:
//!
//! - `root` must specify `/dev/root`.
//! - `rootfstype` specifies a comma-separated list of filesystem type candidates. All supported
//!   types are tried when it is not provided.
//! - `ro` and `rw` select read-only or read-write mounting. The root filesystem is read-only by
//!   default. They use last-wins semantics: the last `ro` or `rw` parameter determines the mount
//!   mode.
//! - `init` specifies the init executable on the mounted filesystem. If it is not provided,
//!   Rust native kernel tries `/sbin/init`, `/etc/init`, `/bin/init`, and `/bin/sh` in order.

// Set this module's log prefix for `ostd::log`.
macro_rules! __log_prefix {
    () => {
        "rootfs: "
    };
}

use core::{
    str::FromStr,
    sync::atomic::{AtomicBool, Ordering},
};

use aster_cmdline::parse::ParamStorage;
use spin::once::Once;

use super::vfs::{
    file_system::FsFlags,
    path::{FsPath, Mount, MountNamespace, Path, PathResolver, PerMountFlags},
    registry::{FsAndRoot, FsCreationCtx},
};
use crate::prelude::*;

/// Filesystem types supported for the root filesystem.
pub(crate) static SUPPORTED_ROOTFS_TYPES: &[&str] = &["virtiofs"];

/// Mounts and switches to the root filesystem configured by the kernel command line.
///
/// The filesystem replaces the bootstrap root mount and becomes the resolver's root and working
/// directory.
///
/// Returns an error if `root` is not provided or the root filesystem cannot be opened or mounted.
pub(crate) fn switch_to_rootfs(path_resolver: &mut PathResolver) -> Result<()> {
    let root = ROOT_PATH.get().ok_or_else(|| {
        Error::with_message(Errno::EINVAL, "the `root` parameter was not provided")
    })?;
    let (fs_flags, mount_flags) = rootfs_flags();
    let fs_and_root = open_rootfs(root, fs_flags)?;
    let rootfs_mount = Mount::new_detached(
        fs_and_root,
        mount_flags,
        Arc::downgrade(MountNamespace::get_init_singleton()),
        Some(root.to_string()),
    )?;
    path_resolver.switch_root_for_boot(rootfs_mount);
    println!("[kernel] rootfs is ready (mounted from {})", root);
    Ok(())
}

/// Finds the init program specified for booting from the root filesystem.
///
/// Resolves the path specified by `init` and returns the resolved path together with the original
/// pathname. Returns `Ok(None)` when `init` is not provided, and an error if the configured
/// pathname is invalid or cannot be resolved.
pub(crate) fn find_init(path_resolver: &PathResolver) -> Result<Option<(Path, &'static str)>> {
    let Some(init_path) = INIT_PATH.get().map(String::as_str) else {
        return Ok(None);
    };

    let path = path_resolver.lookup(&FsPath::try_from(init_path)?)?;
    Ok(Some((path, init_path)))
}

fn open_rootfs(root: &str, fs_flags: FsFlags) -> Result<FsAndRoot> {
    if root != "/dev/root" {
        return_errno_with_message!(Errno::ENODEV, "root must name the /dev/root virtio-fs tag");
    }
    let type_name = rootfs_types().first().ok_or_else(|| {
        Error::with_message(Errno::ENODEV, "no supported root filesystem type was selected")
    })?;
    let fs_type = super::vfs::registry::look_up(type_name)
        .ok_or_else(|| Error::with_message(Errno::ENODEV, "virtiofs is unavailable"))?;
    fs_type.get_or_create(&mut FsCreationCtx::from_source(root, fs_flags))
}

fn rootfs_flags() -> (FsFlags, PerMountFlags) {
    let mut fs_flags = FsFlags::empty();
    let mut mount_flags = PerMountFlags::default();
    if ROOT_MOUNT_READ_ONLY.load(Ordering::Relaxed) {
        fs_flags.insert(FsFlags::RDONLY);
        mount_flags.insert(PerMountFlags::RDONLY);
    }
    (fs_flags, mount_flags)
}

fn rootfs_types() -> &'static [&'static str] {
    ROOTFS_TYPES
        .get()
        .map(RootFsTypes::as_slice)
        .unwrap_or(SUPPORTED_ROOTFS_TYPES)
}

static ROOT_PATH: Once<String> = Once::new();
aster_cmdline::define_kv_param!("root", ROOT_PATH);

static INIT_PATH: Once<String> = Once::new();
aster_cmdline::define_kv_param!("init", INIT_PATH);

/// Root filesystem type candidates.
struct RootFsTypes(Vec<&'static str>);

impl RootFsTypes {
    /// Returns the root filesystem type candidates as a slice.
    fn as_slice(&self) -> &[&'static str] {
        self.0.as_slice()
    }
}

impl FromStr for RootFsTypes {
    type Err = core::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let candidates = value
            .split(',')
            .filter(|type_name| !type_name.is_empty())
            .filter_map(|type_name| {
                SUPPORTED_ROOTFS_TYPES
                    .iter()
                    .copied()
                    .find(|fs_type| *fs_type == type_name)
            })
            .collect();

        Ok(Self(candidates))
    }
}

static ROOTFS_TYPES: Once<RootFsTypes> = Once::new();
aster_cmdline::define_kv_param!("rootfstype", ROOTFS_TYPES);

static ROOT_MOUNT_READ_ONLY: AtomicBool = AtomicBool::new(true);

struct SetRootMountReadOnly;
struct SetRootMountReadWrite;

impl ParamStorage for SetRootMountReadOnly {
    type Value = bool;

    fn store_param(&self, value: Self::Value) {
        if value {
            ROOT_MOUNT_READ_ONLY.store(true, Ordering::Relaxed);
        }
    }
}

impl ParamStorage for SetRootMountReadWrite {
    type Value = bool;

    fn store_param(&self, value: Self::Value) {
        if value {
            ROOT_MOUNT_READ_ONLY.store(false, Ordering::Relaxed);
        }
    }
}

static RO_PARAM: SetRootMountReadOnly = SetRootMountReadOnly;
static RW_PARAM: SetRootMountReadWrite = SetRootMountReadWrite;
aster_cmdline::define_flag_param!("ro", RO_PARAM);
aster_cmdline::define_flag_param!("rw", RW_PARAM);

#[cfg(ktest)]
mod tests {
    use ostd::prelude::ktest;

    use super::*;

    #[ktest]
    fn rootfs_candidates_select_only_virtiofs() {
        for (value, expected) in [
            ("virtiofs", &["virtiofs"][..]),
            ("ext2,virtiofs,exfat", &["virtiofs"][..]),
            ("ext2", &[][..]),
            ("exfat", &[][..]),
            ("", &[][..]),
        ] {
            assert_eq!(value.parse::<RootFsTypes>().unwrap().as_slice(), expected);
        }
    }

    #[ktest]
    fn block_roots_are_rejected_before_filesystem_lookup() {
        for root in ["/dev/vda", "/dev/sda", "/", "root"] {
            match open_rootfs(root, FsFlags::empty()) {
                Err(error) => assert_eq!(error.error(), Errno::ENODEV),
                Ok(_) => panic!("unsupported root was mounted"),
            }
        }
    }
}
