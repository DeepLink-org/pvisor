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
pub(crate) mod idle;
#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
mod pager;
#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
pub(crate) use pager::{start_diagnostics, start_with_pool};

impl VmExecutor {
    /// Prepare this process for later native VM launches before starting threads.
    ///
    /// On Linux, transiently duplicates an owned `/dev/null` descriptor above the
    /// runner's reserved child descriptors, then closes both descriptors. This
    /// grows the current process's descriptor table without retaining handles,
    /// changing existing descriptors or modifying resource limits. Calling before
    /// creating worker threads avoids the first shared-table expansion on launch.
    /// Other platforms return success without changing process state.
    ///
    /// This optional, repeatable preparation does not start a VM or establish
    /// virtualization support. It is safe after threads start, but may then pay
    /// the expansion cost. It does not prepare a subsequently exec'd process or
    /// guarantee capacity under later descriptor pressure. An I/O error leaves
    /// existing descriptors intact; callers may continue without the optimization.
    pub fn prepare_process() -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let mut span = crate::startup_profile::Span::start("vm.reserve_runner_fds");
            let source = std::fs::File::open("/dev/null")?;
            let reserved = supported::runner_fd(source.as_raw_fd())?;
            drop(reserved);
            span.success(None);
        }
        Ok(())
    }
}
