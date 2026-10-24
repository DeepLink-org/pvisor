use crate::devices::virtio::net::backend::ConnectError;
#[cfg(target_os = "linux")]
use crate::devices::virtio::net::tap::Tap;
use crate::devices::virtio::net::unixgram::Unixgram;
use crate::devices::virtio::net::unixstream::Unixstream;
use crate::devices::virtio::net::{MAX_BUFFER_SIZE, QUEUE_SIZE};
use crate::devices::virtio::{DeviceQueue, InterruptTransport};

use super::VNET_HDR_LEN;
use super::backend::{NetBackend, ReadError, WriteError};
use super::device::{FrontendError, RxError, TxError, VirtioNetBackend};

use crate::utils::epoll::{ControlOperation, Epoll, EpollEvent, EventSet};
use std::os::fd::AsRawFd;
#[cfg(target_os = "macos")]
use std::os::fd::RawFd;
use std::thread;
use std::{cmp, result};
use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};

pub struct NetWorker {
    rx_q: DeviceQueue,
    tx_q: DeviceQueue,
    interrupt: InterruptTransport,

    mem: GuestMemoryMmap,
    backend: Box<dyn NetBackend + Send>,

    rx_frame_buf: [u8; MAX_BUFFER_SIZE],
    rx_frame_buf_len: usize,
    rx_has_deferred_frame: bool,

    tx_iovec: Vec<(GuestAddress, usize)>,
    tx_frame_buf: [u8; MAX_BUFFER_SIZE],
    tx_frame_len: usize,
    tx_has_deferred_frame: bool,
    stream_tx: Option<super::tx_batch::TxBatch>,
}

impl NetWorker {
    pub fn new(
        rx_q: DeviceQueue,
        tx_q: DeviceQueue,
        interrupt: InterruptTransport,
        mem: GuestMemoryMmap,
        _vnet_features: u64,
        cfg_backend: VirtioNetBackend,
    ) -> Result<Self, ConnectError> {
        let stream_tx = matches!(
            cfg_backend,
            VirtioNetBackend::UnixstreamFd(_) | VirtioNetBackend::UnixstreamPath(_)
        )
        .then(super::tx_batch::TxBatch::new);
        let backend = match cfg_backend {
            VirtioNetBackend::UnixstreamFd(fd) => {
                let owned_fd = fd.try_clone().map_err(ConnectError::CloneDescriptor)?;
                Box::new(Unixstream::new(owned_fd)?) as Box<dyn NetBackend + Send>
            }
            VirtioNetBackend::UnixstreamPath(path) => {
                Box::new(Unixstream::open(path)?) as Box<dyn NetBackend + Send>
            }
            VirtioNetBackend::UnixgramFd(fd) => {
                let owned_fd = fd.try_clone().map_err(ConnectError::CloneDescriptor)?;
                Box::new(Unixgram::new(owned_fd)) as Box<dyn NetBackend + Send>
            }
            VirtioNetBackend::UnixgramPath(path, vfkit_magic) => {
                Box::new(Unixgram::open(path, vfkit_magic)?) as Box<dyn NetBackend + Send>
            }
            #[cfg(target_os = "linux")]
            VirtioNetBackend::Tap(tap_name) => {
                Box::new(Tap::new(tap_name, _vnet_features)?) as Box<dyn NetBackend + Send>
            }
        };

        Ok(Self {
            rx_q,
            tx_q,

            mem,
            backend,
            interrupt,

            rx_frame_buf: [0u8; MAX_BUFFER_SIZE],
            rx_frame_buf_len: 0,
            rx_has_deferred_frame: false,

            tx_frame_buf: [0u8; MAX_BUFFER_SIZE],
            tx_frame_len: 0,
            tx_iovec: Vec::with_capacity(QUEUE_SIZE as usize),
            stream_tx,
            tx_has_deferred_frame: false,
        })
    }

    pub fn run(self) {
        thread::Builder::new()
            .name("virtio-net worker".into())
            .spawn(|| self.work())
            .unwrap();
    }

    fn work(mut self) {
        #[cfg(target_os = "macos")]
        const TX_TIMER_FD: RawFd = -2;

        let virtq_rx_ev_fd = self.rx_q.event.as_raw_fd();
        let virtq_tx_ev_fd = self.tx_q.event.as_raw_fd();
        let backend_socket = self.backend.raw_socket_fd();

        let epoll = Epoll::new().unwrap();

        let _ = epoll.ctl(
            ControlOperation::Add,
            virtq_rx_ev_fd,
            &EpollEvent::new(EventSet::IN, virtq_rx_ev_fd as u64),
        );
        let _ = epoll.ctl(
            ControlOperation::Add,
            virtq_tx_ev_fd,
            &EpollEvent::new(EventSet::IN, virtq_tx_ev_fd as u64),
        );
        let _ = epoll.ctl(
            ControlOperation::Add,
            backend_socket,
            &EpollEvent::new(
                EventSet::IN | EventSet::OUT | EventSet::EDGE_TRIGGERED | EventSet::READ_HANG_UP,
                backend_socket as u64,
            ),
        );

        let mut epoll_events = vec![EpollEvent::new(EventSet::empty(), 0); 32];
        loop {
            match epoll.wait(epoll_events.len(), -1, epoll_events.as_mut_slice()) {
                Ok(ev_cnt) => {
                    for event in &epoll_events[0..ev_cnt] {
                        let source = event.fd();
                        let event_set = event.event_set();
                        match event_set {
                            EventSet::IN if source == virtq_rx_ev_fd => {
                                self.process_rx_queue_event();
                            }
                            EventSet::IN if source == virtq_tx_ev_fd => {
                                self.process_tx_queue_event();
                            }
                            _ if source == backend_socket => {
                                if event_set.contains(EventSet::HANG_UP)
                                    || event_set.contains(EventSet::READ_HANG_UP)
                                {
                                    log::error!(
                                        "Got {event_set:?} on backend fd, virtio-net will stop working"
                                    );
                                    eprintln!(
                                        "LIBKRUN VIRTIO-NET FATAL: Backend process seems to have quit or crashed! Networking is now disabled!"
                                    );
                                } else {
                                    if event_set.contains(EventSet::IN) {
                                        self.process_backend_socket_readable()
                                    }

                                    if event_set.contains(EventSet::OUT) {
                                        self.process_backend_socket_writeable()
                                    }
                                }
                            }
                            #[cfg(target_os = "macos")]
                            _ if event_set.is_empty() && source == TX_TIMER_FD => {
                                self.process_tx_loop();
                            }
                            _ => {
                                log::warn!(
                                    "Received unknown event: {event_set:?} from fd: {source:?}"
                                );
                            }
                        }
                    }

                    // Arm the retry timer after processing all events, so it
                    // reflects the final state of tx_has_deferred_frame.
                    #[cfg(target_os = "macos")]
                    if self.tx_has_deferred_frame {
                        let delay = self.backend.write_retry_delay_us();
                        if delay > 0 {
                            epoll.add_oneshot_timer(delay, TX_TIMER_FD as u64);
                        }
                    }
                }
                Err(e) => {
                    debug!("vsock: failed to consume muxer epoll event: {e}");
                }
            }
        }
    }

    pub(crate) fn process_rx_queue_event(&mut self) {
        if let Err(e) = self.rx_q.event.read() {
            log::error!("Failed to get rx event from queue: {e:?}");
        }
        if let Err(e) = self.process_rx_loop() {
            log::error!("Failed to process rx: {e:?} (triggered by queue event)")
        };
    }

    pub(crate) fn process_tx_queue_event(&mut self) {
        match self.tx_q.event.read() {
            Ok(_) => self.process_tx_loop(),
            Err(e) => {
                log::error!("Failed to get tx queue event from queue: {e:?}");
            }
        }
    }

    pub(crate) fn process_backend_socket_readable(&mut self) {
        crate::devices::virtio::memory_gate::external_wake(&self.mem, "network_rx");
        if let Err(e) = self.process_rx_loop() {
            log::error!("Failed to process rx: {e:?} (triggered by backend socket readable)");
        };
    }

    pub(crate) fn process_backend_socket_writeable(&mut self) {
        if self.stream_tx.is_some() {
            self.process_tx_loop();
            return;
        }
        match self
            .backend
            .try_finish_write(VNET_HDR_LEN, &self.tx_frame_buf[..self.tx_frame_len])
        {
            Ok(()) => self.process_tx_loop(),
            Err(WriteError::PartialWrite | WriteError::NothingWritten) => {}
            Err(e @ WriteError::Internal(_)) => {
                log::error!("Failed to finish write: {e:?}");
            }
            Err(e @ WriteError::ProcessNotRunning) => {
                log::debug!("Failed to finish write: {e:?}");
            }
        }
    }

    fn process_rx_loop(&mut self) -> result::Result<(), RxError> {
        loop {
            self.rx_q
                .queue
                .disable_notification(&self.mem)
                .map_err(RxError::QueueError)?;
            self.process_rx()?;
            // Only ask the guest to kick when a deferred frame needs new
            // buffers. Recheck after arming, closing the replenishment race.
            if !self.rx_has_deferred_frame
                || !self
                    .rx_q
                    .queue
                    .enable_notification(&self.mem)
                    .map_err(RxError::QueueError)?
            {
                return Ok(());
            }
        }
    }

    fn process_rx(&mut self) -> result::Result<(), RxError> {
        let mut signal_queue = false;
        // if we have a deferred frame we try to process it first,
        // if that is not possible, we don't continue processing other frames
        if self.rx_has_deferred_frame {
            if self.write_frame_to_guest() {
                self.rx_has_deferred_frame = false;
                signal_queue = true;
            } else {
                return Ok(());
            }
        }

        // Read as many frames as possible.
        let result = loop {
            match self.read_into_rx_frame_buf_from_backend() {
                Ok(()) => {
                    if self.write_frame_to_guest() {
                        signal_queue = true;
                    } else {
                        self.rx_has_deferred_frame = true;
                        break Ok(());
                    }
                }
                Err(ReadError::NothingRead) => break Ok(()),
                Err(e @ ReadError::Internal(_)) => break Err(RxError::Backend(e)),
            }
        };

        // At this point we processed as many Rx frames as possible.
        // We have to wake the guest if at least one descriptor chain has been used.
        if signal_queue
            && self
                .rx_q
                .queue
                .needs_notification(&self.mem)
                .map_err(RxError::QueueError)?
        {
            self.interrupt
                .try_signal_used_queue()
                .map_err(RxError::DeviceError)?;
        }

        result
    }

    fn process_tx_loop(&mut self) {
        loop {
            self.tx_q.queue.disable_notification(&self.mem).unwrap();

            self.tx_has_deferred_frame = match self.process_tx() {
                Err(TxError::Backend(WriteError::NothingWritten)) => true,
                Err(e) => {
                    log::error!("Failed to process tx: {e:?}");
                    false
                }
                _ => false,
            };

            let has_new_entries = self.tx_q.queue.enable_notification(&self.mem).unwrap();
            if self.tx_has_deferred_frame || !has_new_entries {
                break;
            }
        }
    }

    fn process_tx(&mut self) -> result::Result<(), TxError> {
        if self.stream_tx.is_some() {
            return self.process_stream_tx();
        }
        let tx_queue = &mut self.tx_q.queue;

        if self.backend.has_unfinished_write()
            && self
                .backend
                .try_finish_write(VNET_HDR_LEN, &self.tx_frame_buf[..self.tx_frame_len])
                .is_err()
        {
            log::trace!("Cannot process tx because of unfinished partial write!");
            return Ok(());
        }

        let mut raise_irq = false;
        let mut result = Ok(());

        while let Some(head) = tx_queue.pop(&self.mem) {
            let head_index = head.index;
            // Addresses below outlive the descriptor walk.
            let _memory_access = head.memory_access.clone();
            let mut next_desc = Some(head);

            self.tx_iovec.clear();
            while let Some(desc) = next_desc {
                if desc.is_write_only() {
                    self.tx_iovec.clear();
                    break;
                }
                self.tx_iovec.push((desc.addr, desc.len as usize));
                next_desc = desc.next_descriptor();
            }

            // Copy buffer from across multiple descriptors.
            let mut read_count = 0;
            for (desc_addr, desc_len) in self.tx_iovec.drain(..) {
                let limit = cmp::min(read_count + desc_len, self.tx_frame_buf.len());

                let read_result = self
                    .mem
                    .read_slice(&mut self.tx_frame_buf[read_count..limit], desc_addr);
                match read_result {
                    Ok(()) => {
                        read_count += limit - read_count;
                    }
                    Err(e) => {
                        log::error!("Failed to read slice: {e:?}");
                        read_count = 0;
                        break;
                    }
                }
            }

            self.tx_frame_len = read_count;
            match self
                .backend
                .write_frame(VNET_HDR_LEN, &mut self.tx_frame_buf[..read_count])
            {
                Ok(()) => {
                    self.tx_frame_len = 0;
                    tx_queue
                        .add_used(&self.mem, head_index, 0)
                        .map_err(TxError::QueueError)?;
                    raise_irq = true;
                }
                Err(WriteError::NothingWritten) => {
                    tx_queue.undo_pop();
                    result = Err(TxError::Backend(WriteError::NothingWritten));
                    break;
                }
                Err(WriteError::PartialWrite) => {
                    log::trace!("process_tx: partial write");
                    /*
                    This situation should be pretty rare, assuming reasonably sized socket buffers.
                    We have written only a part of a frame to the backend socket (the socket is full).

                    The frame we have read from the guest remains in tx_frame_buf, and will be sent
                    later.

                    Note that we cannot wait for the backend to process our sending frames, because
                    the backend could be blocked on sending a remainder of a frame to us - us waiting
                    for backend would cause a deadlock.
                     */
                    tx_queue
                        .add_used(&self.mem, head_index, 0)
                        .map_err(TxError::QueueError)?;
                    raise_irq = true;
                    break;
                }
                Err(e @ WriteError::Internal(_) | e @ WriteError::ProcessNotRunning) => {
                    return Err(TxError::Backend(e));
                }
            }
        }

        if raise_irq && tx_queue.needs_notification(&self.mem).unwrap() {
            self.interrupt
                .try_signal_used_queue()
                .map_err(TxError::DeviceError)?;
        }

        result
    }

    fn process_stream_tx(&mut self) -> result::Result<(), TxError> {
        let batch = self.stream_tx.as_mut().expect("stream TX enabled");
        let mut used = false;
        let result = (|| {
            loop {
                // Always finish an owned partial batch before accepting more
                // guest buffers. At most 32 bounded frames remain pending.
                batch
                    .flush(self.backend.raw_socket_fd())
                    .map_err(TxError::Backend)?;
                while !batch.full() {
                    let Some(head) = self.tx_q.queue.pop(&self.mem) else {
                        break;
                    };
                    let index = head.index;
                    if let Err(error) = batch.push(&self.mem, head) {
                        log::warn!("Rejecting malformed TX frame: {error}");
                    }
                    self.tx_q
                        .queue
                        .add_used(&self.mem, index, 0)
                        .map_err(TxError::QueueError)?;
                    used = true;
                }
                if batch.empty() {
                    return Ok(());
                }
            }
        })();
        if used
            && self
                .tx_q
                .queue
                .needs_notification(&self.mem)
                .map_err(TxError::QueueError)?
        {
            self.interrupt
                .try_signal_used_queue()
                .map_err(TxError::DeviceError)?;
        }
        result
    }

    // Copies a single frame from `self.rx_frame_buf` into the guest.
    fn write_frame_to_guest_impl(&mut self) -> result::Result<(), FrontendError> {
        let mut result: std::result::Result<(), FrontendError> = Ok(());

        let queue = &mut self.rx_q.queue;
        let head_descriptor = queue.pop(&self.mem).ok_or(FrontendError::EmptyQueue)?;
        let head_index = head_descriptor.index;

        let mut frame_slice = &self.rx_frame_buf[..self.rx_frame_buf_len];

        let frame_len = frame_slice.len();
        let mut maybe_next_descriptor = Some(head_descriptor);
        while let Some(descriptor) = &maybe_next_descriptor {
            if frame_slice.is_empty() {
                break;
            }

            if !descriptor.is_write_only() {
                result = Err(FrontendError::ReadOnlyDescriptor);
                break;
            }

            let len = std::cmp::min(frame_slice.len(), descriptor.len as usize);
            match self.mem.write_slice(&frame_slice[..len], descriptor.addr) {
                Ok(()) => {
                    frame_slice = &frame_slice[len..];
                }
                Err(e) => {
                    log::error!("Failed to write slice: {e:?}");
                    result = Err(FrontendError::GuestMemory(e));
                    break;
                }
            };

            maybe_next_descriptor = descriptor.next_descriptor();
        }
        if result.is_ok() && !frame_slice.is_empty() {
            log::warn!("Receiving buffer is too small to hold frame of current size");
            result = Err(FrontendError::DescriptorChainTooSmall);
        }

        // Mark the descriptor chain as used. If an error occurred, skip the descriptor chain.
        let used_len = if result.is_err() { 0 } else { frame_len as u32 };
        queue
            .add_used(&self.mem, head_index, used_len)
            .map_err(FrontendError::QueueError)?;
        result
    }

    // Copies a single frame from `self.rx_frame_buf` into the guest. In case of an error retries
    // the operation if possible. Returns true if the operation was successfull.
    fn write_frame_to_guest(&mut self) -> bool {
        let max_iterations = self.rx_q.queue.actual_size();
        for _ in 0..max_iterations {
            match self.write_frame_to_guest_impl() {
                Ok(()) => return true,
                Err(FrontendError::EmptyQueue) => {
                    // retry
                    continue;
                }
                Err(_) => {
                    // retry
                    continue;
                }
            }
        }

        false
    }

    /// Fills self.rx_frame_buf with an ethernet frame from backend and prepends virtio_net_hdr to it
    fn read_into_rx_frame_buf_from_backend(&mut self) -> result::Result<(), ReadError> {
        self.rx_frame_buf_len = self.backend.read_frame(&mut self.rx_frame_buf)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::legacy::DummyIrqChip;
    use crate::devices::virtio::VirtQueue;
    use crate::utils::eventfd::{EFD_NONBLOCK, EventFd};
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, atomic::Ordering};

    #[test]
    fn tx_batches_own_completed_guest_buffers_and_resume_after_backpressure() {
        use std::io::Read;
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x40000)]).unwrap();
        let rx = VirtQueue::new(GuestAddress(0), &mem, 64);
        let tx = VirtQueue::new(GuestAddress(0x1000), &mem, 64);
        let (local, mut peer) = UnixStream::pair().unwrap();
        local
            .set_write_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        peer.set_nonblocking(true).unwrap();
        let interrupt =
            InterruptTransport::new(DummyIrqChip::new().into(), "net-test".into()).unwrap();
        let mut worker = NetWorker::new(
            DeviceQueue::new(
                rx.create_queue(),
                Arc::new(EventFd::new(EFD_NONBLOCK).unwrap()),
            ),
            DeviceQueue::new(
                tx.create_queue(),
                Arc::new(EventFd::new(EFD_NONBLOCK).unwrap()),
            ),
            interrupt,
            mem.clone(),
            0,
            VirtioNetBackend::UnixstreamFd(Arc::new(local.into())),
        )
        .unwrap();
        let bytes: libc::c_int = 1024;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    worker.backend.raw_socket_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&bytes as *const libc::c_int).cast(),
                    std::mem::size_of_val(&bytes) as _,
                )
            },
            0
        );
        let mut expected = Vec::new();
        for sequence in 0..40usize {
            let address = 0x10000 + sequence as u64 * 2048;
            let mut frame = vec![0; VNET_HDR_LEN];
            frame.extend_from_slice(&[sequence as u8; 1514]);
            mem.write_slice(&frame, GuestAddress(address)).unwrap();
            tx.dtable[sequence].set(address, frame.len() as u32, 0, 0);
            tx.avail.ring[sequence].set(sequence as u16);
            expected.extend_from_slice(&1514u32.to_be_bytes());
            expected.extend_from_slice(&[sequence as u8; 1514]);
        }
        // Malformed guest descriptors complete without a panic or wire bytes.
        tx.dtable[40].set(0x30000, 32, 2, 0);
        tx.dtable[41].set(0x31000, 1, 0, 0);
        tx.avail.ring[40].set(40);
        tx.avail.ring[41].set(41);
        tx.avail.idx.set(42);
        worker.tx_q.event.write(1).unwrap();
        worker.process_tx_queue_event();
        assert_eq!(tx.used.idx.get(), 32);
        assert!(worker.tx_has_deferred_frame);
        // Guest is free to reuse completed buffers, even during a partial send.
        for sequence in 0..32 {
            mem.write_slice(&[0xee; 1526], GuestAddress(0x10000 + sequence * 2048))
                .unwrap();
        }
        let mut arrived = Vec::new();
        for _ in 0..10000 {
            let mut bytes = [0; 503];
            loop {
                match peer.read(&mut bytes) {
                    Ok(0) => panic!("unexpected stream EOF"),
                    Ok(length) => arrived.extend_from_slice(&bytes[..length]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    error => panic!("receive: {error:?}"),
                }
            }
            if arrived.len() == expected.len() {
                break;
            }
            worker.process_backend_socket_writeable();
        }
        assert_eq!(arrived, expected);
        assert_eq!(tx.used.idx.get(), 42);
        assert!(worker.stream_tx.as_ref().unwrap().empty());
        assert!(!worker.tx_has_deferred_frame);
    }

    #[test]
    fn rx_event_index_suppresses_extra_irq_and_deferred_completion_still_notifies() {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let rx = VirtQueue::new(GuestAddress(0), &mem, 8);
        let tx = VirtQueue::new(GuestAddress(0x1000), &mem, 8);
        let mut rx_queue = rx.create_queue();
        rx_queue.set_event_idx(true);
        let (local, mut peer) = UnixStream::pair().unwrap();
        let interrupt =
            InterruptTransport::new(DummyIrqChip::new().into(), "net-test".into()).unwrap();
        let mut worker = NetWorker::new(
            DeviceQueue::new(rx_queue, Arc::new(EventFd::new(EFD_NONBLOCK).unwrap())),
            DeviceQueue::new(
                tx.create_queue(),
                Arc::new(EventFd::new(EFD_NONBLOCK).unwrap()),
            ),
            interrupt.clone(),
            mem.clone(),
            0,
            VirtioNetBackend::UnixstreamFd(Arc::new(local.into())),
        )
        .unwrap();
        rx.dtable[0].set(0x4000, 2048, 2, 0);
        rx.avail.ring[0].set(0);
        rx.avail.idx.set(1);
        // The guest is already polling and only requests a notification after
        // the second used entry. The first packet must not inject an IRQ.
        rx.avail.event.set(1);
        let mut framed = 66u32.to_be_bytes().to_vec();
        framed.extend_from_slice(&[0x5a; 66]);
        peer.write_all(&framed).unwrap();
        worker.process_backend_socket_readable();
        assert_eq!(rx.used.idx.get(), 1);
        assert_eq!(interrupt.status().load(Ordering::SeqCst), 0);
        // Exhausted RX buffers must arm a guest kick, without losing the frame.
        peer.write_all(&framed).unwrap();
        worker.process_backend_socket_readable();
        assert!(worker.rx_has_deferred_frame);
        assert_eq!(rx.used.event.get(), 1);
        rx.dtable[1].set(0x5000, 2048, 2, 0);
        rx.avail.ring[1].set(1);
        rx.avail.idx.set(2);
        worker.rx_q.event.write(1).unwrap();
        worker.process_rx_queue_event();
        assert!(!worker.rx_has_deferred_frame);
        assert_eq!(rx.used.idx.get(), 2);
        assert_eq!(interrupt.status().load(Ordering::SeqCst), 1);
        let mut arrived = [0; 66];
        mem.read_slice(&mut arrived, GuestAddress(0x5000 + VNET_HDR_LEN as u64))
            .unwrap();
        assert_eq!(arrived, [0x5a; 66]);
    }
}
