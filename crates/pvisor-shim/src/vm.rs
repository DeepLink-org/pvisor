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
pub fn boot_vm(
    plan: &ContainerPlan,
    network: Option<std::os::unix::net::UnixStream>,
    stdio: Option<(i32, i32, i32)>,
) -> Result<i32> {
    let config = plan.vm_config();

    let agent = vm_agent_enabled(&plan.annotations);
    let mut guest = guest_config(&plan.process, agent)?;
    // Named console ports (krun-stdin/stdout/stderr) are how non-terminal
    // task IO reaches the runner's FIFOs; their names arrive asynchronously,
    // so PID 1 must wait for every stream that is not a tty. Terminal tasks
    // keep stdio on the console itself.
    guest.stdio_ports = Some(std::array::from_fn(|fd| unsafe {
        libc::isatty(fd as libc::c_int) != 1
    }));
    if network.is_some() {
        // Egress rides pvisor-overlaynet's smoltcp backend: the guest's
        // virtio-net device is its only network path, and every TCP flow is
        // terminated here on the host and re-originated only when the
        // task's policy authorizes it. The supervisor configures the
        // static address without DHCP.
        guest.network = Some(pvisor_guest::NetworkConfig {
            address: pvisor_overlaynet::vm::GUEST_IPV4.octets(),
            gateway: pvisor_overlaynet::vm::ROUTER_IPV4.octets(),
        });
    }
    let guest_config = serde_json::to_vec(&guest)?;
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
    if network.is_some() {
        // The smoltcp backend answers DNS on the synthetic router; without
        // this file the guest keeps resolving through whatever resolver the
        // image ships, which has no route off the VM. Written into the
        // container's own writable layer, like the host path's resolv.conf.
        let mut resolver = pvisor_overlay_core::sys::prepare_rooted_path(
            &plan.rootfs,
            Path::new("etc/resolv.conf"),
            false,
            true,
        )
        .context("prepare guest resolv.conf without following image symlinks")?;
        std::io::Write::write_all(
            &mut resolver,
            b"nameserver 192.0.2.1\noptions timeout:2 attempts:2\n",
        )
        .context("write guest resolv.conf")?;
    }

    let mut vm = pvisor_vm::api::VmBuilder::new(config.cpus, config.ram_mib)?;

    // The rootfs is shared read-write, so it rides plain virtio-fs without a
    // DAX window: writable DAX requires a lifecycle contract the shim does
    // not provide.
    vm.filesystem("/dev/root", &plan.rootfs, 0)?;
    vm.virtual_file(
        "/dev/root",
        "/.pvisor-guest.json",
        guest_config,
        0o400,
        true,
    )?;
    vm.vsock_port(
        AGENT_VSOCK_PORT,
        crate::agent::agent_socket_path(&plan.id),
        true,
    )?;
    if let Some(stream) = network {
        vm.network(stream, pvisor_overlaynet::vm::VM_MAC)?;
    }
    if let Some((stdin, stdout, stderr)) = stdio {
        // Route the krun-stdio console ports to the task's own descriptors:
        // the runner's process stderr then stays free for VMM diagnostics
        // (the io.pvisor.vm.log file) without capturing workload stderr.
        vm.task_stdio(stdin, stdout, stderr)?;
    }
    vm.run(|_| Ok(()))?;
    Ok(0)
}
