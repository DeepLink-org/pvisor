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
        };
        let files = control_files(&plan);
        assert_eq!(
            files,
            vec![
                ("pids.max", "128".to_string()),
                ("memory.max", "max".to_string()),
                ("cpu.max", "500000 100000".to_string()),
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

/// Classic-BPF filter enforcing `linux.resources.devices` rules on cgroup
/// v2 (runc-compatible semantics: first matching rule wins, default deny).
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
        let program = build_device_program(rules);
        // Safety: the attr buffers are plain bytes passed to the bpf(2) syscall.
        unsafe {
            let license = b"GPL\0";
            let mut load_attr = [0u8; 64];
            load_attr[0..4].copy_from_slice(&(15u32).to_ne_bytes()); // BPF_PROG_TYPE_CGROUP_DEVICE
            // program is flattened sock_filter instructions (8 bytes each).
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
                let log_buf = [0u8; 65536];
                load_attr[24..28].copy_from_slice(&1u32.to_ne_bytes()); // log_level
                load_attr[28..32].copy_from_slice(&(log_buf.len() as u32).to_ne_bytes());
                load_attr[32..40].copy_from_slice(&(log_buf.as_ptr() as u64).to_ne_bytes());
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

    /// Emit the rule chain as eBPF instructions.
    fn build_device_program(rules: &[crate::plan::DeviceRulePlan]) -> Vec<u8> {
        // eBPF register conventions for cgroup device programs:
        // r1 = ctx (in), r0 = return value (0 deny / 1 allow).
        const R1: u8 = 1;
        const R2: u8 = 2;
        const R0: u8 = 0;

        const LDX_W: u8 = 0x61; // BPF_LDX | BPF_W | BPF_MEM
        const ALU_AND_K: u8 = 0x54; // BPF_ALU | BPF_AND | BPF_K
        const ALU_RSH_K: u8 = 0x74; // BPF_ALU | BPF_RSH | BPF_K
        const ALU_MOV_K: u8 = 0xb4; // BPF_ALU | BPF_MOV | BPF_K
        const JMP_JNE_K: u8 = 0x55; // BPF_JMP | BPF_JNE | BPF_K
        const JMP_EXIT: u8 = 0x95; // BPF_JMP | BPF_EXIT

        const ACC_READ: u32 = 1 << 1;
        const ACC_WRITE: u32 = 1 << 2;
        const ACC_MKNOD: u32 = 1 << 0;
        const DEV_BLOCK: u32 = 1 << 0;
        const DEV_CHAR: u32 = 1 << 1;

        /// One eBPF instruction: code, regs (src<<4|dst), off (s16, LE),
        /// imm (s32, LE).
        fn insn(code: u8, regs: u8, off: i16, imm: i32) -> [u8; 8] {
            let mut raw = [0u8; 8];
            raw[0] = code;
            raw[1] = regs;
            raw[2..4].copy_from_slice(&off.to_le_bytes());
            raw[4..8].copy_from_slice(&imm.to_le_bytes());
            raw
        }

        // Track JNE instructions that must skip to the next block on
        // mismatch; fix off at block end.
        let mut program: Vec<[u8; 8]> = Vec::new();
        let mut fixups: Vec<usize> = Vec::new();
        for rule in rules {
            let block_start = program.len();
            let _ = block_start;
            // r2 = ctx->access_type
            program.push(insn(LDX_W, (R1 << 4) | R2, 0, 0));
            if let Some(typ) = rule.typ.as_deref() {
                let bit = if typ == "c" { DEV_CHAR } else { DEV_BLOCK };
                program.push(insn(ALU_AND_K, R2, 0, 0xFFFF_i32));
                fixups.push(program.len());
                program.push(insn(JMP_JNE_K, R2, 0, bit as i32));
            }
            let mut access_mask = 0u32;
            for c in rule.access.chars() {
                access_mask |= match c {
                    'r' => ACC_READ,
                    'w' => ACC_WRITE,
                    'm' => ACC_MKNOD,
                    _ => 0,
                };
            }
            // r2 = access bits; reject when any requested bit is outside
            // the rule mask.
            program.push(insn(LDX_W, (R1 << 4) | R2, 0, 0));
            program.push(insn(ALU_RSH_K, R2, 0, 16));
            program.push(insn(ALU_AND_K, R2, 0, (!access_mask & 0xFFFF) as i32));
            fixups.push(program.len());
            program.push(insn(JMP_JNE_K, R2, 0, 0));
            if let Some(major) = rule.major {
                program.push(insn(LDX_W, (R1 << 4) | R2, 4, 0));
                fixups.push(program.len());
                program.push(insn(JMP_JNE_K, R2, 0, major as i32));
            }
            if let Some(minor) = rule.minor {
                program.push(insn(LDX_W, (R1 << 4) | R2, 8, 0));
                fixups.push(program.len());
                program.push(insn(JMP_JNE_K, R2, 0, minor as i32));
            }
            program.push(insn(ALU_MOV_K, R0, 0, i32::from(rule.allow)));
            program.push(insn(JMP_EXIT, 0, 0, 0));
            // All mismatch jumps in this block target the next block start.
            let next_block = program.len() as i32;
            for index in fixups.drain(..) {
                let off = next_block - (index as i32 + 1);
                program[index][2..4].copy_from_slice(&(off as i16).to_le_bytes());
            }
        }
        // Default deny.
        program.push(insn(ALU_MOV_K, R0, 0, 0));
        program.push(insn(JMP_EXIT, 0, 0, 0));
        let mut bytes = Vec::with_capacity(program.len() * 8);
        for raw in program {
            bytes.extend_from_slice(&raw);
        }
        bytes
    }
}
