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
    /// Connect to guest loopback TCP, then exchange framed data and EOF.
    Connect {
        /// Nonzero guest TCP port.
        port: u16,
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
}
