//! VM egress attachment: compiles the annotation policy and owns the
//! pvisor-overlaynet smoltcp backend for one VM task.
//!
//! The backend terminates every guest TCP flow on the host and re-originates
//! it only when the compiled policy authorizes the destination; the guest's
//! virtio-net device is its only network path, so the enforcement cannot be
//! bypassed from inside the VM (see pvisor-overlaynet's `vm_smoltcp`
//! interception profile).

use std::os::fd::{AsRawFd, FromRawFd, RawFd};

use crate::plan::{ContainerPlan, VmNetworkMode};

/// Host-side egress backend plus the guest-side descriptor handed to the VM
/// runner. Dropping the attachment stops policy enforcement; task deletion
/// owns this lifecycle.
pub struct VmNetworkAttachment {
    backend: pvisor_overlaynet::vm::VmNetwork,
    guest_stream: std::os::unix::net::UnixStream,
}

impl VmNetworkAttachment {
    /// Compile the task's network plan and start the backend. Returns `None`
    /// when the task did not opt into egress.
    pub fn start(plan: &ContainerPlan) -> anyhow::Result<Option<Self>> {
        let network = plan.vm_network_plan().map_err(anyhow::Error::msg)?;
        if network.mode == VmNetworkMode::Off {
            return Ok(None);
        }
        let mut config = pvisor_core::NetworkConfig {
            mode: match network.mode {
                VmNetworkMode::Off => pvisor_core::NetworkMode::NoNetwork,
                VmNetworkMode::Public => pvisor_core::NetworkMode::Public,
                VmNetworkMode::Allowlist => pvisor_core::NetworkMode::Allowlist,
            },
            ..Default::default()
        };
        for (host, port) in &network.allow {
            config.rules.push(pvisor_core::NetworkAccessRule {
                host: host.clone(),
                ports: (*port != 0).then(|| vec![*port]).unwrap_or_default(),
                transports: Vec::new(),
                allow_private_ips: network.allow_private,
            });
        }
        let policy = pvisor_core::NetworkPolicy::compile(&config)?;
        let controller = std::sync::Arc::new(pvisor_core::PolicyControlController);
        let egress = pvisor_overlaynet::EgressRuntime::with_bandwidth_registry(
            policy,
            controller,
            pvisor_overlaynet::BandwidthRegistry::default(),
        );
        let vm_config = pvisor_overlaynet::vm::VmNetworkConfig::new(
            egress,
            pvisor_overlaynet::EgressContext {
                run_id: Some(plan.id.clone()),
                attempt_id: Some("containerd-shim".to_string()),
            },
        );
        let (backend, guest_stream) = pvisor_overlaynet::vm::VmNetwork::start(vm_config)?;
        Ok(Some(Self {
            backend,
            guest_stream,
        }))
    }

    /// Duplicate the guest-side descriptor without `FD_CLOEXEC` so the VM
    /// runner inherits exactly one end. The caller closes the duplicate
    /// after spawning; the attachment keeps the original.
    pub fn guest_fd_for_child(&self) -> std::io::Result<RawFd> {
        // SAFETY: dup on a live descriptor; it either returns a new valid
        // descriptor or -1 with errno set.
        let fd = unsafe { libc::dup(self.guest_stream.as_raw_fd()) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(fd)
    }

    /// Stop the backend and return its interception counters. Closing the
    /// guest stream first lets a backend blocked on socket I/O observe EOF
    /// before its thread joins.
    pub fn shutdown(self) -> anyhow::Result<pvisor_overlaynet::InterceptionSnapshot> {
        let Self {
            backend,
            guest_stream,
        } = self;
        drop(guest_stream);
        backend.shutdown()
    }
}

/// Wrap a runner-inherited descriptor for [`crate::vm::boot_vm`].
///
/// # Safety
/// The descriptor must come from [`VmNetworkAttachment::guest_fd_for_child`]
/// and may be wrapped exactly once.
pub unsafe fn stream_from_fd(fd: RawFd) -> std::os::unix::net::UnixStream {
    unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) }
}
