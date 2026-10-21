//! Shim-owned control endpoint naming. Guest execution lives in pvisor-guest.
/// Short per-task Unix endpoint for the shared guest control service.
/// The shim owns this socket's lifetime and removes it on task deletion.
pub fn agent_socket_path(task_id: &str) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    task_id.hash(&mut hasher);
    let name = format!("pvisor-agent-{:016x}.sock", hasher.finish());
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
    fn agent_socket_paths_fit_sun_path_and_stay_distinct() {
        // sockaddr_un::sun_path caps a unix socket path at 108 bytes;
        // containerd task ids are 64 hex chars, so bundle-relative paths
        // cannot be used and even hashed names must stay short.
        let a =
            agent_socket_path("87e32039d31fca9c8f74e400f14b624876e2a4876ece1434ecdfd2aa4876e95a");
        let b = agent_socket_path("ctr-provided-id");
        assert_eq!(
            a,
            agent_socket_path("87e32039d31fca9c8f74e400f14b624876e2a4876ece1434ecdfd2aa4876e95a")
        );
        assert_ne!(a, b);
        for path in [&a, &b] {
            assert!(
                path.as_os_str().len() < 108,
                "{} exceeds sun_path",
                path.display()
            );
        }
    }
}
