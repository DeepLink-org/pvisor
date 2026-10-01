//! The libkrun microVM executor (feature `vm`, Linux only).
//!
//! Boots one VM per task: the container rootfs (materialized from the
//! snapshotter mounts by the internal VM runner) is shared read-write over
//! virtio-fs as `/dev/root`, the workload stdio rides the virtio-console
//! (wired to the task FIFOs/PTY by the runner), and the workload itself is
//! started directly by the Rust guest supervisor from a shared launch config.
//!
//! Static musl builds embed the guest kernel; other builds load libkrunfw
//! from the host. `krun_start_enter` enters the VMM and terminates the runner
//! with the workload exit code reported by the guest.

use crate::agent::{AGENT_GUEST_PATH, AGENT_VSOCK_PORT};
use crate::plan::{ContainerPlan, guest_config, vm_agent_enabled};
use anyhow::{Context, Result};
use std::ffi::CString;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[cfg(target_env = "musl")]
mod embedded_kernel {
    include!(concat!(env!("OUT_DIR"), "/embedded_kernel.rs"));
}

/// Configure the Rust guest supervisor and boot the VM.
/// Returns the guest exit code.
pub fn boot_vm(plan: &ContainerPlan) -> Result<i32> {
    let config = plan.vm_config();

    let agent = vm_agent_enabled(&plan.annotations);
    let guest_config = serde_json::to_vec(&guest_config(&plan.process, agent)?)?;
    if agent {
        // The static musl shim binary doubles as the guest agent: copy it
        // into the rootfs so exec works without image requirements.
        let agent_host = plan.rootfs.join(AGENT_GUEST_PATH.trim_start_matches('/'));
        std::fs::copy(std::env::current_exe().context("current exe")?, &agent_host)
            .with_context(|| format!("copy agent to {}", agent_host.display()))?;
        make_executable(&agent_host)?;
    }

    let ctx = krun::krun_create_ctx();
    if ctx < 0 {
        anyhow::bail!("krun_create_ctx failed: {ctx}");
    }
    let ctx = ctx as u32;

    krun_check(
        krun::krun_set_vm_config(ctx, config.cpus, config.ram_mib),
        "krun_set_vm_config",
    )?;

    #[cfg(target_env = "musl")]
    krun_check(
        unsafe {
            krun::krun_set_embedded_kernel(
                ctx,
                embedded_kernel::KERNEL.as_ptr(),
                embedded_kernel::KERNEL.len(),
                embedded_kernel::GUEST_ADDR,
                embedded_kernel::ENTRY_ADDR,
            )
        },
        "krun_set_embedded_kernel",
    )?;

    let tag = cstring("/dev/root")?;
    let root = path_cstring(&plan.rootfs)?;
    krun_check(
        unsafe { krun::krun_add_virtiofs(ctx, tag.as_ptr(), root.as_ptr()) },
        "krun_add_virtiofs",
    )?;

    krun_check(
        unsafe {
            krun::krun_fs_add_overlay_file(
                ctx,
                c"/dev/root".as_ptr(),
                c"/.pvisor-guest.json".as_ptr(),
                guest_config.as_ptr(),
                guest_config.len(),
                0o400,
                true,
            )
        },
        "krun_fs_add_overlay_file(guest config)",
    )?;

    // Contexts start with an implicit vsock whose heuristics could let guest
    // sockets escape through the host stack; replace it with an explicit
    // zero-feature device (mirrors pVisor's VM executor).
    krun_check(
        krun::krun_disable_implicit_vsock(ctx),
        "krun_disable_implicit_vsock",
    )?;
    krun_check(krun::krun_add_vsock(ctx, 0), "krun_add_vsock")?;
    // The agent vsock port: libkrun listens on a unix socket in the bundle
    // and proxies host connections into the guest listener.
    let agent_socket = path_cstring(&plan.bundle.join("pvisor-agent.sock"))?;
    krun_check(
        unsafe { krun::krun_add_vsock_port2(ctx, AGENT_VSOCK_PORT, agent_socket.as_ptr(), true) },
        "krun_add_vsock_port2",
    )?;

    let code = krun::krun_start_enter(ctx);
    if code < 0 {
        anyhow::bail!("krun_start_enter failed with errno {}", -code);
    }
    Ok(code)
}

fn make_executable(path: &Path) -> Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

fn cstring(value: &str) -> Result<CString> {
    CString::new(value).with_context(|| format!("{value:?} contains NUL"))
}

fn path_cstring(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_encoded_bytes())
        .with_context(|| format!("path {:?} contains NUL", path.display()))
}

fn krun_check(result: i32, what: &str) -> Result<()> {
    if result < 0 {
        anyhow::bail!("{what} failed with errno {}", -result);
    }
    Ok(())
}
