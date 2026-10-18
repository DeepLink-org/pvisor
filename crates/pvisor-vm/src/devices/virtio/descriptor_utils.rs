// Copyright 2019 The Chromium OS Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cmp;
use std::collections::VecDeque;
use std::fmt::{self, Display};
use std::io::{self, Read, Write};
use std::mem::{size_of, MaybeUninit};
use std::ops::Deref;
use std::ptr::copy_nonoverlapping;
use std::result;

use crate::devices::virtio::queue::DescriptorChain;
use vm_memory::{
    Address, ByteValued, Bytes, GuestAddress, GuestMemory, GuestMemoryError, GuestMemoryMmap,
    GuestMemoryRegion, Le16, Le32, Le64, VolatileMemory, VolatileMemoryError, VolatileSlice,
};

use super::file_traits::{FileReadWriteAtVolatile, FileReadWriteVolatile};

#[derive(Debug)]
pub enum Error {
    DescriptorChainOverflow,
    FindMemoryRegion,
    GuestMemoryError(GuestMemoryError),
    InvalidChain,
    #[allow(
        clippy::enum_variant_names,
        reason = "Names the wrapped I/O error domain"
    )]
    IoError(io::Error),
    SplitOutOfBounds(usize),
    VolatileMemoryError(VolatileMemoryError),
}

impl Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use self::Error::*;

        match self {
            DescriptorChainOverflow => write!(
                f,
                "the combined length of all the buffers in a `DescriptorChain` would overflow"
            ),
            FindMemoryRegion => write!(f, "no memory region for this address range"),
            GuestMemoryError(e) => write!(f, "descriptor guest memory error: {e}"),
            InvalidChain => write!(f, "invalid descriptor chain"),
            IoError(e) => write!(f, "descriptor I/O error: {e}"),
            SplitOutOfBounds(off) => write!(f, "`DescriptorChain` split is out of bounds: {off}"),
            VolatileMemoryError(e) => write!(f, "volatile memory error: {e}"),
        }
    }
}

pub type Result<T> = result::Result<T, Error>;

impl std::error::Error for Error {}

#[derive(Clone)]
struct DescriptorChainConsumer<'a> {
    memory_access: Option<std::sync::Arc<super::memory_gate::Access>>,
    buffers: VecDeque<VolatileSlice<'a>>,
    bytes_consumed: usize,
}

impl<'a> DescriptorChainConsumer<'a> {
    fn available_bytes(&self) -> usize {
        // This is guaranteed not to overflow because the total length of the chain
        // is checked during all creations of `DescriptorChainConsumer` (see
        // `Reader::new()` and `Writer::new()`).
        self.buffers
            .iter()
            .fold(0usize, |count, vs| count + vs.len())
    }

    fn bytes_consumed(&self) -> usize {
        self.bytes_consumed
    }

    /// Consumes at most `count` bytes from the `DescriptorChain`. Callers must provide a function
    /// that takes a `&[VolatileSlice]` and returns the total number of bytes consumed. This
    /// function guarantees that the combined length of all the slices in the `&[VolatileSlice]` is
    /// less than or equal to `count`.
    ///
    /// # Errors
    ///
    /// If the provided function returns any error then no bytes are consumed from the buffer and
    /// the error is returned to the caller.
    fn consume<F>(&mut self, count: usize, f: F) -> io::Result<usize>
    where
        F: FnOnce(&[VolatileSlice]) -> io::Result<usize>,
    {
        let mut buflen = 0;
        let mut bufs = Vec::with_capacity(self.buffers.len());
        for &vs in &self.buffers {
            if buflen >= count {
                break;
            }

            let len = (count - buflen).min(vs.len());
            bufs.push(vs.subslice(0, len).map_err(io::Error::other)?);
            buflen += len;
        }

        if bufs.is_empty() {
            return Ok(0);
        }

        let bytes_consumed = f(&bufs)?;
        if bytes_consumed > buflen {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "descriptor consumer exceeded offered bytes",
            ));
        }

        // This can happen if a driver tricks a device into reading/writing more data than
        // fits in a `usize`.
        let total_bytes_consumed =
            self.bytes_consumed
                .checked_add(bytes_consumed)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, Error::DescriptorChainOverflow)
                })?;

        let mut rem = bytes_consumed;
        while let Some(vs) = self.buffers.pop_front() {
            if rem < vs.len() {
                // Split the slice and push the remainder back into the buffer list. Safe because we
                // know that `rem` is not out of bounds due to the check and we checked the bounds
                // on `vs` when we added it to the buffer list.
                self.buffers.push_front(vs.offset(rem).unwrap());
                break;
            }

            // No need for checked math because we know that `vs.size() <= rem`.
            rem -= vs.len();
        }

        self.bytes_consumed = total_bytes_consumed;

        Ok(bytes_consumed)
    }

    fn split_at(&mut self, offset: usize) -> Result<DescriptorChainConsumer<'a>> {
        let mut rem = offset;
        let pos = self.buffers.iter().position(|vs| {
            if rem < vs.len() {
                true
            } else {
                rem -= vs.len();
                false
            }
        });

        if let Some(at) = pos {
            let mut other = self.buffers.split_off(at);

            if rem > 0 {
                // There must be at least one element in `other` because we checked
                // its `size` value in the call to `position` above.
                let front = other.pop_front().expect("empty VecDeque after split");
                self.buffers
                    .push_back(front.subslice(0, rem).map_err(Error::VolatileMemoryError)?);
                other.push_front(front.offset(rem).map_err(Error::VolatileMemoryError)?);
            }

            Ok(DescriptorChainConsumer {
                memory_access: self.memory_access.clone(),
                buffers: other,
                bytes_consumed: 0,
            })
        } else if rem == 0 {
            Ok(DescriptorChainConsumer {
                memory_access: self.memory_access.clone(),
                buffers: VecDeque::new(),
                bytes_consumed: 0,
            })
        } else {
            Err(Error::SplitOutOfBounds(offset))
        }
    }
}

/// A descriptor plan that can cross threads without extending a guest-memory
/// borrow. Addresses are captured once by the queue owner; volatile slices are
/// created only inside the receiving thread. Retain this plan through add_used:
/// its RAM lease also prevents the cold-page pager replacing these mappings.
pub(crate) struct OwnedDescriptorChain {
    memory_access: Option<std::sync::Arc<super::memory_gate::Access>>,
    readable: Vec<(GuestAddress, u32)>,
    writable: Vec<(GuestAddress, u32)>,
}

impl OwnedDescriptorChain {
    pub(crate) fn new(mut chain: DescriptorChain<'_>) -> Result<Self> {
        let mut plan = Self {
            memory_access: chain.memory_access.clone(),
            readable: Vec::new(),
            writable: Vec::new(),
        };
        let mut writing = false;
        let mut readable_len = 0usize;
        let mut writable_len = 0usize;
        loop {
            // Validate the complete captured plan before decoding a request.
            // Header admission does not need to allocate Reader/Writer buffers.
            descriptor_slice(chain.mem, chain.addr, chain.len)?;
            if chain.is_write_only() {
                writing = true;
                writable_len = writable_len
                    .checked_add(chain.len as usize)
                    .ok_or(Error::DescriptorChainOverflow)?;
                plan.writable.push((chain.addr, chain.len));
            } else if writing {
                return Err(Error::InvalidChain);
            } else {
                readable_len = readable_len
                    .checked_add(chain.len as usize)
                    .ok_or(Error::DescriptorChainOverflow)?;
                plan.readable.push((chain.addr, chain.len));
            }
            // has_next() masks the NEXT flag at the TTL limit. Reject cycles
            // and truncated chains instead of accepting a partial request.
            if chain.flags & super::queue::VIRTQ_DESC_F_NEXT == 0 {
                break;
            }
            chain = chain.next_descriptor().ok_or(Error::InvalidChain)?;
        }
        Ok(plan)
    }

    /// Decode a fixed-size prefix without constructing descriptor consumers.
    /// The captured plan and its RAM lease have already validated every range,
    /// including writable buffers that this prefix itself does not touch.
    pub(crate) fn read_prefix<T: ByteValued>(&self, mem: &GuestMemoryMmap) -> Result<T> {
        let mut object = T::zeroed();
        let mut remaining = object.as_mut_slice();
        for &(address, length) in &self.readable {
            if remaining.is_empty() {
                break;
            }
            let count = remaining.len().min(length as usize);
            mem.read_slice(&mut remaining[..count], address)
                .map_err(Error::GuestMemoryError)?;
            remaining = &mut remaining[count..];
        }
        if !remaining.is_empty() {
            return Err(Error::IoError(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "descriptor chain is shorter than the request prefix",
            )));
        }
        Ok(object)
    }

    #[cfg(test)]
    pub(crate) fn reader_writer<'a>(
        &self,
        mem: &'a GuestMemoryMmap,
    ) -> Result<(Reader<'a>, Writer<'a>)> {
        self.reader_writer_after(mem, 0)
    }

    /// Construct consumers after a previously captured readable prefix. This
    /// avoids constructing and splitting a second Reader for the same header.
    pub(crate) fn reader_writer_after<'a>(
        &self,
        mem: &'a GuestMemoryMmap,
        prefix: usize,
    ) -> Result<(Reader<'a>, Writer<'a>)> {
        let consumer = |ranges: &[(GuestAddress, u32)], mut skip: usize| {
            let requested_skip = skip;
            let mut total_len = 0usize;
            let mut buffers = VecDeque::with_capacity(ranges.len());
            for &(address, length) in ranges {
                total_len = total_len
                    .checked_add(length as usize)
                    .ok_or(Error::DescriptorChainOverflow)?;
                let slice = descriptor_slice(mem, address, length)?;
                if skip >= length as usize {
                    skip -= length as usize;
                    continue;
                }
                buffers.push_back(slice.offset(skip).map_err(Error::VolatileMemoryError)?);
                skip = 0;
            }
            if skip != 0 {
                return Err(Error::SplitOutOfBounds(requested_skip));
            }
            Ok(DescriptorChainConsumer {
                memory_access: self.memory_access.clone(),
                buffers,
                bytes_consumed: 0,
            })
        };
        Ok((
            Reader {
                buffer: consumer(&self.readable, prefix)?,
            },
            Writer {
                buffer: consumer(&self.writable, 0)?,
            },
        ))
    }
}

fn descriptor_slice(
    mem: &GuestMemoryMmap,
    address: GuestAddress,
    length: u32,
) -> Result<VolatileSlice<'_>> {
    let region = mem.find_region(address).ok_or(Error::FindMemoryRegion)?;
    let offset = address
        .checked_sub(region.start_addr().raw_value())
        .ok_or(Error::InvalidChain)?;
    region
        .deref()
        .get_slice(offset.raw_value() as usize, length as usize)
        .map_err(Error::VolatileMemoryError)
}

/// Provides high-level interface over the sequence of memory regions
/// defined by readable descriptors in the descriptor chain.
///
/// Note that virtio spec requires driver to place any device-writable
/// descriptors after any device-readable descriptors (2.6.4.2 in Virtio Spec v1.1).
/// Reader will skip iterating over descriptor chain when first writable
/// descriptor is encountered.
#[derive(Clone)]
pub struct Reader<'a> {
    buffer: DescriptorChainConsumer<'a>,
}

impl<'a> Reader<'a> {
    /// Construct a new Reader wrapper over `desc_chain`.
    pub fn new(mem: &'a GuestMemoryMmap, chain: DescriptorChain<'a>) -> Result<Reader<'a>> {
        let memory_access = chain.memory_access.clone();
        let mut total_len: usize = 0;
        let buffers = chain
            .into_iter()
            .readable()
            .map(|desc| {
                // Verify that summing the descriptor sizes does not overflow.
                // This can happen if a driver tricks a device into reading more data than
                // fits in a `usize`.
                total_len = total_len
                    .checked_add(desc.len as usize)
                    .ok_or(Error::DescriptorChainOverflow)?;

                let region = mem.find_region(desc.addr).ok_or(Error::FindMemoryRegion)?;
                let offset = desc
                    .addr
                    .checked_sub(region.start_addr().raw_value())
                    .unwrap();
                region
                    .deref()
                    .get_slice(offset.raw_value() as usize, desc.len as usize)
                    .map_err(Error::VolatileMemoryError)
            })
            .collect::<Result<VecDeque<VolatileSlice<'a>>>>()?;
        Ok(Reader {
            buffer: DescriptorChainConsumer {
                memory_access,
                buffers,
                bytes_consumed: 0,
            },
        })
    }

    /// Reads an object from the descriptor chain buffer.
    pub fn read_obj<T: ByteValued>(&mut self) -> io::Result<T> {
        let mut obj = MaybeUninit::<T>::uninit();

        // Safe because `MaybeUninit` guarantees that the pointer is valid for
        // `size_of::<T>()` bytes.
        let buf = unsafe {
            ::std::slice::from_raw_parts_mut(obj.as_mut_ptr() as *mut u8, size_of::<T>())
        };

        self.read_exact(buf)?;

        // Safe because any type that implements `ByteValued` can be considered initialized
        // even if it is filled with random data.
        Ok(unsafe { obj.assume_init() })
    }

    /// Reads data from the descriptor chain buffer into a file descriptor.
    /// Returns the number of bytes read from the descriptor chain buffer.
    /// The number of bytes read can be less than `count` if there isn't
    /// enough data in the descriptor chain buffer.
    pub fn read_to<F: FileReadWriteVolatile>(
        &mut self,
        mut dst: F,
        count: usize,
    ) -> io::Result<usize> {
        self.buffer
            .consume(count, |bufs| dst.write_vectored_volatile(bufs))
    }

    /// Reads data from the descriptor chain buffer into a File at offset `off`.
    /// Returns the number of bytes read from the descriptor chain buffer.
    /// The number of bytes read can be less than `count` if there isn't
    /// enough data in the descriptor chain buffer.
    pub fn read_to_at<F: FileReadWriteAtVolatile>(
        &mut self,
        dst: F,
        count: usize,
        off: u64,
    ) -> io::Result<usize> {
        self.buffer
            .consume(count, |bufs| dst.write_vectored_at_volatile(bufs, off))
    }

    pub fn read_exact_to<F: FileReadWriteVolatile>(
        &mut self,
        mut dst: F,
        mut count: usize,
    ) -> io::Result<()> {
        while count > 0 {
            match self.read_to(&mut dst, count) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "failed to fill whole buffer",
                    ));
                }
                Ok(n) => count -= n,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }

        Ok(())
    }

    /// Returns number of bytes available for reading.  May return an error if the combined
    /// lengths of all the buffers in the DescriptorChain would cause an integer overflow.
    pub fn available_bytes(&self) -> usize {
        self.buffer.available_bytes()
    }

    /// Returns number of bytes already read from the descriptor chain buffer.
    pub fn bytes_read(&self) -> usize {
        self.buffer.bytes_consumed()
    }

    /// Splits this `Reader` into two at the given offset in the `DescriptorChain` buffer.
    /// After the split, `self` will be able to read up to `offset` bytes while the returned
    /// `Reader` can read up to `available_bytes() - offset` bytes.  Returns an error if
    /// `offset > self.available_bytes()`.
    pub fn split_at(&mut self, offset: usize) -> Result<Reader<'a>> {
        self.buffer.split_at(offset).map(|buffer| Reader { buffer })
    }
}

impl io::Read for Reader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.buffer.consume(buf.len(), |bufs| {
            let mut rem = buf;
            let mut total = 0;
            for vs in bufs {
                let copy_len = cmp::min(rem.len(), vs.len());

                // Safe because we have already verified that `vs` points to valid memory.
                unsafe {
                    copy_nonoverlapping(vs.ptr_guard().as_ptr(), rem.as_mut_ptr(), copy_len);
                }
                rem = &mut rem[copy_len..];
                total += copy_len;
            }
            Ok(total)
        })
    }
}

/// Provides high-level interface over the sequence of memory regions
/// defined by writable descriptors in the descriptor chain.
///
/// Note that virtio spec requires driver to place any device-writable
/// descriptors after any device-readable descriptors (2.6.4.2 in Virtio Spec v1.1).
/// Writer will start iterating the descriptors from the first writable one and will
/// assume that all following descriptors are writable.
#[derive(Clone)]
pub struct Writer<'a> {
    buffer: DescriptorChainConsumer<'a>,
}

/// Captures only integer ranges. The access lease pins the descriptor memory
/// until success or cancellation, including after the data Writer is consumed.
pub(crate) struct FileReadCompletion {
    _access: std::sync::Arc<super::memory_gate::Access>,
    hints: std::sync::Arc<super::memory_gate::ReadHints>,
    ranges: Vec<(u64, usize)>,
    capacity: usize,
}
impl FileReadCompletion {
    /// Only the successful byte prefix is eligible. Drop/error/EOF enqueue
    /// nothing; an impossible count declines acceleration without touching RAM.
    pub(crate) fn finish(self, mut count: usize) {
        if count > self.capacity {
            return;
        }
        let mut pending: Option<(u64, usize)> = None;
        for (address, length) in self.ranges {
            let written = count.min(length);
            if written == 0 {
                break;
            }
            let contiguous = pending
                .as_ref()
                .is_some_and(|(start, bytes)| start.checked_add(*bytes as u64) == Some(address));
            if contiguous {
                pending.as_mut().unwrap().1 += written;
            } else {
                if let Some((start, bytes)) = pending.take() {
                    self.hints.submit(start, bytes);
                }
                pending = Some((address, written));
            }
            count -= written;
            if count == 0 {
                break;
            }
        }
        if let Some((start, bytes)) = pending {
            self.hints.submit(start, bytes);
        }
    }
}

impl<'a> Writer<'a> {
    /// Capture after splitting off the response header, before handing the
    /// writer to READ. Disabled VMs allocate no hint metadata.
    pub(crate) fn file_read_completion(&self) -> Option<FileReadCompletion> {
        let access = self.buffer.memory_access.as_ref()?;
        let hints = access.read_hints()?;
        if self.buffer.buffers.len() > super::memory_gate::MAX_READ_HINT_RANGES {
            return None;
        }
        Some(FileReadCompletion {
            _access: access.clone(),
            hints,
            ranges: self
                .buffer
                .buffers
                .iter()
                .map(|slice| (slice.ptr_guard().as_ptr() as u64, slice.len()))
                .collect(),
            capacity: self.available_bytes(),
        })
    }
    /// Construct a new Writer wrapper over `desc_chain`.
    pub fn new(mem: &'a GuestMemoryMmap, chain: DescriptorChain<'a>) -> Result<Writer<'a>> {
        let memory_access = chain.memory_access.clone();
        let mut total_len: usize = 0;
        let buffers = chain
            .into_iter()
            .writable()
            .map(|desc| {
                // Verify that summing the descriptor sizes does not overflow.
                // This can happen if a driver tricks a device into writing more data than
                // fits in a `usize`.
                total_len = total_len
                    .checked_add(desc.len as usize)
                    .ok_or(Error::DescriptorChainOverflow)?;

                let region = mem.find_region(desc.addr).ok_or(Error::FindMemoryRegion)?;
                let offset = desc
                    .addr
                    .checked_sub(region.start_addr().raw_value())
                    .unwrap();
                region
                    .deref()
                    .get_slice(offset.raw_value() as usize, desc.len as usize)
                    .map_err(Error::VolatileMemoryError)
            })
            .collect::<Result<VecDeque<VolatileSlice<'a>>>>()?;

        Ok(Writer {
            buffer: DescriptorChainConsumer {
                memory_access,
                buffers,
                bytes_consumed: 0,
            },
        })
    }

    /// Writes an object to the descriptor chain buffer.
    pub fn write_obj<T: ByteValued>(&mut self, val: T) -> io::Result<()> {
        self.write_all(val.as_slice())
    }

    /// Returns number of bytes available for writing.  May return an error if the combined
    /// lengths of all the buffers in the DescriptorChain would cause an overflow.
    pub fn available_bytes(&self) -> usize {
        self.buffer.available_bytes()
    }

    /// Writes data to the descriptor chain buffer from a file descriptor.
    /// Returns the number of bytes written to the descriptor chain buffer.
    /// The number of bytes written can be less than `count` if
    /// there isn't enough data in the descriptor chain buffer.
    pub fn write_from<F: FileReadWriteVolatile>(
        &mut self,
        mut src: F,
        count: usize,
    ) -> io::Result<usize> {
        self.buffer
            .consume(count, |bufs| src.read_vectored_volatile(bufs))
    }

    /// Writes data to the descriptor chain buffer from a File at offset `off`.
    /// Returns the number of bytes written to the descriptor chain buffer.
    /// The number of bytes written can be less than `count` if
    /// there isn't enough data in the descriptor chain buffer.
    pub fn write_from_at<F: FileReadWriteAtVolatile>(
        &mut self,
        src: F,
        count: usize,
        off: u64,
    ) -> io::Result<usize> {
        self.buffer
            .consume(count, |bufs| src.read_vectored_at_volatile(bufs, off))
    }

    pub fn write_all_from<F: FileReadWriteVolatile>(
        &mut self,
        mut src: F,
        mut count: usize,
    ) -> io::Result<()> {
        while count > 0 {
            match self.write_from(&mut src, count) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "failed to write whole buffer",
                    ));
                }
                Ok(n) => count -= n,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }

        Ok(())
    }

    /// Returns number of bytes already written to the descriptor chain buffer.
    pub fn bytes_written(&self) -> usize {
        self.buffer.bytes_consumed()
    }

    /// Splits this `Writer` into two at the given offset in the `DescriptorChain` buffer.
    /// After the split, `self` will be able to write up to `offset` bytes while the returned
    /// `Writer` can write up to `available_bytes() - offset` bytes.  Returns an error if
    /// `offset > self.available_bytes()`.
    pub fn split_at(&mut self, offset: usize) -> Result<Writer<'a>> {
        self.buffer.split_at(offset).map(|buffer| Writer { buffer })
    }
}

impl io::Write for Writer<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.consume(buf.len(), |bufs| {
            let mut rem = buf;
            let mut total = 0;
            for vs in bufs {
                let copy_len = cmp::min(rem.len(), vs.len());

                // Safe because we have already verified that `vs` points to valid memory.
                unsafe {
                    copy_nonoverlapping(rem.as_ptr(), vs.ptr_guard_mut().as_ptr(), copy_len);
                }
                rem = &rem[copy_len..];
                total += copy_len;
            }
            Ok(total)
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        // Nothing to flush since the writes go straight into the buffer.
        Ok(())
    }
}

const VIRTQ_DESC_F_NEXT: u16 = 0x1;
const VIRTQ_DESC_F_WRITE: u16 = 0x2;

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum DescriptorType {
    Readable,
    Writable,
}

#[derive(Copy, Clone, Debug, Default)]
#[repr(C)]
struct virtq_desc {
    addr: Le64,
    len: Le32,
    flags: Le16,
    next: Le16,
}

// Safe because it only has data and has no implicit padding.
unsafe impl ByteValued for virtq_desc {}

/// Test utility function to create a descriptor chain in guest memory.
pub fn create_descriptor_chain(
    memory: &GuestMemoryMmap,
    descriptor_array_addr: GuestAddress,
    mut buffers_start_addr: GuestAddress,
    descriptors: Vec<(DescriptorType, u32)>,
    spaces_between_regions: u32,
) -> Result<DescriptorChain<'_>> {
    let descriptors_len = descriptors.len();
    for (index, (type_, size)) in descriptors.into_iter().enumerate() {
        let mut flags = 0;
        if let DescriptorType::Writable = type_ {
            flags |= VIRTQ_DESC_F_WRITE;
        }
        if index + 1 < descriptors_len {
            flags |= VIRTQ_DESC_F_NEXT;
        }

        let index = index as u16;
        let desc = virtq_desc {
            addr: buffers_start_addr.raw_value().into(),
            len: size.into(),
            flags: flags.into(),
            next: (index + 1).into(),
        };

        let offset = size + spaces_between_regions;
        buffers_start_addr = buffers_start_addr
            .checked_add(u64::from(offset))
            .ok_or(Error::InvalidChain)?;

        let _ = memory.write_obj(
            desc,
            descriptor_array_addr
                .checked_add(u64::from(index) * std::mem::size_of::<virtq_desc>() as u64)
                .ok_or(Error::InvalidChain)?,
        );
    }

    DescriptorChain::checked_new(memory, descriptor_array_addr, 0x100, 0).ok_or(Error::InvalidChain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_read_completion_excludes_header_and_partial_tail_and_retains_lease() {
        use super::super::memory_gate::{ColdFaultActivity, ReadHints};
        use std::sync::Arc;
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 32768)]).unwrap();
        let gate = super::super::memory_gate::register(&mem);
        let hints = Arc::new(ReadHints::default());
        gate.close(std::time::Duration::ZERO).unwrap();
        gate.install_cold_faults(Arc::new(ColdFaultActivity::default()))
            .unwrap();
        gate.install_read_hints(hints.clone()).unwrap();
        gate.open();
        let mut writer = Writer {
            buffer: DescriptorChainConsumer {
                memory_access: super::super::memory_gate::access(&mem),
                buffers: [
                    (4080, 16),
                    (4096, 2048),
                    (6144, 2048),
                    (8192, 4096),
                    (16384, 4096),
                ]
                .into_iter()
                .map(|(address, len)| mem.get_slice(GuestAddress(address), len).unwrap())
                .collect(),
                bytes_consumed: 0,
            },
        };
        let mut data = writer.split_at(16).unwrap();
        let completion = data.file_read_completion().unwrap();
        data.write_all(&[0x55; 5000]).unwrap();
        assert!(
            hints.take(1).is_none(),
            "only successful completion produces hints"
        );
        drop((data, writer));
        assert!(
            !gate.try_close().unwrap(),
            "completion retains the RAM access lease"
        );
        completion.finish(5000);
        let host = mem.get_host_address(GuestAddress(4096)).unwrap() as u64;
        assert_eq!(hints.take(16), Some((host, 4096)));
        assert!(
            hints.take(16).is_none(),
            "short tail and response header are excluded"
        );
        assert!(gate.try_close().unwrap());
    }
    #[test]
    fn file_read_completion_declines_disabled_canceled_eof_and_invalid_counts() {
        use super::super::memory_gate::{ColdFaultActivity, ReadHints};
        use std::sync::Arc;
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 16384)]).unwrap();
        let gate = super::super::memory_gate::register(&mem);
        let make = || Writer {
            buffer: DescriptorChainConsumer {
                memory_access: super::super::memory_gate::access(&mem),
                buffers: [mem.get_slice(GuestAddress(4096), 4096).unwrap()]
                    .into_iter()
                    .collect(),
                bytes_consumed: 0,
            },
        };
        assert!(make().file_read_completion().is_none());
        let hints = Arc::new(ReadHints::default());
        gate.close(std::time::Duration::ZERO).unwrap();
        gate.install_cold_faults(Arc::new(ColdFaultActivity::default()))
            .unwrap();
        gate.install_read_hints(hints.clone()).unwrap();
        gate.open();
        for count in [None, Some(0), Some(4097)] {
            let writer = make();
            let completion = writer.file_read_completion().unwrap();
            drop(writer);
            if let Some(count) = count {
                completion.finish(count);
            } else {
                drop(completion);
            }
            assert!(hints.take(1).is_none());
            assert!(gate.try_close().unwrap());
            gate.open();
        }
        let mut oversized = make();
        oversized.buffer.buffers = vec![
            mem.get_slice(GuestAddress(4096), 4096).unwrap();
            super::super::memory_gate::MAX_READ_HINT_RANGES + 1
        ]
        .into();
        assert!(
            oversized.file_read_completion().is_none(),
            "completion metadata is bounded"
        );
        drop(oversized);
        assert!(gate.try_close().unwrap());
    }
    #[test]
    fn owned_prefix_spans_fragments_and_body_keeps_captured_addresses_and_ram_lease() {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let gate = super::super::memory_gate::register(&mem);
        let chain = create_descriptor_chain(
            &mem,
            GuestAddress(0),
            GuestAddress(0x100),
            vec![
                (DescriptorType::Readable, 3),
                (DescriptorType::Readable, 0),
                (DescriptorType::Readable, 5),
                (DescriptorType::Readable, 4),
                (DescriptorType::Writable, 8),
            ],
            8,
        )
        .unwrap();
        let plan = OwnedDescriptorChain::new(chain).unwrap();
        let header = 0x0807060504030201u64;
        mem.write_slice(&header.as_slice()[..3], plan.readable[0].0)
            .unwrap();
        mem.write_slice(&header.as_slice()[3..], plan.readable[2].0)
            .unwrap();
        mem.write_slice(b"body", plan.readable[3].0).unwrap();
        assert_eq!(plan.read_prefix::<u64>(&mem).unwrap(), header);

        // The decoded header is captured separately. Changing its bytes and
        // the descriptor table cannot redirect or reread the body consumers.
        mem.write_slice(&[0; 3], plan.readable[0].0).unwrap();
        mem.write_obj(0x800u64, GuestAddress(3 * 16)).unwrap();
        mem.write_slice(b"fake", GuestAddress(0x800)).unwrap();
        let (mut reader, mut writer) = plan.reader_writer_after(&mem, 8).unwrap();
        assert_eq!(reader.bytes_read(), 0);
        assert_eq!(reader.available_bytes(), 4);
        let mut body = [0u8; 4];
        reader.read_exact(&mut body).unwrap();
        assert_eq!(&body, b"body");
        writer.write_all(&body).unwrap();
        let mut output = [0u8; 4];
        mem.read_slice(&mut output, plan.writable[0].0).unwrap();
        assert_eq!(output, body);
        assert!(matches!(
            plan.reader_writer_after(&mem, 13),
            Err(Error::SplitOutOfBounds(13))
        ));
        drop((reader, writer));
        assert!(!gate.try_close().unwrap());
        drop(plan);
        assert!(gate.try_close().unwrap());
    }

    #[test]
    fn owned_prefix_rejects_short_input_and_validates_unread_output_ranges() {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let chain = create_descriptor_chain(
            &mem,
            GuestAddress(0),
            GuestAddress(0x100),
            vec![
                (DescriptorType::Readable, 4),
                (DescriptorType::Writable, 16),
            ],
            0,
        )
        .unwrap();
        let plan = OwnedDescriptorChain::new(chain).unwrap();
        assert!(matches!(
            plan.read_prefix::<u64>(&mem),
            Err(Error::IoError(error)) if error.kind() == io::ErrorKind::UnexpectedEof
        ));
        drop(plan);

        // The prefix is readable, but executing it would have an invalid
        // output buffer. Admission must reject the whole captured plan.
        mem.write_obj(0xfff8u64, GuestAddress(16)).unwrap();
        let chain = DescriptorChain::checked_new(&mem, GuestAddress(0), 0x100, 0).unwrap();
        assert!(matches!(
            OwnedDescriptorChain::new(chain),
            Err(Error::VolatileMemoryError(_))
        ));
    }

    #[test]
    fn owned_plan_captures_addresses_and_keeps_ram_pinned_across_threads() {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let gate = super::super::memory_gate::register(&mem);
        let chain = create_descriptor_chain(
            &mem,
            GuestAddress(0),
            GuestAddress(0x100),
            vec![(DescriptorType::Readable, 8), (DescriptorType::Writable, 8)],
            0,
        )
        .unwrap();
        mem.write_slice(b"original", GuestAddress(0x100)).unwrap();
        let plan = OwnedDescriptorChain::new(chain).unwrap();
        // A driver changing its descriptor table after admission must not
        // redirect the worker's I/O to a different address.
        mem.write_obj(0x200u64, GuestAddress(0)).unwrap();
        mem.write_slice(b"replaced", GuestAddress(0x200)).unwrap();
        assert!(!gate.try_close().unwrap());
        let copied_mem = mem.clone();
        let plan = std::thread::spawn(move || {
            let (mut reader, mut writer) = plan.reader_writer(&copied_mem).unwrap();
            let mut bytes = [0u8; 8];
            reader.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"original");
            writer.write_all(&bytes).unwrap();
            drop((reader, writer));
            plan
        })
        .join()
        .unwrap();
        let mut bytes = [0u8; 8];
        mem.read_slice(&mut bytes, GuestAddress(0x108)).unwrap();
        assert_eq!(&bytes, b"original");
        // Worker completion alone is insufficient: the owner still has to
        // publish the used element before dropping this retained lease.
        assert!(!gate.try_close().unwrap());
        drop(plan);
        assert!(gate.try_close().unwrap());
    }

    #[test]
    fn owned_plan_rejects_reordered_and_cyclic_chains() {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let chain = create_descriptor_chain(
            &mem,
            GuestAddress(0),
            GuestAddress(0x100),
            vec![(DescriptorType::Writable, 8), (DescriptorType::Readable, 8)],
            0,
        )
        .unwrap();
        assert!(matches!(
            OwnedDescriptorChain::new(chain),
            Err(Error::InvalidChain)
        ));
        let mut chain = create_descriptor_chain(
            &mem,
            GuestAddress(0),
            GuestAddress(0x100),
            vec![(DescriptorType::Readable, 8)],
            0,
        )
        .unwrap();
        chain.flags |= VIRTQ_DESC_F_NEXT;
        chain.next = 0;
        // Cycle the underlying descriptor as well as the captured head.
        mem.write_obj(VIRTQ_DESC_F_NEXT, GuestAddress(12)).unwrap();
        mem.write_obj(0u16, GuestAddress(14)).unwrap();
        assert!(matches!(
            OwnedDescriptorChain::new(chain),
            Err(Error::InvalidChain)
        ));
    }

    #[test]
    fn reader_test_simple_chain() {
        use DescriptorType::*;

        let memory_start_addr = GuestAddress(0x0);
        let memory = GuestMemoryMmap::from_ranges(&[(memory_start_addr, 0x10000)]).unwrap();

        let chain = create_descriptor_chain(
            &memory,
            GuestAddress(0x0),
            GuestAddress(0x100),
            vec![
                (Readable, 8),
                (Readable, 16),
                (Readable, 18),
                (Readable, 64),
            ],
            0,
        )
        .expect("create_descriptor_chain failed");
        let mut reader = Reader::new(&memory, chain).expect("failed to create Reader");
        assert_eq!(reader.available_bytes(), 106);
        assert_eq!(reader.bytes_read(), 0);

        let mut buffer = [0_u8; 64];
        if reader.read_exact(&mut buffer).is_err() {
            panic!("read_exact should not fail here");
        }

        assert_eq!(reader.available_bytes(), 42);
        assert_eq!(reader.bytes_read(), 64);

        match reader.read(&mut buffer) {
            Err(_) => panic!("read should not fail here"),
            Ok(length) => assert_eq!(length, 42),
        }

        assert_eq!(reader.available_bytes(), 0);
        assert_eq!(reader.bytes_read(), 106);
    }

    #[test]
    fn writer_test_simple_chain() {
        use DescriptorType::*;

        let memory_start_addr = GuestAddress(0x0);
        let memory = GuestMemoryMmap::from_ranges(&[(memory_start_addr, 0x10000)]).unwrap();

        let chain = create_descriptor_chain(
            &memory,
            GuestAddress(0x0),
            GuestAddress(0x100),
            vec![
                (Writable, 8),
                (Writable, 16),
                (Writable, 18),
                (Writable, 64),
            ],
            0,
        )
        .expect("create_descriptor_chain failed");
        let mut writer = Writer::new(&memory, chain).expect("failed to create Writer");
        assert_eq!(writer.available_bytes(), 106);
        assert_eq!(writer.bytes_written(), 0);

        let buffer = [0_u8; 64];
        if writer.write_all(&buffer).is_err() {
            panic!("write_all should not fail here");
        }

        assert_eq!(writer.available_bytes(), 42);
        assert_eq!(writer.bytes_written(), 64);

        match writer.write(&buffer) {
            Err(_) => panic!("write should not fail here"),
            Ok(length) => assert_eq!(length, 42),
        }

        assert_eq!(writer.available_bytes(), 0);
        assert_eq!(writer.bytes_written(), 106);
    }

    #[test]
    fn reader_test_incompatible_chain() {
        use DescriptorType::*;

        let memory_start_addr = GuestAddress(0x0);
        let memory = GuestMemoryMmap::from_ranges(&[(memory_start_addr, 0x10000)]).unwrap();

        let chain = create_descriptor_chain(
            &memory,
            GuestAddress(0x0),
            GuestAddress(0x100),
            vec![(Writable, 8)],
            0,
        )
        .expect("create_descriptor_chain failed");
        let mut reader = Reader::new(&memory, chain).expect("failed to create Reader");
        assert_eq!(reader.available_bytes(), 0);
        assert_eq!(reader.bytes_read(), 0);

        assert!(reader.read_obj::<u8>().is_err());

        assert_eq!(reader.available_bytes(), 0);
        assert_eq!(reader.bytes_read(), 0);
    }

    #[test]
    fn writer_test_incompatible_chain() {
        use DescriptorType::*;

        let memory_start_addr = GuestAddress(0x0);
        let memory = GuestMemoryMmap::from_ranges(&[(memory_start_addr, 0x10000)]).unwrap();

        let chain = create_descriptor_chain(
            &memory,
            GuestAddress(0x0),
            GuestAddress(0x100),
            vec![(Readable, 8)],
            0,
        )
        .expect("create_descriptor_chain failed");
        let mut writer = Writer::new(&memory, chain).expect("failed to create Writer");
        assert_eq!(writer.available_bytes(), 0);
        assert_eq!(writer.bytes_written(), 0);

        assert!(writer.write_obj(0u8).is_err());

        assert_eq!(writer.available_bytes(), 0);
        assert_eq!(writer.bytes_written(), 0);
    }

    #[test]
    fn reader_writer_shared_chain() {
        use DescriptorType::*;

        let memory_start_addr = GuestAddress(0x0);
        let memory = GuestMemoryMmap::from_ranges(&[(memory_start_addr, 0x10000)]).unwrap();

        let chain = create_descriptor_chain(
            &memory,
            GuestAddress(0x0),
            GuestAddress(0x100),
            vec![
                (Readable, 16),
                (Readable, 16),
                (Readable, 96),
                (Writable, 64),
                (Writable, 1),
                (Writable, 3),
            ],
            0,
        )
        .expect("create_descriptor_chain failed");
        let mut reader = Reader::new(&memory, chain.clone()).expect("failed to create Reader");
        let mut writer = Writer::new(&memory, chain).expect("failed to create Writer");

        assert_eq!(reader.bytes_read(), 0);
        assert_eq!(writer.bytes_written(), 0);

        let mut buffer = Vec::with_capacity(200);

        assert_eq!(
            reader
                .read_to_end(&mut buffer)
                .expect("read should not fail here"),
            128
        );

        // The writable descriptors are only 68 bytes long.
        writer
            .write_all(&buffer[..68])
            .expect("write should not fail here");

        assert_eq!(reader.available_bytes(), 0);
        assert_eq!(reader.bytes_read(), 128);
        assert_eq!(writer.available_bytes(), 0);
        assert_eq!(writer.bytes_written(), 68);
    }

    #[test]
    fn reader_writer_shattered_object() {
        use DescriptorType::*;

        let memory_start_addr = GuestAddress(0x0);
        let memory = GuestMemoryMmap::from_ranges(&[(memory_start_addr, 0x10000)]).unwrap();

        let secret: Le32 = 0x12345678.into();

        // Create a descriptor chain with memory regions that are properly separated.
        let chain_writer = create_descriptor_chain(
            &memory,
            GuestAddress(0x0),
            GuestAddress(0x100),
            vec![(Writable, 1), (Writable, 1), (Writable, 1), (Writable, 1)],
            123,
        )
        .expect("create_descriptor_chain failed");
        let mut writer = Writer::new(&memory, chain_writer).expect("failed to create Writer");
        if writer.write_obj(secret).is_err() {
            panic!("write_obj should not fail here");
        }

        // The readable chain aliases the bytes written through the writable chain.
        let chain_reader = create_descriptor_chain(
            &memory,
            GuestAddress(0x0),
            GuestAddress(0x100),
            vec![(Readable, 1), (Readable, 1), (Readable, 1), (Readable, 1)],
            123,
        )
        .expect("create_descriptor_chain failed");
        let mut reader = Reader::new(&memory, chain_reader).expect("failed to create Reader");
        match reader.read_obj::<Le32>() {
            Err(_) => panic!("read_obj should not fail here"),
            Ok(read_secret) => assert_eq!(read_secret, secret),
        }
    }

    #[test]
    fn reader_unexpected_eof() {
        use DescriptorType::*;

        let memory_start_addr = GuestAddress(0x0);
        let memory = GuestMemoryMmap::from_ranges(&[(memory_start_addr, 0x10000)]).unwrap();

        let chain = create_descriptor_chain(
            &memory,
            GuestAddress(0x0),
            GuestAddress(0x100),
            vec![(Readable, 256), (Readable, 256)],
            0,
        )
        .expect("create_descriptor_chain failed");

        let mut reader = Reader::new(&memory, chain).expect("failed to create Reader");

        let mut buf = vec![0; 1024];

        assert_eq!(
            reader
                .read_exact(&mut buf[..])
                .expect_err("read more bytes than available")
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn split_border() {
        use DescriptorType::*;

        let memory_start_addr = GuestAddress(0x0);
        let memory = GuestMemoryMmap::from_ranges(&[(memory_start_addr, 0x10000)]).unwrap();

        let chain = create_descriptor_chain(
            &memory,
            GuestAddress(0x0),
            GuestAddress(0x100),
            vec![
                (Readable, 16),
                (Readable, 16),
                (Readable, 96),
                (Writable, 64),
                (Writable, 1),
                (Writable, 3),
            ],
            0,
        )
        .expect("create_descriptor_chain failed");
        let mut reader = Reader::new(&memory, chain).expect("failed to create Reader");

        let other = reader.split_at(32).expect("failed to split Reader");
        assert_eq!(reader.available_bytes(), 32);
        assert_eq!(other.available_bytes(), 96);
    }

    #[test]
    fn split_middle() {
        use DescriptorType::*;

        let memory_start_addr = GuestAddress(0x0);
        let memory = GuestMemoryMmap::from_ranges(&[(memory_start_addr, 0x10000)]).unwrap();

        let chain = create_descriptor_chain(
            &memory,
            GuestAddress(0x0),
            GuestAddress(0x100),
            vec![
                (Readable, 16),
                (Readable, 16),
                (Readable, 96),
                (Writable, 64),
                (Writable, 1),
                (Writable, 3),
            ],
            0,
        )
        .expect("create_descriptor_chain failed");
        let mut reader = Reader::new(&memory, chain).expect("failed to create Reader");

        let other = reader.split_at(24).expect("failed to split Reader");
        assert_eq!(reader.available_bytes(), 24);
        assert_eq!(other.available_bytes(), 104);
    }

    #[test]
    fn split_end() {
        use DescriptorType::*;

        let memory_start_addr = GuestAddress(0x0);
        let memory = GuestMemoryMmap::from_ranges(&[(memory_start_addr, 0x10000)]).unwrap();

        let chain = create_descriptor_chain(
            &memory,
            GuestAddress(0x0),
            GuestAddress(0x100),
            vec![
                (Readable, 16),
                (Readable, 16),
                (Readable, 96),
                (Writable, 64),
                (Writable, 1),
                (Writable, 3),
            ],
            0,
        )
        .expect("create_descriptor_chain failed");
        let mut reader = Reader::new(&memory, chain).expect("failed to create Reader");

        let other = reader.split_at(128).expect("failed to split Reader");
        assert_eq!(reader.available_bytes(), 128);
        assert_eq!(other.available_bytes(), 0);
    }

    #[test]
    fn split_beginning() {
        use DescriptorType::*;

        let memory_start_addr = GuestAddress(0x0);
        let memory = GuestMemoryMmap::from_ranges(&[(memory_start_addr, 0x10000)]).unwrap();

        let chain = create_descriptor_chain(
            &memory,
            GuestAddress(0x0),
            GuestAddress(0x100),
            vec![
                (Readable, 16),
                (Readable, 16),
                (Readable, 96),
                (Writable, 64),
                (Writable, 1),
                (Writable, 3),
            ],
            0,
        )
        .expect("create_descriptor_chain failed");
        let mut reader = Reader::new(&memory, chain).expect("failed to create Reader");

        let other = reader.split_at(0).expect("failed to split Reader");
        assert_eq!(reader.available_bytes(), 0);
        assert_eq!(other.available_bytes(), 128);
    }

    #[test]
    fn split_outofbounds() {
        use DescriptorType::*;

        let memory_start_addr = GuestAddress(0x0);
        let memory = GuestMemoryMmap::from_ranges(&[(memory_start_addr, 0x10000)]).unwrap();

        let chain = create_descriptor_chain(
            &memory,
            GuestAddress(0x0),
            GuestAddress(0x100),
            vec![
                (Readable, 16),
                (Readable, 16),
                (Readable, 96),
                (Writable, 64),
                (Writable, 1),
                (Writable, 3),
            ],
            0,
        )
        .expect("create_descriptor_chain failed");
        let mut reader = Reader::new(&memory, chain).expect("failed to create Reader");

        if reader.split_at(256).is_ok() {
            panic!("successfully split Reader with out of bounds offset");
        }
    }

    #[test]
    fn read_full() {
        use DescriptorType::*;

        let memory_start_addr = GuestAddress(0x0);
        let memory = GuestMemoryMmap::from_ranges(&[(memory_start_addr, 0x10000)]).unwrap();

        let chain = create_descriptor_chain(
            &memory,
            GuestAddress(0x0),
            GuestAddress(0x100),
            vec![(Readable, 16), (Readable, 16), (Readable, 16)],
            0,
        )
        .expect("create_descriptor_chain failed");
        let mut reader = Reader::new(&memory, chain).expect("failed to create Reader");

        let mut buf = [0u8; 64];
        assert_eq!(
            reader.read(&mut buf[..]).expect("failed to read to buffer"),
            48
        );
    }

    #[test]
    fn write_full() {
        use DescriptorType::*;

        let memory_start_addr = GuestAddress(0x0);
        let memory = GuestMemoryMmap::from_ranges(&[(memory_start_addr, 0x10000)]).unwrap();

        let chain = create_descriptor_chain(
            &memory,
            GuestAddress(0x0),
            GuestAddress(0x100),
            vec![(Writable, 16), (Writable, 16), (Writable, 16)],
            0,
        )
        .expect("create_descriptor_chain failed");
        let mut writer = Writer::new(&memory, chain).expect("failed to create Writer");

        let buf = [0xdeu8; 64];
        assert_eq!(
            writer.write(&buf[..]).expect("failed to write from buffer"),
            48
        );
    }
}
