//! containerd Runtime v2 executable entry for Linux hosts.
//!
//! containerd owns the public task/sandbox wire contracts. OCI plans, process
//! handles, mount/cgroup lowering and guest transports are private to this crate.
//! The Rust entry is identical with and without `vm`; unsupported hosts return
//! an error, and Linux tasks requesting an unavailable VM executor are rejected.
//!
//! ```compile_fail
//! use pvisor_shim::plan::ContainerPlan;
//! ```
//! ```compile_fail
//! use pvisor_shim::service::PvisorShim;
//! ```

/// Runtime type used for containerd discovery and RuntimeInfo responses.
pub const RUNTIME_TYPE: &str = "io.containerd.pvisor.v2";

/// Stateless provider of the process entry point; owns no per-task state.
pub struct Shim;

/// Process startup, internal reentry and containerd service lifecycle.
pub trait ShimRuntime {
    /// Run once at executable entry, before creating threads or a Tokio runtime.
    /// Read process arguments/environment and dispatch private child reentry first;
    /// handled child modes may replace or terminate the process with their result.
    /// Otherwise construct the runtime and serve containerd's task/sandbox protocol
    /// until shutdown. `-info` writes protobuf RuntimeInfo and returns without
    /// starting services. Task state and process handles stay owned by the service.
    /// Errors may follow partial setup; retain runtime state and reconcile through
    /// the owning shim. Unsupported hosts return an io::ErrorKind::Unsupported
    /// error without starting children, binding sockets or creating runtime state.
    fn run() -> anyhow::Result<()>;
}
