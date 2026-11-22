// SPDX-License-Identifier: MPL-2.0

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::{
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};

use aster_util::mem_obj_slice::Slice;
use ostd::{
    mm::{PAGE_SIZE, VmReader, VmWriter, dma::DmaStream, io::util::HasVmReaderWriter},
    sync::{SpinLock, WaitQueue},
};
use spin::Once;

use crate::{device::VirtioDeviceError, queue::VirtQueue, transport::VirtioTransport};

pub(crate) static PORTS: Once<Vec<Arc<NamedPort>>> = Once::new();

pub fn named_ports() -> &'static [Arc<NamedPort>] {
    PORTS.get().map(Vec::as_slice).unwrap_or(&[])
}

/// One raw virtio-serial stream; data never passes through the TTY line discipline.
pub struct NamedPort {
    id: u16,
    pub(super) name: Once<String>,
    rx: SpinLock<(VirtQueue, Range<usize>)>,
    tx: SpinLock<Tx>,
    input: DmaStream,
    output: DmaStream,
    disconnected: AtomicBool,
    tx_wait: WaitQueue,
    on_event: Once<Box<dyn Fn() + Send + Sync>>,
}

struct Tx {
    queue: VirtQueue,
    submitted: u64,
    completed: u64,
}

impl NamedPort {
    pub(super) fn new(
        id: u16,
        transport: &mut dyn VirtioTransport,
    ) -> Result<Arc<Self>, VirtioDeviceError> {
        let rx_idx = 2 + 2 * id;
        let tx_idx = rx_idx + 1;
        let port = Arc::new(Self {
            id,
            name: Once::new(),
            rx: SpinLock::new((VirtQueue::new(rx_idx, 2, transport)?, 0..0)),
            tx: SpinLock::new(Tx {
                queue: VirtQueue::new(tx_idx, 2, transport)?,
                submitted: 0,
                completed: 0,
            }),
            input: DmaStream::alloc(1, false).map_err(VirtioDeviceError::ResourceAlloc)?,
            output: DmaStream::alloc(1, false).map_err(VirtioDeviceError::ResourceAlloc)?,
            disconnected: AtomicBool::new(false),
            tx_wait: WaitQueue::new(),
            on_event: Once::new(),
        });
        let reader = port.clone();
        transport.register_queue_callback(
            rx_idx,
            Box::new(move |_| {
                if reader.readable() {
                    reader.notify();
                }
            }),
            false,
        )?;
        let writer = port.clone();
        transport.register_queue_callback(
            tx_idx,
            Box::new(move |_| {
                let mut tx = writer.tx.disable_irq().lock();
                if tx.queue.pop_used().is_ok() {
                    tx.completed = tx.submitted;
                    drop(tx);
                    writer.tx_wait.wake_all();
                    writer.notify();
                }
            }),
            false,
        )?;
        port.post_read(&mut port.rx.disable_irq().lock().0);
        Ok(port)
    }

    fn post_read(&self, queue: &mut VirtQueue) {
        queue
            .add_output_bufs(&[&Slice::new(&self.input, 0..PAGE_SIZE)])
            .unwrap();
        queue.notify();
    }

    pub fn id(&self) -> u16 {
        self.id
    }
    pub fn name(&self) -> &str {
        self.name.get().unwrap()
    }

    pub fn on_event(&self, callback: Box<dyn Fn() + Send + Sync>) {
        self.on_event.call_once(|| callback);
    }

    fn notify(&self) {
        if let Some(callback) = self.on_event.get() {
            callback();
        }
    }

    pub(super) fn set_open(&self, open: bool) {
        self.disconnected.store(!open, Ordering::Release);
        if !open {
            self.notify();
            self.tx_wait.wake_all();
        }
    }

    pub fn closed(&self) -> bool {
        self.disconnected.load(Ordering::Acquire)
    }

    pub fn readable(&self) -> bool {
        let rx = self.rx.disable_irq().lock();
        !rx.1.is_empty() || rx.0.can_pop() || self.closed()
    }

    /// None means no bytes yet; zero means EOF after all received bytes are drained.
    pub fn try_read(&self, dst: &mut [u8]) -> Option<usize> {
        let mut rx = self.rx.disable_irq().lock();
        if rx.1.is_empty() {
            let Ok((_, len)) = rx.0.pop_used() else {
                return self.closed().then_some(0);
            };
            if len == 0 {
                self.disconnected.store(true, Ordering::Release);
                return Some(0);
            }
            rx.1 = 0..len as usize;
            self.input.sync_from_device(rx.1.clone()).unwrap();
        }
        let mut reader = self.input.reader().unwrap();
        reader.skip(rx.1.start);
        reader.limit(rx.1.len());
        let n = reader.read(&mut VmWriter::from(dst));
        rx.1.start += n;
        if rx.1.is_empty() && !self.closed() {
            self.post_read(&mut rx.0);
        }
        Some(n)
    }

    pub fn writable(&self) -> bool {
        let tx = self.tx.disable_irq().lock();
        tx.completed == tx.submitted && !self.closed()
    }

    /// A ticket identifies the submission even if another writer runs before its waiter.
    pub fn try_write(&self, src: &[u8]) -> Option<u64> {
        let mut tx = self.tx.disable_irq().lock();
        if tx.completed != tx.submitted || self.closed() {
            return None;
        }
        let n = self
            .output
            .writer()
            .unwrap()
            .write(&mut VmReader::from(src));
        self.output.sync_to_device(0..n).unwrap();
        tx.queue
            .add_input_bufs(&[&Slice::new(&self.output, 0..n)])
            .unwrap();
        tx.submitted += 1;
        tx.queue.notify();
        Some(tx.submitted)
    }

    pub fn wait_written(&self, ticket: u64) -> bool {
        self.tx_wait.wait_until(|| {
            if self.tx.disable_irq().lock().completed >= ticket {
                Some(true)
            } else {
                self.closed().then_some(false)
            }
        })
    }
}
