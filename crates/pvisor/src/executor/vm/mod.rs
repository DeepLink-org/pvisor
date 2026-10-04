#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
mod supported;
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
mod unsupported;

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
pub use supported::{VmExecutor, run_internal_if_requested};

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub use unsupported::{VmExecutor, run_internal_if_requested};

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
#[expect(
    dead_code,
    reason = "Job capture/commit transport is not wired yet; keep the checked implementation until integration"
)]
pub(crate) mod checkpoint;
pub(crate) mod control;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod pager;
