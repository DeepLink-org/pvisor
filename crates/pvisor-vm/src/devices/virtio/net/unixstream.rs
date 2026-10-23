use nix::sys::socket::{
    AddressFamily, MsgFlags, SockFlag, SockType, UnixAddr, connect, getsockopt, recv, send,
    setsockopt, socket, sockopt,
};
use std::{
    os::fd::{AsRawFd, OwnedFd, RawFd},
    path::PathBuf,
};

use crate::devices::virtio::net::backend::ConnectError;

use super::backend::{NetBackend, ReadError, WriteError};
use super::write_virtio_net_hdr;

/// Each frame the network proxy is prepended by a 4 byte "header".
/// It is interpreted as a big-endian u32 integer and is the length of the following ethernet frame.
const FRAME_HEADER_LEN: usize = 4;

pub struct Unixstream {
    fd: OwnedFd,
    // Bounded read-ahead retains fragmented frames without blocking the device
    // worker, and amortizes recv across coalesced frames.
    receive: Vec<u8>,
    receive_start: usize,
    receive_end: usize,
    // 0 if last write is fully complete, otherwise the length that was written
    last_partial_write_length: usize,
}

impl Unixstream {
    /// Create the backend with a pre-established connection to the userspace network proxy.
    pub fn new(fd: OwnedFd) -> Self {
        if let Err(e) = setsockopt(&fd, sockopt::SndBuf, &(16 * 1024 * 1024)) {
            log::warn!("Failed to increase SO_SNDBUF (performance may be decreased): {e}");
        }

        log::debug!(
            "network proxy socket (fd {fd:?}) buffer sizes: SndBuf={:?} RcvBuf={:?}",
            getsockopt(&fd, sockopt::SndBuf),
            getsockopt(&fd, sockopt::RcvBuf)
        );

        Self {
            fd,
            receive: vec![0; super::MAX_BUFFER_SIZE + FRAME_HEADER_LEN],
            receive_start: 0,
            receive_end: 0,
            last_partial_write_length: 0,
        }
    }

    /// Create the backend opening a connection to the userspace network proxy.
    pub fn open(path: PathBuf) -> Result<Self, ConnectError> {
        let fd = socket(
            AddressFamily::Unix,
            SockType::Stream,
            SockFlag::empty(),
            None,
        )
        .map_err(ConnectError::CreateSocket)?;
        let peer_addr = UnixAddr::new(&path).map_err(ConnectError::InvalidAddress)?;
        connect(fd.as_raw_fd(), &peer_addr).map_err(ConnectError::Binding)?;

        Ok(Self::new(fd))
    }

    fn write_loop(&mut self, buf: &[u8]) -> Result<(), WriteError> {
        let mut bytes_send = 0;

        #[cfg(target_os = "linux")]
        let flags = MsgFlags::MSG_DONTWAIT | MsgFlags::MSG_NOSIGNAL;
        #[cfg(target_os = "macos")]
        let flags = MsgFlags::MSG_DONTWAIT;

        while bytes_send < buf.len() {
            match send(self.fd.as_raw_fd(), &buf[bytes_send..], flags) {
                Ok(size) => bytes_send += size,
                #[allow(unreachable_patterns)]
                Err(nix::Error::EAGAIN | nix::Error::EWOULDBLOCK) => {
                    if bytes_send == 0 {
                        return Err(WriteError::NothingWritten);
                    } else {
                        log::trace!(
                            "Wrote {bytes_send} bytes, but socket blocked, will need try_finish_write() to finish"
                        );

                        self.last_partial_write_length += bytes_send;
                        return Err(WriteError::PartialWrite);
                    }
                }
                Err(nix::Error::EPIPE) => return Err(WriteError::ProcessNotRunning),
                Err(e) => return Err(WriteError::Internal(e)),
            }
        }
        self.last_partial_write_length = 0;
        Ok(())
    }
}

impl NetBackend for Unixstream {
    /// Try to read a frame from the proxy. If no bytes are available reports ReadError::NothingRead
    fn read_frame(&mut self, buf: &mut [u8]) -> Result<usize, ReadError> {
        loop {
            let available = self.receive_end - self.receive_start;
            if available >= FRAME_HEADER_LEN {
                let frame_length = u32::from_be_bytes(
                    self.receive[self.receive_start..self.receive_start + FRAME_HEADER_LEN]
                        .try_into()
                        .unwrap(),
                ) as usize;
                if frame_length == 0
                    || frame_length > buf.len().saturating_sub(super::VNET_HDR_LEN)
                    || frame_length > self.receive.len() - FRAME_HEADER_LEN
                {
                    return Err(ReadError::Internal(nix::Error::EMSGSIZE));
                }
                if available >= FRAME_HEADER_LEN + frame_length {
                    let header = write_virtio_net_hdr(buf);
                    let start = self.receive_start + FRAME_HEADER_LEN;
                    buf[header..header + frame_length]
                        .copy_from_slice(&self.receive[start..start + frame_length]);
                    self.receive_start = start + frame_length;
                    return Ok(header + frame_length);
                }
            }
            if self.receive_start != 0 {
                self.receive
                    .copy_within(self.receive_start..self.receive_end, 0);
                self.receive_end = available;
                self.receive_start = 0;
            }
            #[cfg(target_os = "linux")]
            let flags = MsgFlags::MSG_DONTWAIT | MsgFlags::MSG_NOSIGNAL;
            #[cfg(target_os = "macos")]
            let flags = MsgFlags::MSG_DONTWAIT;
            match recv(
                self.fd.as_raw_fd(),
                &mut self.receive[self.receive_end..],
                flags,
            ) {
                Ok(0) => return Err(ReadError::Internal(nix::Error::ECONNRESET)),
                Ok(size) => self.receive_end += size,
                Err(nix::Error::EAGAIN) => return Err(ReadError::NothingRead),
                Err(nix::Error::EINTR) => continue,
                Err(error) => return Err(ReadError::Internal(error)),
            }
        }
    }

    /// Try to write a frame to the proxy.
    /// (Will mutate and override parts of buf, with a frame header!)
    ///
    /// * `hdr_len` - specifies the size of any existing headers encapsulating the ethernet frame,
    ///   (such as vnet header), that can be overwritten. Must be >= FRAME_HEADER_LEN.
    /// * `buf` - the buffer to write to the proxy, `buf[..hdr_len]` may be overwritten
    ///
    /// If this function returns WriteError::PartialWrite, you have to finish the write using
    /// try_finish_write.
    fn write_frame(&mut self, hdr_len: usize, buf: &mut [u8]) -> Result<(), WriteError> {
        if self.last_partial_write_length != 0 {
            panic!("Cannot write a frame to the proxy, while a partial write is not resolved.");
        }
        assert!(
            hdr_len >= FRAME_HEADER_LEN,
            "Not enough space to write the frame header"
        );
        assert!(buf.len() > hdr_len);
        let frame_length = buf.len() - hdr_len;

        buf[hdr_len - FRAME_HEADER_LEN..hdr_len]
            .copy_from_slice(&(frame_length as u32).to_be_bytes());

        self.write_loop(&buf[hdr_len - FRAME_HEADER_LEN..])?;
        Ok(())
    }

    fn has_unfinished_write(&self) -> bool {
        self.last_partial_write_length != 0
    }

    /// Try to finish a partial write
    ///
    /// If no partial write is required will do nothing and return Ok(())
    ///
    /// * `hdr_len` - must be the same value as passed to write_frame, that caused the partial write
    /// * `buf` - must be same buffer that was given to write_frame, that caused the partial write
    fn try_finish_write(&mut self, hdr_len: usize, buf: &[u8]) -> Result<(), WriteError> {
        if self.last_partial_write_length != 0 {
            let already_written = self.last_partial_write_length;
            log::trace!("Requested to finish partial write");
            self.write_loop(&buf[hdr_len - FRAME_HEADER_LEN + already_written..])?;
            log::debug!("Finished partial write ({already_written}bytes written before)")
        }

        Ok(())
    }

    fn raw_socket_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    #[test]
    fn partial_header_and_body_never_block_and_preserve_every_byte() {
        let (local, mut peer) = UnixStream::pair().unwrap();
        let mut backend = Unixstream::new(local.into());
        let mut output = [0; 2048];
        let mut framed = 1514u32.to_be_bytes().to_vec();
        framed.extend((0..1514).map(|index| (index * 37) as u8));
        for (index, byte) in framed.iter().enumerate() {
            peer.write_all(&[*byte]).unwrap();
            let result = backend.read_frame(&mut output);
            if index + 1 == framed.len() {
                assert_eq!(result.unwrap(), super::super::VNET_HDR_LEN + 1514);
                assert_eq!(
                    &output[super::super::VNET_HDR_LEN..super::super::VNET_HDR_LEN + 1514],
                    &framed[4..]
                );
            } else {
                assert!(matches!(result, Err(ReadError::NothingRead)));
            }
        }
    }

    #[test]
    fn coalesced_frames_and_eof_are_drained_without_spinning() {
        let (local, mut peer) = UnixStream::pair().unwrap();
        let mut backend = Unixstream::new(local.into());
        let mut framed = Vec::new();
        for sequence in 0..32u8 {
            framed.extend_from_slice(&66u32.to_be_bytes());
            framed.extend_from_slice(&[sequence; 66]);
        }
        peer.write_all(&framed).unwrap();
        drop(peer);
        let mut output = [0; 2048];
        for sequence in 0..32u8 {
            assert_eq!(
                backend.read_frame(&mut output).unwrap(),
                super::super::VNET_HDR_LEN + 66
            );
            assert_eq!(
                &output[super::super::VNET_HDR_LEN..super::super::VNET_HDR_LEN + 66],
                &[sequence; 66]
            );
        }
        assert!(matches!(
            backend.read_frame(&mut output),
            Err(ReadError::Internal(nix::Error::ECONNRESET))
        ));
    }

    #[test]
    fn invalid_frame_lengths_fail_without_unbounded_allocation_or_panic() {
        for length in [0, u32::MAX, 2048] {
            let (local, mut peer) = UnixStream::pair().unwrap();
            let mut backend = Unixstream::new(local.into());
            peer.write_all(&length.to_be_bytes()).unwrap();
            assert!(matches!(
                backend.read_frame(&mut [0; 2048]),
                Err(ReadError::Internal(nix::Error::EMSGSIZE))
            ));
        }
    }

    #[test]
    fn read_ahead_compaction_preserves_maximum_frame_and_rejects_truncated_eof() {
        let (local, mut peer) = UnixStream::pair().unwrap();
        let mut backend = Unixstream::new(local.into());
        let mut bytes = Vec::new();
        for _ in 0..16 {
            bytes.extend_from_slice(&1514u32.to_be_bytes());
            bytes.extend_from_slice(&[0x5a; 1514]);
        }
        let large: Vec<u8> = (0..65535).map(|index| (index * 37) as u8).collect();
        bytes.extend_from_slice(&65535u32.to_be_bytes());
        bytes.extend_from_slice(&large);
        bytes.extend_from_slice(&66u32.to_be_bytes());
        bytes.extend_from_slice(&[0; 3]);
        peer.write_all(&bytes).unwrap();
        drop(peer);
        let mut output = vec![0; super::super::MAX_BUFFER_SIZE];
        for _ in 0..16 {
            let length = backend.read_frame(&mut output).unwrap();
            assert_eq!(&output[super::super::VNET_HDR_LEN..length], &[0x5a; 1514]);
        }
        let length = backend.read_frame(&mut output).unwrap();
        assert_eq!(&output[super::super::VNET_HDR_LEN..length], &large);
        assert!(matches!(
            backend.read_frame(&mut output),
            Err(ReadError::Internal(nix::Error::ECONNRESET))
        ));
    }
}
