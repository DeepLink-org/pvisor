//! The pvisor-vm microVM executor (feature `vm`, Linux only).
//!
//! Boots one VM per task: the container rootfs (materialized from the
//! snapshotter mounts by the internal VM runner) is shared read-write over
//! virtio-fs as `/dev/root`, the workload stdio rides the virtio-console
//! (wired to the task FIFOs/PTY by the runner), and the workload itself is
//! started directly by the Rust guest supervisor from a shared launch config.
//!
//! Static musl builds embed the guest kernel; other builds load libkrunfw
//! from the host. `VmRuntime::run` enters the VMM and terminates the runner
//! with the workload exit code reported by the guest.

use crate::agent::{AGENT_GUEST_PATH, AGENT_VSOCK_PORT};
use crate::plan::{ContainerPlan, guest_config, vm_agent_enabled};
use anyhow::{Context, Result};
use pvisor_vm::api::{VmConfiguration, VmRuntime};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Configure the Rust guest supervisor and boot the VM.
/// Returns the guest exit code.
pub fn boot_vm(plan: &ContainerPlan) -> Result<i32> {
    let config = plan.vm_config();

    let agent = vm_agent_enabled(&plan.annotations);
    let guest_config = serde_json::to_vec(&guest_config(&plan.process, agent)?)?;
    if agent {
        // The static musl shim binary doubles as the guest agent: copy it
        // into the rootfs so exec works without image requirements.
        let mut agent_file = pvisor_overlay_core::sys::prepare_rooted_path(
            &plan.rootfs,
            Path::new(AGENT_GUEST_PATH.trim_start_matches('/')),
            false,
            true,
        )
        .context("prepare guest agent without following image symlinks")?;
        agent_file.set_len(0)?;
        std::io::copy(
            &mut std::fs::File::open(std::env::current_exe().context("current exe")?)?,
            &mut agent_file,
        )?;
        agent_file.set_permissions(std::fs::Permissions::from_mode(0o755))?;
    }

    let mut vm = pvisor_vm::api::VmBuilder::new(config.cpus, config.ram_mib)?;

    // Use the shim's default 512 MiB DAX window.
    vm.filesystem("/dev/root", &plan.rootfs, 1 << 29)?;
    vm.virtual_file(
        "/dev/root",
        "/.pvisor-guest.json",
        guest_config,
        0o400,
        true,
    )?;
    vm.vsock_port(
        AGENT_VSOCK_PORT,
        plan.bundle.join("pvisor-agent.sock"),
        true,
    )?;
    vm.run(|_| Ok(()))?;
    Ok(0)
}
