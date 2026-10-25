//! Cgroup v2 paths and values.
//!
//! Limit values are pre-rendered by [`crate::plan`]; this module owns the
//! filesystem layout: where the unified controller is mounted, which files a
//! `CgroupPlan` produces, and how the systemd `slice:prefix:id` notation maps
//! onto directories.

use crate::plan::CgroupPlan;

/// Default mount point of the cgroup v2 unified hierarchy.
pub const UNIFIED_MOUNT: &str = "/sys/fs/cgroup";

/// Files (relative to the cgroup directory) written for a plan, in order.
pub fn control_files(plan: &CgroupPlan) -> Vec<(&'static str, String)> {
    let mut files = Vec::new();
    if let Some(value) = plan.pids_max.as_deref() {
        files.push(("pids.max", value.to_string()));
    }
    if let Some(value) = plan.memory_max.as_deref() {
        files.push(("memory.max", value.to_string()));
    }
    if let Some(value) = plan.cpu_max.as_deref() {
        files.push(("cpu.max", value.to_string()));
    }
    if let Some(value) = plan.cpu_weight.as_deref() {
        files.push(("cpu.weight", value.to_string()));
    }
    if let Some(value) = plan.memory_swap_max.as_deref() {
        files.push(("memory.swap.max", value.to_string()));
    }
    if let Some(value) = plan.memory_oom_group.as_deref() {
        files.push(("memory.oom.group", value.to_string()));
    }
    files
}

/// Absolute directory of the cgroup for a plan, when one is configured.
pub fn cgroup_dir(plan: &CgroupPlan) -> Option<std::path::PathBuf> {
    let path = plan.path.as_deref()?;
    if path.is_empty() {
        return None;
    }
    Some(std::path::Path::new(UNIFIED_MOUNT).join(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::CgroupPlan;

    #[test]
    fn limits_render_as_ordered_files() {
        let plan = CgroupPlan {
            path: Some("burstable.slice/pod1/c1".to_string()),
            pids_max: Some("128".to_string()),
            memory_max: Some("max".to_string()),
            cpu_max: Some("500000 100000".to_string()),
            cpu_weight: None,
            memory_swap_max: Some("0".to_string()),
            memory_oom_group: Some("1".to_string()),
        };
        let files = control_files(&plan);
        assert_eq!(
            files,
            vec![
                ("pids.max", "128".to_string()),
                ("memory.max", "max".to_string()),
                ("cpu.max", "500000 100000".to_string()),
                ("memory.swap.max", "0".to_string()),
                ("memory.oom.group", "1".to_string()),
            ]
        );
        assert_eq!(
            cgroup_dir(&plan),
            Some(std::path::PathBuf::from(
                "/sys/fs/cgroup/burstable.slice/pod1/c1"
            ))
        );
    }

    #[test]
    fn no_path_means_no_directory() {
        let plan = CgroupPlan {
            path: None,
            ..CgroupPlan::default()
        };
        assert!(cgroup_dir(&plan).is_none());
        let empty = CgroupPlan {
            path: Some(String::new()),
            ..CgroupPlan::default()
        };
        assert!(cgroup_dir(&empty).is_none());
    }
}

/// Attach the compiled, ordered device permission policy to a cgroup v2 leaf.
#[cfg(target_os = "linux")]
pub mod device_bpf {
    use std::path::Path;
    ///
    /// The program reads the kernel's `bpf_cgroup_dev_ctx`:
    /// `access_type` = (BPF_DEVCG_ACC_* << 16) | BPF_DEVCG_DEV_* at offset 0,
    /// `major` at 4, `minor` at 8.
    pub fn attach_device_filter(
        dir: &Path,
        rules: &[crate::plan::DeviceRulePlan],
    ) -> anyhow::Result<()> {
        if rules.is_empty() {
            return Ok(());
        }
        let program = crate::device_policy::build_device_program(rules)?;
        // SAFETY: attributes, instructions and license remain alive throughout
        // each syscall. The verifier's output buffer is exclusively writable
        // until the syscall returns; no Rust reference observes it concurrently.
        unsafe {
            let license = b"GPL\0";
            let mut load_attr = [0u8; 64];
            load_attr[0..4].copy_from_slice(&(15u32).to_ne_bytes()); // BPF_PROG_TYPE_CGROUP_DEVICE
            // The program contains native-endian bpf_insn records (8 bytes each).
            load_attr[4..8].copy_from_slice(&((program.len() / 8) as u32).to_ne_bytes());
            load_attr[8..16].copy_from_slice(&(program.as_ptr() as u64).to_ne_bytes());
            load_attr[16..24].copy_from_slice(&(license.as_ptr() as u64).to_ne_bytes());
            let prog_fd = libc::syscall(libc::SYS_bpf, 5u32, load_attr.as_ptr(), load_attr.len());
            let prog_fd = if prog_fd >= 0 {
                prog_fd
            } else {
                // Retry with the verifier log enabled to surface the
                // rejection reason (the logless attempt is the fast path).
                let error = std::io::Error::last_os_error();
                let mut log_buf = [0u8; 65536];
                load_attr[24..28].copy_from_slice(&1u32.to_ne_bytes()); // log_level
                load_attr[28..32].copy_from_slice(&(log_buf.len() as u32).to_ne_bytes());
                load_attr[32..40].copy_from_slice(&(log_buf.as_mut_ptr() as u64).to_ne_bytes());
                let retried =
                    libc::syscall(libc::SYS_bpf, 5u32, load_attr.as_ptr(), load_attr.len());
                if retried >= 0 {
                    retried
                } else {
                    let detail =
                        String::from_utf8_lossy(log_buf.split(|b| *b == 0).next().unwrap_or(&[]));
                    anyhow::bail!("BPF_PROG_LOAD: {error} (verifier: {detail})");
                }
            };
            let cgroup_fd = libc::open(
                cstring(dir).as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            );
            if cgroup_fd < 0 {
                let error = std::io::Error::last_os_error();
                libc::close(prog_fd as i32);
                anyhow::bail!("open cgroup {}: {error}", dir.display());
            }
            let mut attach_attr = [0u8; 16];
            attach_attr[0..4].copy_from_slice(&(cgroup_fd as u32).to_ne_bytes());
            attach_attr[4..8].copy_from_slice(&(prog_fd as u32).to_ne_bytes());
            attach_attr[8..12].copy_from_slice(&(6u32).to_ne_bytes()); // BPF_CGROUP_DEVICE
            let attached =
                libc::syscall(libc::SYS_bpf, 8u32, attach_attr.as_ptr(), attach_attr.len());
            let attach_error = if attached < 0 {
                Some(std::io::Error::last_os_error())
            } else {
                None
            };
            libc::close(cgroup_fd);
            libc::close(prog_fd as i32);
            if let Some(error) = attach_error {
                anyhow::bail!("BPF_PROG_ATTACH {}: {error}", dir.display());
            }
        }
        Ok(())
    }

    fn cstring(path: &Path) -> std::ffi::CString {
        std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .unwrap_or_else(|_| std::ffi::CString::new("/").expect("fallback path"))
    }
}
