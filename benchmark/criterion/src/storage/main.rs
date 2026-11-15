//! Dedicated worker executable; all sampling remains in the Criterion entry.
#[cfg(target_os = "linux")]
#[path = "mod.rs"]
#[allow(dead_code)] // Worker and benchmark deliberately share protocol code.
mod storage;

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    storage::worker_main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("storage-worker requires Linux namespaces, cgroup v2 and private rootless Docker")
}
