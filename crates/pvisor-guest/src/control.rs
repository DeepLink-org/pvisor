//! Host-only guest control messages; process policy belongs to the launch contract.
#![deny(missing_docs)]
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;

/// Explicit virtio-vsock port, separate from workload networking.
pub const CONTROL_PORT: u32 = 1025;
/// Maximum encoded control frame, including streamed output frames.
pub const MAX_FRAME: usize = 1024 * 1024;

/// One operation on the current VM generation. Requests are length-bounded.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "operation", deny_unknown_fields)]
pub enum Request {
    /// Check that the guest supervisor is accepting requests.
    Ping {},
    /// Attach framed input to the main process; explicit EOF closes its stdin.
    Input {},
    /// Signal the main workload's process group; does not signal host processes.
    Stop {
        /// Linux signal number, from 1 through 64.
        signal: i32,
    },
    /// Start another process in this guest. Environment overrides the launch environment.
    Exec {
        /// Executable and arguments; the program must be nonempty.
        argv: Vec<String>,
        /// Overrides merged with the workload launch environment.
        env: BTreeMap<String, String>,
        /// Environment names to remove before applying overrides.
        unset_env: Vec<String>,
        /// Absolute working-directory override; omission inherits launch cwd.
        cwd: Option<PathBuf>,
        /// Guest user/group override; omission inherits the launch identity.
        user: Option<String>,
        /// Optional terminal dimensions, rows followed by columns.
        tty: Option<[u16; 2]>,
    },
    /// Start a tracked exec; replies Started, Output*, Exit. Disconnect kills the
    /// process unless stdin was explicitly closed. Host owns stream retention.
    Start {
        /// Validated launch overrides, sharing the ordinary process launcher.
        launch: ExecLaunch,
    },
    /// Signal a registered child through the PID-1 registry (never a reused PID).
    Signal {
        /// Guest child PID returned by Started.
        pid: u32,
        /// Registration generation returned by Started; stale values fail.
        generation: u64,
        /// Signal in 1..=64, delivered to its managed process group.
        signal: i32,
    },
    /// Resize a registered PTY; rejects ordinary pipes and exited/reused PIDs.
    Resize {
        /// Live guest child ID returned by Started.
        pid: u32,
        /// Registration generation returned by Started.
        generation: u64,
        /// Nonzero terminal rows.
        rows: u16,
        /// Nonzero terminal columns.
        columns: u16,
    },
    /// Execute file operations inside the guest, using guest paths and root identity.
    File {
        /// Read/write stream or metadata operation; see FileRequest.
        request: FileRequest,
    },
    /// Return guest launch binding and actual guest memory/CPU observations.
    Info {},
    /// Cooperatively inspect or trim a registered child's allocator.
    /// Requires ProcessConfig::allocator_reclaim at launch. Only host-CID peers
    /// may request this operation; no executable payload or address is accepted.
    /// PID 1 exchanges a bounded request with a persistent preload worker over
    /// its private socketpair. Original threads continue running; no ptrace,
    /// injected code or application safe point is used. Same-PID exec can retain
    /// the channel; fork descendants do not inherit independent control.
    /// Value replies contain status, pid, allocator and worker_tid. Trim adds
    /// malloc_trim_result and anonymous RSS before/after (null if unavailable).
    /// Guest counters do not measure host physical-memory savings.
    /// Failure replies Failure; disconnecting does not cancel an admitted call.
    ReclaimAllocator {
        /// Live registered guest child PID; PID 1 and unregistered PIDs fail.
        pid: u32,
        /// Registration identity fencing guest PID reuse; stale values fail.
        generation: u64,
        /// False inspects; true calls malloc_trim(0) in the preload worker.
        /// One request executes at a time. Responses have a two-second wait
        /// budget. Any channel/protocol/allocator error retires the channel;
        /// the target is neither stopped nor killed. A timed-out call may still
        /// complete, and cannot safely be canceled. Further requests then fail.
        trim: bool,
    },
    /// Connect to guest loopback TCP, then exchange framed data and EOF.
    Connect {
        /// Nonzero guest TCP port.
        port: u16,
    },
}

/// Native tracked exec overrides. Environment values are literal, never shell-expanded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecLaunch {
    /// Nonempty executable followed by literal arguments.
    pub argv: Vec<String>,
    /// Overrides merged into the launch environment; native transport keys are removed.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Treat `env` as the complete process environment instead of an overlay
    /// onto the launch environment. Callers that already carry a full
    /// environment (OCI exec specs) use this; native transport keys are
    /// still removed and HOME keeps its Docker-equivalent defaulting.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace_env: bool,
    /// Per-exec rlimits keyed by Linux resource name with soft and hard
    /// values, replacing the launch limits entirely. Empty inherits them,
    /// which is the OCI exec default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub limits: BTreeMap<String, (u64, u64)>,
    /// Optional absolute guest cwd; omission inherits workload cwd.
    pub cwd: Option<PathBuf>,
    /// Existing guest identity; omission inherits launch identity.
    pub user: Option<String>,
    /// Optional rows, columns; PTY combines stdout and stderr.
    pub tty: Option<[u16; 2]>,
}

/// Root-identity guest file primitives. Paths must be absolute and NUL-free.
/// Read replies Ready/Output*/Exit; Write consumes Data*/Eof after Ready and
/// replies Value. Failed writes may leave partial content and must not be retried
/// blindly. Metadata/list replies Value; lists are bounded to 1024 entries.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", deny_unknown_fields)]
pub enum FileRequest {
    /// Read an ordinary guest file as bounded Output frames.
    Read {
        /// Absolute guest path.
        path: PathBuf,
    },
    /// Truncate/create and stream bytes. Parent directories must already exist.
    Write {
        /// Absolute guest path.
        path: PathBuf,
        /// Unix permission bits, applied after writing.
        mode: u32,
    },
    /// Observe metadata, following symlinks while reporting their target.
    Stat {
        /// Absolute guest path.
        path: PathBuf,
    },
    /// List immediate entries; no recursive traversal or symlink following.
    List {
        /// Absolute guest directory.
        path: PathBuf,
    },
    /// Create directories recursively.
    MakeDir {
        /// Absolute guest directory.
        path: PathBuf,
        /// Unix permission bits.
        mode: u32,
    },
    /// Remove a file, symlink, or recursively a directory. Root is rejected.
    Remove {
        /// Absolute guest path other than root.
        path: PathBuf,
    },
    /// Rename using guest filesystem semantics.
    Move {
        /// Absolute existing guest source.
        source: PathBuf,
        /// Absolute guest destination.
        destination: PathBuf,
    },
    /// Change permission bits using guest filesystem semantics.
    Chmod {
        /// Absolute guest path.
        path: PathBuf,
        /// Unix permission bits.
        mode: u32,
    },
}

/// Explicit stdin framing avoids interpreting a transport half-close as a VM reset.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "input", deny_unknown_fields)]
pub enum StdinFrame {
    /// A bounded input chunk.
    Data {
        /// Bytes to write to the process or TCP stream, bounded by MAX_FRAME.
        bytes: Vec<u8>,
    },
    /// Close process stdin while retaining the duplex control connection.
    Eof {},
}

/// Guest reply. Output chunks are at most 16 KiB before JSON encoding.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Reply {
    /// The operation was accepted; subsequent frames depend on the request.
    Ready,
    /// Tracked exec accepted. PID remains guest-local and is fenced by the reaper.
    Started {
        /// Guest child process ID.
        pid: u32,
        /// Monotonic registration identity fencing guest PID reuse.
        generation: u64,
    },
    /// Bounded JSON metadata from File, Info or ReclaimAllocator. No host paths or credentials.
    Value {
        /// Operation-specific metadata; stream transfers use Output instead.
        value: serde_json::Value,
    },
    /// A chunk from descriptor 1 or 2.
    Output {
        /// Descriptor 1 for stdout/TCP or 2 for stderr.
        stream: u8,
        /// Output bytes, no larger than 16 KiB before encoding.
        bytes: Vec<u8>,
    },
    /// Exec terminated; contains the process exit code or 128 + signal.
    Exit {
        /// Workload exit code, 128 + signal, or zero for TCP EOF.
        code: i32,
    },
    /// Native operation failed; effects already acknowledged may be partial.
    Failure {
        /// Linux errno, or EIO for errors without an errno.
        errno: i32,
        /// Human-readable detail, never interpreted as a host command.
        message: String,
    },
    /// The operation failed before acceptance.
    Error {
        /// Human-readable failure detail; never interpreted as a host command.
        message: String,
    },
}

/// Write one bounded frame. Failure may follow a partial write; do not retry it.
pub fn write_frame(writer: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "guest control frame exceeds 1 MiB",
        ));
    }
    writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
    writer.write_all(&bytes)
}

/// Read one frame without allocating beyond the protocol bound.
pub fn read_frame<T: serde::de::DeserializeOwned>(reader: &mut impl Read) -> io::Result<T> {
    let mut size = [0; 4];
    reader.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    if size > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "guest control frame exceeds 1 MiB",
        ));
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frames_reject_oversize_and_unknown_operations() {
        assert!(read_frame::<Request>(&mut &((MAX_FRAME + 1) as u32).to_be_bytes()[..]).is_err());
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &Request::Stop { signal: 15 }).unwrap();
        assert!(matches!(
            read_frame::<Request>(&mut &bytes[..]).unwrap(),
            Request::Stop { signal: 15 }
        ));
        assert!(
            serde_json::from_str::<Request>(r#"{"operation":"Ping","host_path":"/"}"#).is_err()
        );
    }

    #[test]
    fn allocator_request_has_no_machine_code_or_address_input() {
        let request = Request::ReclaimAllocator {
            pid: 23,
            generation: 4,
            trim: true,
        };
        let mut frame = Vec::new();
        write_frame(&mut frame, &request).unwrap();
        assert!(matches!(
            read_frame::<Request>(&mut &frame[..]).unwrap(),
            Request::ReclaimAllocator {
                pid: 23,
                generation: 4,
                trim: true
            }
        ));
        assert!(serde_json::from_str::<Request>(
            r#"{"operation":"ReclaimAllocator","pid":23,"generation":4,"trim":true,"shellcode":[204]}"#
        ).is_err());
    }
}
