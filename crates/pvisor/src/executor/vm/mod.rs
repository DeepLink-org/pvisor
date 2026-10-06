#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
mod supported;
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
mod unsupported;

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
pub use supported::{VmExecutor, run_internal_if_requested};

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub use unsupported::{VmExecutor, run_internal_if_requested};

pub(crate) mod checkpoint;
#[cfg(target_os = "linux")]
mod cpu;
#[cfg(target_os = "linux")]
mod cpu_qos;
#[cfg(target_os = "linux")]
mod exit_cpu;
#[cfg(target_os = "linux")]
pub use cpu_qos::CpuQosGroup;
#[cfg(target_os = "linux")]
mod memory;
#[cfg(target_os = "linux")]
pub use memory::sample_supervisor_memory;
#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
mod restore_ram;

pub(crate) mod control;
#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
mod pager;
