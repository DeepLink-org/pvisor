//! Shared Unix/TCP endpoint validation and stream I/O.
use anyhow::{Context, bail, ensure};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(super) const TOKEN_ENV: &str = "PVISOR_CACHE_TOKEN";
pub(super) const TIMEOUT: Duration = Duration::from_secs(300);

pub fn default_endpoint() -> anyhow::Result<String> {
    let base = dirs::cache_dir().context("cannot find user cache directory")?;
    Ok(format!(
        "unix://{}",
        base.join("pvisor/cache.sock").display()
    ))
}

pub(super) enum Endpoint {
    Unix(PathBuf),
    Tcp(SocketAddr),
}

pub(super) fn endpoint(value: &str) -> anyhow::Result<Endpoint> {
    if let Some(path) = value.strip_prefix("unix://") {
        ensure!(
            Path::new(path).is_absolute(),
            "Unix socket path must be absolute"
        );
        return Ok(Endpoint::Unix(path.into()));
    }
    if let Some(address) = value.strip_prefix("tcp://") {
        let address: SocketAddr = address
            .parse()
            .context("TCP endpoint requires an IP address and port")?;
        ensure!(
            address.ip().is_loopback(),
            "cache TCP is loopback-only; use an SSH tunnel for remote access"
        );
        return Ok(Endpoint::Tcp(address));
    }
    bail!("expected unix:///absolute/path or tcp://127.0.0.1:PORT")
}

pub(super) enum Stream {
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Unix(s) => s.read(buf),
            Self::Tcp(s) => s.read(buf),
        }
    }
}
impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Unix(s) => s.write(buf),
            Self::Tcp(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Unix(s) => s.flush(),
            Self::Tcp(s) => s.flush(),
        }
    }
}
impl Stream {
    pub(super) fn nodelay(&self) -> std::io::Result<()> {
        // Frames write their prefix and JSON separately. Disable Nagle so small
        // persistent exchanges cannot stall behind the peer's delayed ACK.
        match self {
            Self::Unix(_) => Ok(()),
            Self::Tcp(s) => s.set_nodelay(true),
        }
    }

    pub(super) fn timeouts(&self, timeout: Duration) -> std::io::Result<()> {
        let result = match self {
            Self::Unix(s) => s
                .set_read_timeout(Some(timeout))
                .and_then(|()| s.set_write_timeout(Some(timeout))),
            Self::Tcp(s) => s
                .set_read_timeout(Some(timeout))
                .and_then(|()| s.set_write_timeout(Some(timeout))),
        };
        // Darwin rejects sockopts after shutdown; let I/O drain buffered frames
        // and report EOF/EPIPE instead, preserving response validation and retry.
        #[cfg(target_os = "macos")]
        if !timeout.is_zero()
            && result
                .as_ref()
                .is_err_and(|error| error.raw_os_error() == Some(libc::EINVAL))
        {
            return Ok(());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_update_after_disconnect_preserves_buffered_bytes_and_eof() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        peer.write_all(b"partial frame").unwrap();
        drop(peer);
        let mut stream = Stream::Unix(socket);
        assert!(stream.timeouts(Duration::ZERO).is_err());
        stream.timeouts(Duration::from_millis(50)).unwrap();
        let mut bytes = [0; 13];
        stream.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"partial frame");
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
    }
}
