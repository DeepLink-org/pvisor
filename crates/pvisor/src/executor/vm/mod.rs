#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
mod supported;
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
mod unsupported;

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
pub use supported::{VmExecutor, run_internal_if_requested};
#[cfg(not(any(
    all(target_os = "linux", target_env = "musl", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "x86_64")
)))]
pub(crate) use supported::{bundled_firmware_dir, firmware_name};

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub use unsupported::{VmExecutor, run_internal_if_requested};

pub(crate) mod checkpoint;
mod compressed;
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
#[cfg(all(target_os = "linux", target_env = "musl", target_arch = "x86_64"))]
pub(crate) mod embedded_kernel {
    include!(concat!(env!("OUT_DIR"), "/embedded_kernel.rs"));
}
pub(crate) mod control;
#[cfg(not(any(
    all(target_os = "linux", target_env = "musl", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "x86_64")
)))]
pub(crate) mod firmware;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod pager;
