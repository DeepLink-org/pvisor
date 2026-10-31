//! Native transport ownership behind the uniform API.
#[cfg(unix)]
pub(crate) type Stream = std::os::unix::net::UnixStream;
#[cfg(windows)]
pub(crate) type Stream = std::fs::File;

impl From<Stream> for crate::api::NetworkStream {
    fn from(inner: Stream) -> Self {
        Self { inner }
    }
}
