//! Resolve the frontend-only `auto` selector before passing paths to the runtime.
use anyhow::{Context, ensure};
use std::path::{Path, PathBuf};

pub(super) fn resolve_pool(selector: PathBuf) -> anyhow::Result<PathBuf> {
    if selector != Path::new("auto") {
        return Ok(selector);
    }
    super::host_service::resolve_memory_pool()
}

// Only the frontend starts user-wide services. Job owners delegate over their
// private bootstrap channel so request descendant cleanup cannot kill the pool.
pub(super) fn resolve_auto_pool() -> anyhow::Result<PathBuf> {
    let (executable, _) = crate::companions::find("daemon")?
        .context("pvisor-daemon is not installed beside pvisor; required by memory-pool auto")?;
    // The companion owns discovery, peer authentication, version negotiation,
    // serialization and readiness. Explicit paths never invoke the companion.
    let output = std::process::Command::new(executable)
        .arg("resolve-memory-pool")
        .stdin(std::process::Stdio::null())
        .output()
        .context("start or discover same-user pvisor daemon")?;
    ensure!(
        output.status.success(),
        "memory-pool auto: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let socket: PathBuf = serde_json::from_slice(&output.stdout)
        .context("invalid pvisor-daemon memory-pool response")?;
    ensure!(
        socket.is_absolute(),
        "daemon returned a non-absolute pool socket"
    );
    Ok(socket)
}

#[cfg(test)]
mod tests {
    #[test]
    fn explicit_pool_selector_does_not_discover_or_start_services() {
        for path in ["pool.sock", "./auto", "/unavailable/pool.sock"] {
            let path = std::path::PathBuf::from(path);
            assert_eq!(super::resolve_pool(path.clone()).unwrap(), path);
        }
    }
}
