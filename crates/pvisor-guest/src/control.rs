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
    /// Start a process using the shared launcher. Acceptance returns Started.
    Exec(ExecRequest),
    /// Connect to guest loopback TCP, then exchange framed data and EOF.
    Connect {
        /// Nonzero guest TCP port.
        port: u16,
    },
}

/// An exec request shared by native CLI and containerd adapters.
/// Defaults inherit launch environment, identity and limits. Adapters supplying
/// a complete OCI process must set replace_env and limits explicitly.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExecRequest {
    /// Nonempty executable followed by literal arguments.
    pub argv: Vec<String>,
    /// Overrides, or the complete environment when replace_env is true.
    pub env: BTreeMap<String, String>,
    /// Remove these names before applying env.
    pub unset_env: Vec<String>,
    /// Replace rather than inherit the workload environment.
    pub replace_env: bool,
    /// Absolute cwd; omission inherits the workload cwd. It must already exist.
    pub cwd: Option<PathBuf>,
    /// Guest name/UID and optional group; omission inherits launch identity.
    pub user: Option<String>,
    /// Optional initial PTY size [rows, columns], both nonzero.
    pub tty: Option<[u16; 2]>,
    /// Replace launch limits; Some(empty) requests no explicit rlimits.
    pub limits: Option<BTreeMap<String, (u64, u64)>>,
    /// Optional output write and post-exit drain timeout, in milliseconds (1..=60000).
    /// None waits for output without a deadline. The shim supplies 2000.
    pub output_timeout_ms: Option<u64>,
}

/// Observed exec termination, before frontend-specific status mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecOutcome {
    /// Normal exit code, or 128 + signal when terminated by a signal.
    pub code: i32,
    /// Terminating signal; None distinguishes a normal exit even for code >= 128.
    pub signal: Option<i32>,
    /// False when output failed or the requested drain deadline expired.
    pub output_complete: bool,
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
    /// Exec is running. PID belongs to the guest and must never be signalled on the host.
    Started {
        /// Guest PID associated with this connection's exec.
        pid: u32,
    },
    /// A chunk from descriptor 1 or 2.
    Output {
        /// Descriptor 1 for stdout/TCP or 2 for stderr.
        stream: u8,
        /// Output bytes, no larger than 16 KiB before encoding.
        bytes: Vec<u8>,
    },
    /// Accepted exec terminated; preserves signal and output completion facts.
    ExecExit(ExecOutcome),
    /// Startup rejection status or TCP tunnel EOF.
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
    fn shared_frames_preserve_binary_output_and_started_identity() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &Reply::Started { pid: 42 }).unwrap();
        write_frame(
            &mut bytes,
            &Reply::Output {
                stream: 2,
                bytes: vec![0, 255, 254],
            },
        )
        .unwrap();
        let mut reader = &bytes[..];
        assert!(matches!(
            read_frame::<Reply>(&mut reader).unwrap(),
            Reply::Started { pid: 42 }
        ));
        assert!(
            matches!(read_frame::<Reply>(&mut reader).unwrap(), Reply::Output {stream:2, bytes} if bytes == [0,255,254])
        );
    }

    #[test]
    fn frame_bounds_and_truncation_are_checked_before_payload_allocation() {
        struct HeaderOnly(std::io::Cursor<Vec<u8>>);
        impl Read for HeaderOnly {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                assert!(self.0.position() < 4, "must reject before reading payload");
                self.0.read(out)
            }
        }
        for size in [MAX_FRAME as u32 + 1, u32::MAX] {
            let mut reader = HeaderOnly(std::io::Cursor::new(size.to_be_bytes().to_vec()));
            assert_eq!(
                read_frame::<Reply>(&mut reader).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        for bytes in [&[0][..], &[0, 0, 0][..], &[0, 0, 0, 5, b'{'][..]] {
            assert_eq!(
                read_frame::<Reply>(&mut &bytes[..]).unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
        }
        let value = "x".repeat(MAX_FRAME - 2); // JSON quotes count toward the frame bound.
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &value).unwrap();
        assert_eq!(read_frame::<String>(&mut &bytes[..]).unwrap(), value);
        let mut bytes = Vec::new();
        assert!(write_frame(&mut bytes, &"x".repeat(MAX_FRAME)).is_err());
        assert!(bytes.is_empty());
    }

    #[test]
    fn exec_request_retains_complete_oci_options_and_rejects_unknown_fields() {
        let request = Request::Exec(ExecRequest {
            argv: vec!["sh".into(), "-c".into(), "echo 'spaces'".into()],
            replace_env: true,
            limits: Some(BTreeMap::from([("RLIMIT_NOFILE".into(), (32, 64))])),
            ..Default::default()
        });
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &request).unwrap();
        let Request::Exec(decoded) = read_frame::<Request>(&mut &bytes[..]).unwrap() else {
            panic!()
        };
        assert!(decoded.replace_env);
        assert_eq!(decoded.limits.unwrap()["RLIMIT_NOFILE"], (32, 64));
        assert!(
            serde_json::from_str::<Request>(
                r#"{"operation":"Exec","argv":["true"],"privileged":true}"#
            )
            .is_err()
        );
    }

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
