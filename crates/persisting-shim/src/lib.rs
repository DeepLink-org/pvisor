//! containerd Runtime v2 shim for pVisor.
//!
//! The binary name (`containerd-shim-pvisor-v2`) follows the containerd
//! runtime-v2 discovery rule for the runtime type `io.containerd.pvisor.v2`.
//! Pure planning and state logic is cross-platform so it stays unit-testable
//! on development hosts; everything that touches Linux container primitives
//! is gated to `target_os = "linux"`.

pub mod caps;
pub mod cgroup;
pub mod plan;
pub mod spec;
pub mod state;

#[cfg(target_os = "linux")]
pub mod child;
#[cfg(target_os = "linux")]
pub mod fifo;
#[cfg(target_os = "linux")]
pub mod mount;
#[cfg(target_os = "linux")]
pub mod service;
#[cfg(all(target_os = "linux", feature = "vm"))]
pub mod vm;

/// Runtime type under which containerd discovers this shim.
pub const RUNTIME_TYPE: &str = "io.containerd.pvisor.v2";

/// Exit code the container init child uses when `execve` itself fails,
/// matching the OCI runtime convention.
pub const EXEC_FAILURE_EXIT_CODE: i32 = 127;
