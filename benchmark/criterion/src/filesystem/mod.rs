//! Rust subjects for B-FS-TOOLS and B-FS-ENG. No Python measurement adapters.
#[cfg(target_os = "linux")]
mod dax;
#[cfg(target_os = "linux")]
mod layered;
#[cfg(target_os = "linux")]
mod layered_prepare;
#[cfg(target_os = "linux")]
mod native;
#[cfg(target_os = "linux")]
pub(crate) mod support;

use criterion::Criterion;

/// Register only the explicitly selected filesystem protocol.
pub fn register(c: &mut Criterion) {
    if std::env::var("PVISOR_BENCH_FAMILY").as_deref() != Ok("filesystem") {
        return;
    }
    #[cfg(not(target_os = "linux"))]
    panic!("filesystem Criterion protocols require Linux namespace/cgroup support");
    #[cfg(target_os = "linux")]
    match std::env::var("PVISOR_BENCH_ENTRY")
        .unwrap_or_else(|_| "filesystem/tools".into())
        .as_str()
    {
        "filesystem/tools" | "filesystem/tools.md" | "tools" => native::register(c),
        "filesystem/user_workloads" | "filesystem/user_workloads.json" | "user_workloads" => {
            layered::register(c)
        }
        "filesystem/dax_ab" | "filesystem/dax_ab.json" | "dax_ab" => dax::register(c),
        other => panic!("unknown filesystem entry {other}"),
    }
}
