//! Host-side unix socket paths for the per-task vsock proxies.
//!
//! pvisor-vm proxies host unix sockets into guest AF_VSOCK listeners
//! (`VmConfiguration::vsock_port` with `listen = true`): one for the guest
//! control protocol (exec) and one per port bridge. Unix socket paths are
//! capped at 108 bytes (`sockaddr_un::sun_path`), and containerd bundle
//! paths (prefix plus a 64-byte task id) exceed that, so each socket lives
//! keyed by a hash of the task id. `/run` is preferred: it is root-owned,
//! so unprivileged local users cannot pre-create (squat) the predictable
//! path the way they could in a world-writable temp directory; the temp
//! directory is the fallback for rootless setups without `/run`. Names are
//! derived deterministically from the task id, so the VM runner (creator)
//! and the exec path (connector) agree. Callers own the lifecycle: remove
//! each socket when the task is deleted.

/// Host-side unix socket that pvisor-vm binds as the vsock proxy for the
/// guest control protocol ([`pvisor_guest::control::CONTROL_PORT`]).
pub fn control_socket_path(task_id: &str) -> std::path::PathBuf {
    hashed_socket_path(task_id, "control", None)
}

/// Host end of a VM port bridge: the unix socket libkrun listens on and
/// tunnels into the guest's AF_VSOCK listener for `port`.
pub fn bridge_socket_path(task_id: &str, port: u16) -> std::path::PathBuf {
    hashed_socket_path(task_id, "bridge", Some(port))
}

fn hashed_socket_path(task_id: &str, role: &str, port: Option<u16>) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    task_id.hash(&mut hasher);
    let name = match port {
        Some(port) => format!("pvisor-{role}-{:016x}-{port}.sock", hasher.finish()),
        None => format!("pvisor-{role}-{:016x}.sock", hasher.finish()),
    };
    let directory = if std::path::Path::new("/run").is_dir() {
        std::path::PathBuf::from("/run")
    } else {
        std::env::temp_dir()
    };
    directory.join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_paths_fit_sun_path_and_stay_distinct() {
        // sockaddr_un::sun_path caps a unix socket path at 108 bytes;
        // containerd task ids are 64 hex chars, so bundle-relative paths
        // cannot be used and even hashed names must stay short.
        let long = "87e32039d31fca9c8f74e400f14b624876e2a4876ece1434ecdfd2aa4876e95a";
        let control = control_socket_path(long);
        let bridge = bridge_socket_path(long, 44772);
        assert_eq!(control, control_socket_path(long));
        assert_ne!(control, bridge);
        assert_ne!(control, control_socket_path("ctr-provided-id"));
        for path in [control, bridge] {
            assert!(
                path.as_os_str().len() < 108,
                "{} exceeds sun_path",
                path.display()
            );
        }
    }
}
