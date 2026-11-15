//! Dedicated cgroup child. Wire as `pvisor-memory-worker`; never use Python.
#[path = "allocator.rs"]
mod allocator;
#[path = "common.rs"]
mod common;
#[path = "fixture.rs"]
mod fixture;
#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod linux;
#[path = "process.rs"]
mod process;
#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    linux::worker_main()
}
#[cfg(not(target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("Linux cgroup worker is unavailable on this platform")
}
