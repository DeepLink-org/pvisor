//! Process entry contracts for the installed command-line applications.
//!
//! Embed `pvisor` for runtime APIs. Command parsers, companion discovery and
//! terminal adapters are implementation details, not embeddable services.
//!
//! ```compile_fail
//! use pvisor_cli::cli::RunArgs;
//! ```
//! ```compile_fail
//! use pvisor_cli::companions::find;
//! ```

/// Stateless owner of the installed command-line process entry points.
pub struct Application;

/// Startup contracts shared across platforms and feature selections.
pub trait CommandLine {
    /// Run `pvisor` using the process arguments, environment and standard streams.
    ///
    /// Call once at process entry, before starting threads or an async runtime.
    /// Internal child modes are dispatched before normal argument parsing. This
    /// may replace or exit the process, install signal handlers, start children,
    /// and publish runtime state. Parser and workload exit codes are preserved.
    /// Returned errors may follow partial setup; runtime-owned cleanup and
    /// recovery rules still apply. This is not a reentrant embedding interface.
    fn run() -> anyhow::Result<()>;

    /// Run `pvisor-replay` using the process arguments and standard streams.
    ///
    /// Call once at process entry, before starting threads or an async runtime.
    /// May launch a managed `pvisor` process from the trusted installation and
    /// create replay artifacts. Parser and replay failures may exit the process;
    /// returned errors do not roll back published artifacts. Windows returns an
    /// explicit unsupported error without parsing or starting a replay.
    fn replay() -> anyhow::Result<()>;
}
