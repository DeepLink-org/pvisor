//! Mount table handling: option parsing is pure and cross-platform, the
//! syscall layer is Linux-only (used by the init child).

/// Mount flags parsed out of an option list; anything unrecognized becomes
/// filesystem-specific data (e.g. `size=64m` for tmpfs, `upperdir=` for
/// overlay).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedMountOptions {
    pub flags: libc::c_ulong,
    /// `rbind` requests a recursive bind mount.
    pub recursive: bool,
    pub data: Vec<String>,
}

/// Split an OCI/containerd mount option list into flags and data options.
pub fn parse_mount_options(options: &[String]) -> ParsedMountOptions {
    let mut parsed = ParsedMountOptions::default();
    for option in options {
        match option.as_str() {
            "ro" => parsed.flags |= libc::MS_RDONLY,
            "rw" => {}
            "suid" => parsed.flags &= !libc::MS_NOSUID,
            "nosuid" => parsed.flags |= libc::MS_NOSUID,
            "dev" => parsed.flags &= !libc::MS_NODEV,
            "nodev" => parsed.flags |= libc::MS_NODEV,
            "exec" => parsed.flags &= !libc::MS_NOEXEC,
            "noexec" => parsed.flags |= libc::MS_NOEXEC,
            "sync" => parsed.flags |= libc::MS_SYNCHRONOUS,
            "async" | "atime" | "diratime" => {}
            "noatime" => parsed.flags |= libc::MS_NOATIME,
            "relatime" => parsed.flags |= libc::MS_RELATIME,
            "strictatime" => parsed.flags |= libc::MS_STRICTATIME,
            "nodiratime" => parsed.flags |= libc::MS_NODIRATIME,
            "bind" => parsed.flags |= libc::MS_BIND,
            "rbind" => {
                parsed.flags |= libc::MS_BIND | libc::MS_REC;
                parsed.recursive = true;
            }
            "remount" => parsed.flags |= libc::MS_REMOUNT,
            "private" => parsed.flags |= libc::MS_PRIVATE,
            "shared" => parsed.flags |= libc::MS_SHARED,
            "slave" => parsed.flags |= libc::MS_SLAVE,
            "unbindable" => parsed.flags |= libc::MS_UNBINDABLE,
            "rprivate" => parsed.flags |= libc::MS_REC | libc::MS_PRIVATE,
            "rshared" => parsed.flags |= libc::MS_REC | libc::MS_SHARED,
            "rslave" => parsed.flags |= libc::MS_REC | libc::MS_SLAVE,
            "runbindable" => parsed.flags |= libc::MS_REC | libc::MS_UNBINDABLE,
            _ => parsed.data.push(option.clone()),
        }
    }
    parsed
}

#[cfg(target_os = "linux")]
mod linux {
    use crate::plan::MountPlan;
    use anyhow::{Context, Result};
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    fn cstr(path: &Path) -> std::ffi::CString {
        std::ffi::CString::new(path.as_os_str().as_bytes().to_vec())
            .expect("path contains no interior NUL")
    }

    pub fn mount(
        source: Option<&Path>,
        target: &Path,
        fs_type: Option<&str>,
        flags: libc::c_ulong,
        data: Option<&str>,
    ) -> Result<()> {
        let source = source.map(cstr);
        let fs_type = fs_type.map(|ty| std::ffi::CString::new(ty).expect("fs type has no NUL"));
        let data = data.map(|data| std::ffi::CString::new(data).expect("mount data has no NUL"));
        let ret = unsafe {
            libc::mount(
                source.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
                cstr(target).as_ptr(),
                fs_type.as_ref().map_or(std::ptr::null(), |t| t.as_ptr()),
                flags,
                data.as_ref()
                    .map_or(std::ptr::null(), |d| d.as_ptr().cast()),
            )
        };
        if ret != 0 {
            return Err(std::io::Error::last_os_error()).context(format!(
                "mount {} -> {}",
                source_display(source.as_ref(), fs_type.as_ref()),
                target.display()
            ));
        }
        Ok(())
    }

    fn source_display(
        source: Option<&std::ffi::CString>,
        fs_type: Option<&std::ffi::CString>,
    ) -> String {
        if let Some(fs_type) = fs_type {
            return fs_type.to_string_lossy().to_string();
        }
        source
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    /// Make the whole mount tree private so container mounts do not propagate
    /// back to the host.
    pub fn make_mounts_private() -> Result<()> {
        mount(
            None,
            Path::new("/"),
            None,
            libc::MS_REC | libc::MS_PRIVATE,
            None,
        )
        .context("mark mount tree private")
    }

    fn apply_one(mount_plan: &MountPlan, rootfs: &Path) -> Result<()> {
        let target = rootfs.join(mount_plan.destination.strip_prefix("/")?);
        let parsed = super::parse_mount_options(&mount_plan.options);
        let data = parsed.data.join(",");
        let source = mount_plan.source.as_deref().map(Path::new);
        let directory = if matches!(mount_plan.fs_type.as_str(), "bind" | "rbind") {
            fs::metadata(source.context("bind mount without source")?)?.is_dir()
        } else {
            true
        };
        let _mountpoint = pvisor_overlay_core::sys::prepare_rooted_path(
            rootfs,
            mount_plan.destination.strip_prefix("/")?,
            directory,
            false,
        )
        .with_context(|| format!("prepare mountpoint {}", target.display()))?;
        match mount_plan.fs_type.as_str() {
            "bind" | "rbind" => {
                let source = source.with_context(|| "bind mount without source")?;
                mount(
                    Some(source),
                    &target,
                    None,
                    libc::MS_BIND | libc::MS_REC,
                    None,
                )
                .with_context(|| format!("bind {} -> {}", source.display(), target.display()))?;
                // Re-apply non-bind options (ro, nosuid, ...) in a remount.
                let extra = parsed.flags & !libc::MS_BIND & !libc::MS_REC;
                if extra != 0 || !parsed.data.is_empty() {
                    mount(
                        None,
                        &target,
                        None,
                        libc::MS_BIND | libc::MS_REMOUNT | extra,
                        if parsed.data.is_empty() {
                            None
                        } else {
                            Some(data.as_str())
                        },
                    )
                    .context(format!("remount {}", target.display()))?;
                }
            }
            "overlay" => {
                let source = mount_plan
                    .source
                    .clone()
                    .unwrap_or_else(|| "overlay".to_string());
                mount(
                    Some(Path::new(&source)),
                    &target,
                    Some("overlay"),
                    0,
                    Some(data.as_str()),
                )
                .with_context(|| format!("overlay mount at {}", target.display()))?;
            }
            "proc" | "sysfs" | "cgroup" | "cgroup2" | "mqueue" | "tmpfs" | "devpts" => {
                mount(
                    source.or(Some(Path::new(&mount_plan.fs_type))),
                    &target,
                    Some(&mount_plan.fs_type),
                    parsed.flags,
                    if parsed.data.is_empty() {
                        None
                    } else {
                        Some(data.as_str())
                    },
                )
                .with_context(|| format!("{} mount at {}", mount_plan.fs_type, target.display()))?;
            }
            other => anyhow::bail!("unsupported mount type {other} at {}", target.display()),
        }
        Ok(())
    }

    /// Apply request mounts (snapshotter output), then spec mounts, into the
    /// container mount namespace. `proc` spec mounts are skipped here: they
    /// must be mounted by a process inside the new pid namespace.
    pub fn apply_mounts(mounts: &[MountPlan], rootfs: &Path) -> Result<()> {
        for mount_plan in mounts {
            if !mount_plan.from_request && mount_plan.fs_type == "proc" {
                continue;
            }
            apply_one(mount_plan, rootfs)?;
        }
        Ok(())
    }

    /// Mount a fresh procfs at `<rootfs>/proc`; called from inside the new
    /// pid namespace.
    pub fn mount_proc(rootfs: &Path) -> Result<()> {
        let target = rootfs.join("proc");
        std::fs::create_dir_all(&target).with_context(|| format!("create {}", target.display()))?;
        mount(
            Some(Path::new("proc")),
            &target,
            Some("proc"),
            libc::MS_NOSUID | libc::MS_NOEXEC | libc::MS_NODEV,
            None,
        )
        .context("mount proc")
    }

    /// `pivot_root(".", ".")` dance: move the new root over `/`, detach the
    /// old root, and land in the new root.
    pub fn pivot_root(new_root: &Path) -> Result<()> {
        std::env::set_current_dir(new_root).context("chdir new root")?;
        // Bind the new root onto itself so pivot_root has a parent to move.
        mount(
            Some(new_root),
            new_root,
            None,
            libc::MS_BIND | libc::MS_REC,
            None,
        )
        .context("self bind before pivot")?;
        let ret = unsafe {
            libc::syscall(
                libc::SYS_pivot_root,
                cstr(Path::new(".")).as_ptr(),
                cstr(Path::new(".")).as_ptr(),
            )
        };
        if ret != 0 {
            return Err(std::io::Error::last_os_error()).context("pivot_root");
        } else {
            // The old root is now mounted on top of ".".
            let ret = unsafe { libc::umount2(cstr(Path::new(".")).as_ptr(), libc::MNT_DETACH) };
            if ret != 0 {
                return Err(std::io::Error::last_os_error()).context("detach old root");
            }
        }
        std::env::set_current_dir("/").context("chdir /")?;
        Ok(())
    }

    /// Bind-mount /dev/null over each masked path (runc semantics: reading a
    /// masked directory fails with ENOTDIR). Missing paths are skipped, matching
    /// runc's tolerance for absent kernel paths.
    /// Extract the raw OS error from an anyhow chain (mount syscall errors).
    fn errno_of(error: &anyhow::Error) -> Option<i32> {
        error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
            .find_map(|io_error| io_error.raw_os_error())
    }

    pub fn apply_masked_paths(paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        ensure_minimal_dev_nodes()?;
        let source = Path::new("/dev/null");
        for path in paths {
            let target = Path::new(path);
            if !target.exists() {
                continue;
            }
            if let Err(error) = mount(Some(source), target, None, libc::MS_BIND, None) {
                // Kernel-proc entries that are symlinks or don't support
                // bind mounts are skipped, matching runc's tolerance.
                if errno_of(&error) == Some(libc::ENOENT) || errno_of(&error) == Some(libc::ENOTDIR)
                {
                    continue;
                }
                return Err(error).with_context(|| format!("mask {path}"));
            }
        }
        Ok(())
    }

    /// Self-bind each readonly path and remount it read-only.
    pub fn apply_readonly_paths(paths: &[String]) -> Result<()> {
        for path in paths {
            let target = Path::new(path);
            if !target.exists() {
                continue;
            }
            let bind = mount(Some(target), target, None, libc::MS_BIND, None);
            let mount = || {
                mount(
                    None,
                    target,
                    None,
                    libc::MS_BIND
                        | libc::MS_REMOUNT
                        | libc::MS_RDONLY
                        | libc::MS_NOSUID
                        | libc::MS_NODEV
                        | libc::MS_NOEXEC,
                    None,
                )
            };
            match bind.and_then(|()| mount()) {
                Ok(()) => {}
                Err(error)
                    if errno_of(&error) == Some(libc::ENOENT)
                        || errno_of(&error) == Some(libc::ENOTDIR) =>
                {
                    continue;
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("read-only {path}"));
                }
            }
        }
        Ok(())
    }

    /// Create the minimal device set inside a fresh tmpfs /dev (only when the
    /// root did not bring its own nodes): null, zero, full, random, urandom,
    /// tty and the fd/std{in,out,err} indirections runc provides.
    fn ensure_minimal_dev_nodes() -> Result<()> {
        let dev = Path::new("/dev");
        if dev.join("null").exists() {
            return Ok(());
        }
        let nodes: [(&str, u32, u32); 6] = [
            ("null", 1, 3),
            ("zero", 1, 5),
            ("full", 1, 7),
            ("random", 1, 8),
            ("urandom", 1, 9),
            ("tty", 5, 0),
        ];
        for (name, major, minor) in nodes {
            let path = dev.join(name);
            if path.exists() {
                continue;
            }
            let c_name = std::ffi::CString::new(name).expect("device name");
            let ret = unsafe {
                libc::mknod(
                    c_name.as_ptr(),
                    libc::S_IFCHR | 0o666,
                    libc::makedev(major, minor),
                )
            };
            let _ = path; // silence unused when ret checked below
            if ret != 0 {
                let error = std::io::Error::last_os_error();
                // Missing CAP_MKNOD or a read-only /dev must not mask paths
                // silently differently — surface the failure.
                return Err(error).with_context(|| format!("mknod /dev/{name}"));
            }
        }
        let _ = std::os::unix::fs::symlink("/proc/self/fd", dev.join("fd"));
        let _ = std::os::unix::fs::symlink("/proc/self/fd/0", dev.join("stdin"));
        let _ = std::os::unix::fs::symlink("/proc/self/fd/1", dev.join("stdout"));
        let _ = std::os::unix::fs::symlink("/proc/self/fd/2", dev.join("stderr"));
        Ok(())
    }

    /// Apply `linux.sysctl` entries by writing them under /proc/sys; a write
    /// error fails the container (fail-closed, like runc rejecting
    /// non-namespaced sysctls).
    pub fn apply_sysctls(entries: &[(String, String)]) -> Result<()> {
        for (key, value) in entries {
            let proc_path = format!("/proc/sys/{}", key.replace('.', "/"));
            std::fs::write(&proc_path, value).with_context(|| format!("sysctl {key}={value}"))?;
        }
        Ok(())
    }

    /// Remount the (new) root read-only when the spec asks for it.
    pub fn remount_root_readonly() -> Result<()> {
        mount(
            None,
            Path::new("/"),
            None,
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
            None,
        )
        .or_else(|_| {
            mount(
                None,
                Path::new("/"),
                None,
                libc::MS_REMOUNT | libc::MS_RDONLY,
                None,
            )
        })
        .context("remount root read-only")
    }
}

#[cfg(target_os = "linux")]
pub use linux::{
    apply_masked_paths, apply_mounts, apply_readonly_paths, apply_sysctls, make_mounts_private,
    mount, mount_proc, pivot_root, remount_root_readonly,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flags_and_data_are_separated() {
        let parsed = parse_mount_options(&opts(&["ro", "nosuid", "size=64m", "mode=700"]));
        assert_eq!(parsed.flags, libc::MS_RDONLY | libc::MS_NOSUID);
        assert_eq!(
            parsed.data,
            vec!["size=64m".to_string(), "mode=700".to_string()]
        );
        assert!(!parsed.recursive);
    }

    #[test]
    fn rbind_sets_recursive() {
        let parsed = parse_mount_options(&opts(&["rbind", "nodev"]));
        assert!(parsed.recursive);
        assert!(parsed.flags & libc::MS_BIND != 0);
        assert!(parsed.flags & libc::MS_REC != 0);
        assert!(parsed.flags & libc::MS_NODEV != 0);
    }

    #[test]
    fn propagation_options_map_to_flags() {
        let parsed = parse_mount_options(&opts(&["rprivate", "relatime"]));
        assert_eq!(
            parsed.flags,
            libc::MS_REC | libc::MS_PRIVATE | libc::MS_RELATIME
        );
        assert!(parsed.data.is_empty());
    }

    #[test]
    fn overlay_options_stay_data() {
        let parsed = parse_mount_options(&opts(&["lowerdir=/a:/b", "upperdir=/u", "workdir=/w"]));
        assert_eq!(parsed.flags, 0);
        assert_eq!(parsed.data.len(), 3);
    }
}
