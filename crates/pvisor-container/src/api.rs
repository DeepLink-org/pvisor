//! Contracts for one guest exec and its input/output lifecycle.
use std::{
    fs::File,
    io::{self, Write},
    os::unix::net::UnixStream,
};

pub use crate::exec::{ExecCancellation, ExecSession};
pub use pvisor_guest::control::{ExecOutcome, ExecRequest};

/// Failure before a guest exec session has been accepted.
#[derive(Debug, thiserror::Error)]
pub enum ExecStartError {
    /// Transport or protocol failure; no successful launch is established.
    #[error(transparent)]
    Transport(#[from] io::Error),
    /// Guest rejected the launch, with an explicit process-style exit code.
    #[error("{message}")]
    Rejected {
        /// Guest's human-readable diagnostic.
        message: String,
        /// Guest exit code, including 126/127 for executable failures.
        code: i32,
    },
}

/// Failure while relaying an accepted exec. Frontends retain their own status mapping.
#[derive(Debug, thiserror::Error)]
pub enum ExecRelayError {
    /// Socket, framing or guest service failure; completion was not received.
    #[error(transparent)]
    Transport(#[from] io::Error),
    /// The caller's stdout/stderr writer failed. The live exec is cancelled.
    #[error("exec output: {0}")]
    Output(#[source] io::Error),
}

/// Single-use exec lifecycle. All IO is blocking; async callers use a worker.
pub trait ExecRuntime: Sized {
    /// Consume a connected control socket and send one exec request. Success
    /// means the guest returned a PID. Startup uses the supplied socket timeouts.
    /// After acceptance, reads and writes allow arbitrary backpressure;
    /// cancellation shuts down the socket to wake a blocked input writer.
    /// On failure the socket is closed; do not retry on that connection.
    fn start(stream: UnixStream, request: ExecRequest) -> Result<Self, ExecStartError>;
    /// Guest PID for task bookkeeping only, never a host signal target.
    fn pid(&self) -> u32;
    /// Create an explicit cancellation handle for this connection. Dropping
    /// the handle does not cancel; shutdown is idempotent and affects the session.
    fn cancellation(&self) -> io::Result<ExecCancellation>;
    /// Consume the session, forward optional input and drain stdout/stderr until
    /// Exit. None closes guest stdin immediately. EOF leaves output connected.
    /// The input File has a single reader; do not concurrently consume aliases.
    /// Idle input workers stop at exit. Transport/output errors cancel the exec.
    fn relay(
        self,
        input: Option<File>,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> Result<ExecOutcome, ExecRelayError>;
}

/// Explicit cancellation of one accepted exec, independent of input EOF.
pub trait CancelExec {
    /// Shut down both socket directions. The guest cancels the live process
    /// or its group according to launch session policy, even after stdin EOF.
    /// Repeated calls are safe.
    fn cancel(&self) -> io::Result<()>;
}
