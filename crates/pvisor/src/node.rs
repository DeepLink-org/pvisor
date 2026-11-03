//! Client pins for caller-provided same-user, same-host resource services.
//! pVisor does not start a node server. Losing an external owner is not
//! transparent recovery for live FUSE-backed VMs.
use crate::environment_snapshot::Compatibility;
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

const MAX_FRAME: usize = 64 * 1024;
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    request: Request,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Image {
        handle: String,
        manifest_digest: String,
    },
    Ram {
        store: PathBuf,
        snapshot_id: String,
        compatibility: Compatibility,
        filesystem_pool: Option<PathBuf>,
    },
    Stats,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Response {
    Pinned {
        path: PathBuf,
        manifest_digest: Option<String>,
    },
    Stats {
        active_pins: usize,
        live_owners: usize,
        max_owners: usize,
        warm_owners: usize,
        cache_bytes: usize,
        cache_limit: usize,
        cache_misses: usize,
    },
    Error {
        message: String,
    },
}

/// The socket must remain open until the native runner is reaped.
pub struct Pin {
    path: PathBuf,
    digest: Option<String>,
    _session: UnixStream,
}
impl Pin {
    pub fn rootfs(&self) -> &Path {
        &self.path
    }
    pub fn manifest_digest(&self) -> &str {
        self.digest.as_deref().unwrap_or("")
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    fn acquire(socket: &Path, request: Request) -> anyhow::Result<Self> {
        let mut session = connect(socket)?;
        write_frame(
            &mut session,
            &Envelope {
                version: VERSION,
                request,
            },
        )?;
        match read_frame(&mut session)? {
            Response::Pinned {
                path,
                manifest_digest,
            } => Ok(Self {
                path,
                digest: manifest_digest,
                _session: session,
            }),
            Response::Error { message } => anyhow::bail!("node resource preparation: {message}"),
            _ => anyhow::bail!("invalid node pin response"),
        }
    }
    pub fn image(socket: &Path, handle: &str, digest: &str) -> anyhow::Result<Self> {
        Self::acquire(
            socket,
            Request::Image {
                handle: handle.into(),
                manifest_digest: digest.into(),
            },
        )
    }
    pub fn ram(
        socket: &Path,
        store: &Path,
        snapshot_id: &str,
        compatibility: Compatibility,
        filesystem_pool: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        Self::acquire(
            socket,
            Request::Ram {
                store: store.into(),
                snapshot_id: snapshot_id.into(),
                compatibility,
                filesystem_pool,
            },
        )
    }
}
fn connect(socket: &Path) -> anyhow::Result<UnixStream> {
    let stream = UnixStream::connect(socket).context("connect node resource service")?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(stream)
}
pub fn stats(socket: &Path) -> anyhow::Result<serde_json::Value> {
    let mut stream = connect(socket)?;
    write_frame(
        &mut stream,
        &Envelope {
            version: VERSION,
            request: Request::Stats,
        },
    )?;
    let response = read_frame(&mut stream)?;
    if let Response::Error { message } = response {
        anyhow::bail!("{message}");
    }
    Ok(serde_json::to_value(response)?)
}
fn write_frame(stream: &mut impl Write, value: &impl Serialize) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= MAX_FRAME, "node frame too large");
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}
fn read_frame(stream: &mut impl Read) -> anyhow::Result<Response> {
    let mut header = [0; 4];
    stream.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    ensure!(
        length > 0 && length <= MAX_FRAME,
        "invalid node frame length"
    );
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Cursor, os::unix::net::UnixListener};

    #[test]
    fn ram_pin_keeps_external_owner_connected_until_drop() {
        // macOS's default temporary root can exceed sockaddr_un's path limit.
        let root = tempfile::Builder::new()
            .prefix("pvisor-node-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = root.path().join("node.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (acquired, check_pin) = std::sync::mpsc::channel();
        let (connected, checked_pin) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut session, _) = listener.accept().unwrap();
            session
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut header = [0; 4];
            session.read_exact(&mut header).unwrap();
            let mut bytes = vec![0; u32::from_be_bytes(header) as usize];
            session.read_exact(&mut bytes).unwrap();
            let envelope: Envelope = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(envelope.version, VERSION);
            let Request::Ram {
                store,
                snapshot_id,
                compatibility,
                filesystem_pool,
            } = envelope.request
            else {
                panic!("expected restored RAM pin");
            };
            assert_eq!(store, Path::new("/store"));
            assert_eq!(snapshot_id, "snapshot");
            assert_eq!(compatibility.host_boot, "boot");
            assert_eq!(filesystem_pool.as_deref(), Some(Path::new("/pool")));
            write_frame(
                &mut session,
                &Response::Pinned {
                    path: "/ram".into(),
                    manifest_digest: None,
                },
            )
            .unwrap();
            check_pin.recv_timeout(Duration::from_secs(5)).unwrap();
            session.set_nonblocking(true).unwrap();
            let mut byte = [0];
            assert_eq!(
                session.read(&mut byte).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
            session.set_nonblocking(false).unwrap();
            connected.send(()).unwrap();
            assert_eq!(session.read(&mut byte).unwrap(), 0);
        });
        let pin = Pin::ram(
            &socket,
            Path::new("/store"),
            "snapshot",
            Compatibility {
                host_boot: "boot".into(),
                build: "build".into(),
                firmware: "firmware".into(),
                profile: "profile".into(),
            },
            Some("/pool".into()),
        )
        .unwrap();
        assert_eq!(pin.path(), Path::new("/ram"));
        acquired.send(()).unwrap();
        checked_pin.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(pin);
        server.join().unwrap();
    }

    #[test]
    fn bounded_frames_reject_invalid_lengths_and_preserve_owner_errors() {
        for length in [0, MAX_FRAME as u32 + 1, u32::MAX] {
            assert!(read_frame(&mut Cursor::new(length.to_be_bytes())).is_err());
        }
        let mut bytes = Vec::new();
        write_frame(
            &mut bytes,
            &Response::Error {
                message: "owner unavailable".into(),
            },
        )
        .unwrap();
        assert!(matches!(
            read_frame(&mut Cursor::new(bytes)).unwrap(),
            Response::Error { message } if message == "owner unavailable"
        ));
        let mut bytes = Vec::new();
        assert!(write_frame(&mut bytes, &"x".repeat(MAX_FRAME)).is_err());
        assert!(
            bytes.is_empty(),
            "oversized frames must not publish a header"
        );
    }
}
