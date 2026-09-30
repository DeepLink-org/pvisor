//! The libkrun microVM executor (feature `vm`, Linux only).
//!
//! Boots one VM per task: the container rootfs (materialized from the
//! snapshotter mounts by the internal VM runner) is shared read-write over
//! virtio-fs as `/dev/root`, the workload stdio rides the virtio-console
//! (wired to the task FIFOs/PTY by the runner), and the workload itself is
//! started through a generated helper script — `krun_set_exec` serializes
//! argv through the kernel command line without escaping, so only the
//! quote-free helper path crosses that boundary (the same pattern pVisor's
//! VM executor uses).
//!
//! The guest kernel comes from libkrunfw on the host (same deployment
//! requirement as `/dev/kvm`). `krun_start_enter` blocks until the guest
//! init exits and returns its exit code.

use crate::agent::{AGENT_GUEST_PATH, AGENT_VSOCK_PORT};
use crate::plan::{
    ContainerPlan, guest_init_script_path, render_guest_init_script_with, vm_agent_enabled,
};
use anyhow::{Context, Result};
use std::ffi::CString;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Write the guest init helper into the container rootfs and boot the VM.
/// Returns the guest exit code.
pub fn boot_vm(plan: &ContainerPlan) -> Result<i32> {
    let config = plan.vm_config();

    let script_guest = guest_init_script_path(&plan.id);
    let host_script = plan.rootfs.join(script_guest.trim_start_matches('/'));
    let agent = vm_agent_enabled(&plan.annotations);
    write_executable(
        &host_script,
        &render_guest_init_script_with(&plan.process, agent),
    )
    .with_context(|| format!("write guest init helper {}", host_script.display()))?;
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

    let tag = cstring("/dev/root")?;
    let root = path_cstring(&plan.rootfs)?;
    krun_check(
        unsafe { krun::krun_add_virtiofs(ctx, tag.as_ptr(), root.as_ptr()) },
        "krun_add_virtiofs",
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

    let workdir = cstring("/")?;
    krun_check(
        unsafe { krun::krun_set_workdir(ctx, workdir.as_ptr()) },
        "krun_set_workdir",
    )?;

    // Empty argv/envp: everything argument-shaped lives in the helper.
    let program = cstring(&script_guest)?;
    let argv = [std::ptr::null::<libc::c_char>()];
    let envp = [std::ptr::null::<libc::c_char>()];
    krun_check(
        unsafe { krun::krun_set_exec(ctx, program.as_ptr(), argv.as_ptr(), envp.as_ptr()) },
        "krun_set_exec",
    )?;

    let code = krun::krun_start_enter(ctx);
    if code < 0 {
        anyhow::bail!("krun_start_enter failed with errno {}", -code);
    }
    Ok(code)
}

fn write_executable(path: &Path, content: &str) -> Result<()> {
    std::fs::write(path, content)?;
    make_executable(path)
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
