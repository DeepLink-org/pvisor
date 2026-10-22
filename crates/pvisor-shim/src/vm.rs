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
use crate::plan::{ContainerPlan, guest_config, vm_agent_enabled, vm_ram_dedup_enabled};
use anyhow::{Context, Result};
use pvisor_vm::api::{RamDedupControl, VmConfiguration, VmRuntime};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Guest mount point of the read-only payloads share (the virtio-fs tag
/// matches); shared payload targets symlink into it.
pub const PAYLOADS_TAG: &str = "pvisor-payloads";
/// Guest path the payloads share is mounted at.
pub const PAYLOADS_GUEST_DIR: &str = "/.pvisor-payloads";
/// DAX window for the payloads share: guest address space only, sized to
/// cover a payload directory with headroom.
const PAYLOADS_DAX_WINDOW: usize = 64 * 1024 * 1024;

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
    // Injected payloads become guest sidecars when they run; stage-only
    // entries (entrypoint wrappers) are just staged below. Port bridges ride
    // AF_VSOCK with the same numbering: the guest supervisor relays each
    // bridged vsock port to guest 127.0.0.1:<port>.
    guest.sidecars = plan
        .injections
        .iter()
        .filter(|injection| injection.run)
        .map(|injection| pvisor_guest::SidecarProcess {
            argv: injection.argv.clone(),
            env: injection.env.clone(),
            log: std::path::PathBuf::from(format!("/.pvisor-inject-{}.log", injection.name)),
        })
        .collect();
    guest.bridges = plan
        .injections
        .iter()
        .filter_map(|injection| {
            injection.bridge.map(|port| pvisor_guest::PortBridge {
                vsock_port: u32::from(port),
                tcp_port: port,
            })
        })
        .collect();
    if plan
        .injections
        .iter()
        .any(|injection| injection.share.is_some())
    {
        guest.payloads_dir = Some(std::path::PathBuf::from(PAYLOADS_GUEST_DIR));
    }
    // Pod volume bind mounts re-expressed as virtio-fs shares: a VM guest
    // cannot see CRI mounts, so directory binds (hostPath/NFS mount points,
    // emptyDirs, projected volumes) become shares — read-only volumes ride
    // the physically-immutable share (one host copy for every sandbox),
    // read-write ones a plain passthrough. File binds (kubelet's
    // /etc/hosts and friends) stay no-ops; the guest keeps its rootfs's
    // own files for those paths.
    let volume_shares: Vec<(std::path::PathBuf, std::path::PathBuf, bool)> = plan
        .mounts
        .iter()
        .filter(|mount| mount.fs_type == "bind" && !mount.from_request)
        .filter_map(|mount| {
            let source = std::path::PathBuf::from(mount.source.as_deref()?);
            let read_only = mount.options.iter().any(|option| option == "ro");
            std::fs::metadata(&source)
                .ok()
                .filter(|metadata| metadata.is_dir())
                .map(|_| (source, mount.destination.clone(), read_only))
        })
        .collect();
    guest.shares = volume_shares
        .iter()
        .enumerate()
        .map(
            |(index, (_, destination, read_only))| pvisor_guest::GuestShare {
                tag: format!("pvisor-vol-{index}"),
                path: destination.clone(),
                read_only: *read_only,
            },
        )
        .collect();
    // Re-validate with the sidecars attached so an invalid launch contract
    // fails here (admission already checked the payload catalogue; this
    // guards the serialized handoff as a whole).
    guest.command()?;
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
    for injection in &plan.injections {
        let relative = injection
            .target
            .strip_prefix("/")
            .expect("admission guarantees absolute targets");
        if injection.staged {
            // Delivered through a pod volume's host source; the volume
            // share serves it into the guest and would shadow any rootfs
            // staging anyway.
            continue;
        }
        if let Some(share_dir) = &injection.share {
            // Shared payloads never copy: the target becomes a symlink into
            // the read-only guest mount of the payloads share (one host
            // copy for every sandbox on the node). Admission verified the
            // source sits directly inside the share.
            let guest_path = Path::new(PAYLOADS_GUEST_DIR)
                .join(injection.source.strip_prefix(share_dir).expect("validated"));
            let target = plan.rootfs.join(relative);
            if let Some(parent) = target.parent() {
                let _mountpoint = pvisor_overlay_core::sys::prepare_rooted_path(
                    &plan.rootfs,
                    parent
                        .strip_prefix(&plan.rootfs)
                        .expect("parent stays inside the rootfs"),
                    true,
                    true,
                )
                .with_context(|| {
                    format!(
                        "prepare shared payload parent for {} without following image symlinks",
                        injection.name
                    )
                })?;
            }
            let _ = std::fs::remove_file(&target);
            std::os::unix::fs::symlink(&guest_path, &target).with_context(|| {
                format!(
                    "symlink {} -> {} for payload {}",
                    target.display(),
                    guest_path.display(),
                    injection.name
                )
            })?;
            continue;
        }
        // Same materialization as the agent binary, into the container's
        // writable rootfs layer: the guest supervisor starts it from the
        // sidecar argv. Create-time admission already verified the target
        // was free, so a collision here is unexpected and fails the boot.
        let mut payload =
            pvisor_overlay_core::sys::prepare_rooted_path(&plan.rootfs, relative, false, true)
                .with_context(|| {
                    format!(
                        "stage injection {} without following image symlinks",
                        injection.name
                    )
                })?;
        payload.set_len(0)?;
        std::io::copy(
            &mut std::fs::File::open(&injection.source)
                .with_context(|| format!("open injection source {}", injection.source.display()))?,
            &mut payload,
        )?;
        payload.set_permissions(std::fs::Permissions::from_mode(0o755))?;
    }
    if network.is_some() {
        // The smoltcp backend answers DNS on the synthetic router; without
        // this file the guest keeps resolving through whatever resolver the
        // image ships, which has no route off the VM. Written into the
        // container's own writable layer, like the container tier's resolv.conf.
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
    // One read-only DAX share serves every VM's staged payloads from a
    // single host copy; targets inside the rootfs symlink into it.
    if let Some(injection) = plan.injections.iter().find(|i| i.share.is_some()) {
        let share_dir = injection.share.as_ref().expect("checked above");
        vm.immutable_filesystem(PAYLOADS_TAG, share_dir, PAYLOADS_DAX_WINDOW)?;
    }
    for (index, (source, _, read_only)) in volume_shares.iter().enumerate() {
        let tag = format!("pvisor-vol-{index}");
        if *read_only {
            // Minimum window: the guest mounts dax=never, so the window is
            // reserved address space only, never coupled to dataset size.
            vm.immutable_filesystem(&tag, source, 2 * 1024 * 1024)?;
        } else {
            vm.filesystem(&tag, source, 0)?;
        }
    }
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
    // Bridged ports: libkrun listens on each host unix socket and tunnels
    // connections into the guest's AF_VSOCK listener of the same number
    // (the pod-netns proxies connect here; see the bridge proxies).
    for injection in &plan.injections {
        if let Some(port) = injection.bridge {
            vm.vsock_port(
                u32::from(port),
                crate::agent::bridge_socket_path(&plan.id, port),
                true,
            )?;
        }
    }
    if let Some(stream) = network {
        vm.network(stream, pvisor_overlaynet::vm::VM_MAC)?;
    }
    if let Some((stdin, stdout, stderr)) = stdio {
        // Route the krun-stdio console ports to the task's own descriptors:
        // the runner's process stderr then stays free for VMM diagnostics
        // (the io.pvisor.vm.log file) without capturing workload stderr.
        vm.task_stdio(stdin, stdout, stderr)?;
    }
    // `io.pvisor.vm.ram-dedup=true` opts the task's guest RAM into host KSM.
    // The advice only registers mappings as mergeable; merging still needs
    // the host KSM scanner, which the shim never configures. Non-fatal on
    // failure, matching pVisor's VM executor: an unavailable advisor must
    // not take the task down.
    let ram_dedup = vm_ram_dedup_enabled(&plan.annotations);
    vm.run(move |handle| {
        if ram_dedup {
            match handle.advise_ram_dedup() {
                Ok(report) => eprintln!(
                    "VM RAM dedup advice installed: accepted_bytes={} \
                     (registration only, not merged bytes or savings), mappings={:?}",
                    report.accepted_bytes, report.mappings
                ),
                Err(error) => {
                    eprintln!("VM RAM dedup advice failed; continuing VM: {error}")
                }
            }
        }
        Ok(())
    })?;
    Ok(0)
}
