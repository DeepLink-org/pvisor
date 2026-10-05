//! Bounded read-only node observations. Scopes overlap and must not be added.
use anyhow::{Context, ensure};
use pvisor_core::memory::SystemMemory;
use std::{collections::BTreeMap, fs::File, io::Read, path::Path};

fn read(path: &Path, limit: u64) -> anyhow::Result<String> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    bounded(file, limit)
}
fn bounded(file: File, limit: u64) -> anyhow::Result<String> {
    let mut text = String::new();
    file.take(limit + 1).read_to_string(&mut text)?;
    ensure!(
        text.len() as u64 <= limit,
        "node memory interface exceeds read limit"
    );
    Ok(text)
}
fn number(text: &str) -> anyhow::Result<u64> {
    let text = text.trim();
    ensure!(
        !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()),
        "invalid node memory counter"
    );
    text.parse().context("node memory counter overflow")
}

pub fn sample_system_memory() -> anyhow::Result<SystemMemory> {
    ensure!(
        cfg!(target_os = "linux"),
        "system memory observations require Linux"
    );
    system_from_proc(Path::new("/proc"))
}

fn system_from_proc(proc: &Path) -> anyhow::Result<SystemMemory> {
    let text = read(&proc.join("meminfo"), 64 * 1024)?;
    let names = [
        "MemTotal:",
        "MemAvailable:",
        "MemFree:",
        "Cached:",
        "Buffers:",
        "Slab:",
        "SwapTotal:",
        "SwapFree:",
    ];
    let mut counters = BTreeMap::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.first().is_some_and(|key| names.contains(key)) {
            ensure!(
                fields.len() == 3 && fields[2] == "kB",
                "invalid system memory unit"
            );
            let bytes = number(fields[1])?
                .checked_mul(1024)
                .context("system memory counter overflow")?;
            ensure!(
                counters.insert(fields[0], bytes).is_none(),
                "duplicate system memory counter"
            );
        }
    }
    let get = |key| {
        counters
            .get(key)
            .copied()
            .context("missing system memory counter")
    };
    let result = SystemMemory {
        host_boot_id: read(&proc.join("sys/kernel/random/boot_id"), 128)?
            .trim()
            .into(),
        total_bytes: get("MemTotal:")?,
        available_bytes: get("MemAvailable:")?,
        free_bytes: get("MemFree:")?,
        cached_bytes: get("Cached:")?,
        buffers_bytes: get("Buffers:")?,
        slab_bytes: get("Slab:")?,
        swap_total_bytes: get("SwapTotal:")?,
        swap_free_bytes: get("SwapFree:")?,
    };
    result.validate()?;
    Ok(result)
}

pub fn sample_cgroup_memory() -> anyhow::Result<pvisor_core::memory::CgroupMemory> {
    #[cfg(target_os = "linux")]
    {
        cgroup::sample_from_proc(Path::new("/proc"), true)
    }
    #[cfg(not(target_os = "linux"))]
    {
        anyhow::bail!("cgroup memory observations require Linux")
    }
}

#[cfg(target_os = "linux")]
mod cgroup {
    use super::*;
    use crate::admission::cgroup_paths;
    use pvisor_core::memory::{CgroupMemory, MemoryLimit};
    use std::{
        ffi::CStr,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{
                ffi::OsStrExt,
                fs::{MetadataExt, OpenOptionsExt},
            },
        },
    };

    fn interface(directory: &File, name: &CStr) -> anyhow::Result<String> {
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("open cgroup {}", name.to_string_lossy()));
        }
        bounded(unsafe { File::from_raw_fd(fd) }, 64 * 1024)
    }
    fn optional_counter(directory: &File, name: &CStr) -> anyhow::Result<Option<u64>> {
        match interface(directory, name) {
            Ok(value) => Ok(Some(number(&value)?)),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
    fn limit(text: &str) -> anyhow::Result<MemoryLimit> {
        if text.trim() == "max" {
            Ok(MemoryLimit::Unlimited)
        } else {
            Ok(MemoryLimit::Bytes(number(text)?))
        }
    }
    fn counters(text: &str, max: usize) -> anyhow::Result<BTreeMap<String, u64>> {
        let mut result = BTreeMap::new();
        for line in text.lines() {
            let fields: Vec<_> = line.split_whitespace().collect();
            ensure!(
                fields.len() == 2
                    && !fields[0].is_empty()
                    && fields[0].len() <= 64
                    && fields[0]
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "invalid cgroup memory counter key"
            );
            ensure!(
                result.len() < max
                    && result
                        .insert(fields[0].into(), number(fields[1])?)
                        .is_none(),
                "duplicate or excessive cgroup memory counters"
            );
        }
        ensure!(!result.is_empty(), "empty cgroup memory counters");
        Ok(result)
    }
    pub(super) fn sample_from_proc(
        proc: &Path,
        verify_filesystem: bool,
    ) -> anyhow::Result<CgroupMemory> {
        let membership = read(&proc.join("self/cgroup"), 64 * 1024)?;
        let (path, mount) = cgroup_paths(
            &membership,
            &read(&proc.join("self/mountinfo"), 16 * 1024 * 1024)?,
        )?;
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&path)?;
        let identity = directory.metadata()?;
        if verify_filesystem {
            let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
            ensure!(
                unsafe { libc::fstatfs(directory.as_raw_fd(), fs.as_mut_ptr()) } == 0,
                "cannot verify cgroup filesystem"
            );
            ensure!(
                unsafe { fs.assume_init() }.f_type == 0x6367_7270,
                "memory scope is not a cgroup v2 filesystem"
            );
        }
        let result = CgroupMemory {
            directory: String::from_utf8(path.as_os_str().as_bytes().to_vec())
                .context("cgroup directory is not UTF-8")?,
            device: identity.dev(),
            inode: identity.ino(),
            current_bytes: number(&interface(&directory, c"memory.current")?)?,
            peak_bytes: optional_counter(&directory, c"memory.peak")?,
            max: limit(&interface(&directory, c"memory.max")?)?,
            high: limit(&interface(&directory, c"memory.high")?)?,
            swap_current_bytes: optional_counter(&directory, c"memory.swap.current")?,
            stat: counters(&interface(&directory, c"memory.stat")?, 256)?,
            events: counters(&interface(&directory, c"memory.events")?, 32)?,
        };
        // Resolve again instead of comparing all mountinfo bytes: ordinary
        // concurrent FUSE mounts need not invalidate the cgroup identity.
        ensure!(
            membership == read(&proc.join("self/cgroup"), 64 * 1024)?,
            "cgroup membership changed during observation"
        );
        let (after, after_mount) = cgroup_paths(
            &membership,
            &read(&proc.join("self/mountinfo"), 16 * 1024 * 1024)?,
        )?;
        ensure!(
            after == path && after_mount == mount,
            "cgroup mount changed during observation"
        );
        let after = std::fs::metadata(&after)?;
        ensure!(
            after.dev() == identity.dev() && after.ino() == identity.ino(),
            "cgroup directory changed during observation"
        );
        result.validate()?;
        Ok(result)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn synthetic_mount_mapping_preserves_scope_units_and_fails_on_bad_interfaces() {
            let temp = tempfile::tempdir().unwrap();
            let proc = temp.path().join("proc");
            let mount = temp.path().join("cgroup mount");
            let leaf = mount.join("leaf");
            std::fs::create_dir_all(proc.join("self")).unwrap();
            std::fs::create_dir_all(&leaf).unwrap();
            let put = |path: &Path, text: &str| std::fs::write(path, text).unwrap();
            put(&proc.join("self/cgroup"), "0::/host/leaf\n");
            put(
                &proc.join("self/mountinfo"),
                &format!(
                    "1 0 0:1 /host {} rw - cgroup2 cgroup rw\n",
                    mount.display().to_string().replace(' ', "\\040")
                ),
            );
            for (name, value) in [
                ("memory.current", "900"),
                ("memory.max", "800"),
                ("memory.high", "700"),
                (
                    "memory.stat",
                    "anon 300\nfile 400\nkernel 200\nslab 50\npgfault 123\nfuture_stat 42\n",
                ),
                ("memory.events", "high 3\noom_kill 1\n"),
            ] {
                put(&leaf.join(name), value);
            }
            // Synthetic files test parsing only; production always verifies
            // cgroup2 with fstatfs before accepting these counters.
            let valid = sample_from_proc(&proc, false).unwrap();
            assert_eq!(valid.directory, leaf.to_str().unwrap());
            assert_eq!(valid.current_bytes, 900);
            assert_eq!(valid.max, MemoryLimit::Bytes(800));
            assert_eq!(valid.stat["pgfault"], 123);
            assert_eq!(valid.stat["future_stat"], 42);
            assert!(valid.peak_bytes.is_none() && valid.swap_current_bytes.is_none());
            assert!(sample_from_proc(&proc, true).is_err());
            put(&leaf.join("memory.max"), "max\n");
            put(&leaf.join("memory.peak"), "1000\n");
            put(&leaf.join("memory.swap.current"), "7\n");
            let newer = sample_from_proc(&proc, false).unwrap();
            assert_eq!(newer.max, MemoryLimit::Unlimited);
            assert_eq!(newer.peak_bytes, Some(1000));
            assert_eq!(newer.swap_current_bytes, Some(7));
            for bad in [
                "anon 1\nfile 2\nfile 3\n",
                "anon 1\n",
                "anon -1\nfile 2\n",
                "anon 18446744073709551616\nfile 2\n",
                "anon 1 kB\nfile 2\n",
            ] {
                put(&leaf.join("memory.stat"), bad);
                assert!(sample_from_proc(&proc, false).is_err(), "{bad}");
            }
            put(&leaf.join("memory.stat"), "anon 1\nfile 2\n");
            std::fs::remove_file(leaf.join("memory.current")).unwrap();
            assert!(sample_from_proc(&proc, false).is_err());
            put(&leaf.join("memory.current"), "1");
            std::fs::remove_file(leaf.join("memory.stat")).unwrap();
            std::os::unix::fs::symlink(leaf.join("memory.current"), leaf.join("memory.stat"))
                .unwrap();
            assert!(sample_from_proc(&proc, false).is_err());
            put(&proc.join("self/cgroup"), "0::/../../hidden\n");
            assert!(sample_from_proc(&proc, false).is_err());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synthetic_meminfo_requires_exact_units_required_fields_and_valid_boot_identity() {
        let temp = tempfile::tempdir().unwrap();
        let proc = temp.path();
        std::fs::create_dir_all(proc.join("sys/kernel/random")).unwrap();
        std::fs::write(
            proc.join("sys/kernel/random/boot_id"),
            "00000000-0000-0000-0000-000000000001\n",
        )
        .unwrap();
        let valid = "MemTotal: 1000 kB\nMemAvailable: 400 kB\nMemFree: 100 kB\nCached: 200 kB\nBuffers: 5 kB\nSlab: 10 kB\nSwapTotal: 20 kB\nSwapFree: 12 kB\nUnknown: 1\n";
        std::fs::write(proc.join("meminfo"), valid).unwrap();
        let sample = system_from_proc(proc).unwrap();
        assert_eq!(sample.total_bytes, 1024 * 1000);
        assert_eq!(sample.cached_bytes, 1024 * 200);
        for bad in [
            valid.replace("400 kB", "400 MB"),
            valid.replace("400 kB", "1001 kB"),
            valid.replace("400", "18446744073709551615"),
            valid.replace("SwapFree: 12 kB\n", ""),
            format!("{valid}Cached: 200 kB\n"),
        ] {
            std::fs::write(proc.join("meminfo"), &bad).unwrap();
            assert!(system_from_proc(proc).is_err(), "{bad}");
        }
        std::fs::write(proc.join("meminfo"), valid).unwrap();
        std::fs::write(proc.join("sys/kernel/random/boot_id"), "invalid").unwrap();
        assert!(system_from_proc(proc).is_err());
    }
}
