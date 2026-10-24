//! Bounded, owned stream TX frames. Guest descriptors can be released after
//! copying; no guest RAM view survives a nonblocking socket write.
use std::os::fd::RawFd;

use super::backend::WriteError;
use super::{MAX_BUFFER_SIZE, VNET_HDR_LEN};
use crate::devices::virtio::queue::DescriptorChain;
use vm_memory::{Bytes, GuestMemoryMmap};

const BATCH: usize = 32;
const PREFIX: usize = VNET_HDR_LEN - 4;

pub(super) struct TxBatch {
    frames: [Vec<u8>; BATCH],
    lengths: [usize; BATCH],
    count: usize,
    first: usize,
    offset: usize,
}

impl TxBatch {
    pub(super) fn new() -> Self {
        Self {
            frames: std::array::from_fn(|_| Vec::with_capacity(2048)),
            lengths: [0; BATCH],
            count: 0,
            first: 0,
            offset: 0,
        }
    }

    pub(super) fn full(&self) -> bool {
        self.count == BATCH
    }

    pub(super) fn empty(&self) -> bool {
        self.count == 0
    }

    pub(super) fn push(
        &mut self,
        mem: &GuestMemoryMmap,
        head: DescriptorChain<'_>,
    ) -> Result<(), &'static str> {
        let frame = &mut self.frames[self.count];
        let mut written = 0usize;
        let _memory_access = head.memory_access.clone();
        let mut next = Some(head);
        while let Some(descriptor) = next {
            let start = written;
            let end = start
                .checked_add(descriptor.len as usize)
                .filter(|end| *end <= MAX_BUFFER_SIZE)
                .ok_or("oversized TX frame")?;
            if descriptor.is_write_only() {
                return Err("write-only TX descriptor");
            }
            if end > frame.capacity() {
                frame.reserve_exact(end - frame.len());
            }
            if end > frame.len() {
                frame.resize(end, 0);
            }
            mem.read_slice(&mut frame[start..end], descriptor.addr)
                .map_err(|_| "invalid TX memory")?;
            written = end;
            next = descriptor.next_descriptor();
        }
        if written <= VNET_HDR_LEN {
            return Err("missing TX payload");
        }
        let length = (written - VNET_HDR_LEN) as u32;
        frame[PREFIX..VNET_HDR_LEN].copy_from_slice(&length.to_be_bytes());
        self.lengths[self.count] = written;
        self.count += 1;
        Ok(())
    }

    pub(super) fn flush(&mut self, fd: RawFd) -> Result<(), WriteError> {
        // The stream backend configures O_NONBLOCK before handing us its fd.
        while self.first < self.count {
            let mut vectors: [libc::iovec; BATCH] = std::array::from_fn(|_| libc::iovec {
                iov_base: std::ptr::null_mut(),
                iov_len: 0,
            });
            for (index, frame) in self.frames[self.first..self.count].iter().enumerate() {
                let skip = PREFIX + if index == 0 { self.offset } else { 0 };
                vectors[index] = libc::iovec {
                    iov_base: frame[skip..].as_ptr().cast_mut().cast(),
                    iov_len: self.lengths[self.first + index] - skip,
                };
            }
            // Every iovec borrows a live owned frame for the duration of sendmsg.
            // The kernel reads payloads and only writes the msghdr's output fields.
            let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
            message.msg_iov = vectors.as_mut_ptr();
            message.msg_iovlen = (self.count - self.first) as _;
            #[cfg(target_os = "linux")]
            let flags = libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL;
            #[cfg(not(target_os = "linux"))]
            let flags = libc::MSG_DONTWAIT;
            let written = unsafe { libc::sendmsg(fd, &message, flags) };
            if written < 0 {
                match nix::Error::last() {
                    nix::Error::EINTR => continue,
                    nix::Error::EAGAIN => return Err(WriteError::NothingWritten),
                    nix::Error::EPIPE => return Err(WriteError::ProcessNotRunning),
                    error => return Err(WriteError::Internal(error)),
                }
            }
            if written == 0 {
                return Err(WriteError::ProcessNotRunning);
            }
            let mut remaining = written as usize;
            while self.first < self.count {
                let length = self.lengths[self.first] - PREFIX - self.offset;
                if remaining < length {
                    self.offset += remaining;
                    break;
                }
                remaining -= length;
                self.first += 1;
                self.offset = 0;
            }
        }
        self.count = 0;
        self.first = 0;
        self.offset = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::{fd::AsRawFd, unix::net::UnixStream};

    #[test]
    fn short_writes_retain_batch_cursor_without_replaying_prefixes_or_payloads() {
        let (sender, mut receiver) = UnixStream::pair().unwrap();
        sender.set_nonblocking(true).unwrap();
        receiver.set_nonblocking(true).unwrap();
        let bytes: libc::c_int = 1024;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    sender.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&bytes as *const libc::c_int).cast(),
                    std::mem::size_of_val(&bytes) as _,
                )
            },
            0
        );
        let mut batch = TxBatch::new();
        let mut expected = Vec::new();
        for sequence in 0..BATCH {
            let payload = vec![sequence as u8; if sequence % 2 == 0 { 1514 } else { 65535 }];
            let prefix = (payload.len() as u32).to_be_bytes();
            expected.extend_from_slice(&prefix);
            expected.extend_from_slice(&payload);
            batch.frames[sequence].resize(VNET_HDR_LEN, 0);
            batch.frames[sequence][PREFIX..].copy_from_slice(&prefix);
            batch.frames[sequence].extend_from_slice(&payload);
            batch.lengths[sequence] = batch.frames[sequence].len();
            batch.count += 1;
        }
        let mut arrived = Vec::new();
        let mut blocked = false;
        for _ in 0..10000 {
            match batch.flush(sender.as_raw_fd()) {
                Ok(()) => {}
                Err(WriteError::NothingWritten) => blocked = true,
                error => panic!("unexpected flush: {error:?}"),
            }
            let mut bytes = [0; 997];
            loop {
                match receiver.read(&mut bytes) {
                    Ok(0) => panic!("unexpected stream EOF"),
                    Ok(length) => arrived.extend_from_slice(&bytes[..length]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    error => panic!("unexpected receive: {error:?}"),
                }
            }
            if batch.empty() {
                break;
            }
        }
        assert!(blocked);
        assert!(batch.empty());
        assert_eq!(arrived, expected);
        assert!(batch.flush(sender.as_raw_fd()).is_ok());
    }
}
