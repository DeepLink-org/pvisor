// SPDX-License-Identifier: MPL-2.0

//! Virtio-console control handshake for the primary pVisor console.

use alloc::{boxed::Box, sync::Arc};
use core::{
    hint::spin_loop,
    sync::atomic::{AtomicBool, Ordering},
};

use aster_util::mem_obj_slice::Slice;
use ostd::{
    arch::trap::TrapFrame,
    mm::{dma::DmaStream, io::util::HasVmReaderWriter},
    sync::SpinLock,
};

use crate::{device::VirtioDeviceError, queue::VirtQueue, transport::VirtioTransport};

pub(super) struct ConsoleControl {
    receive: SpinLock<VirtQueue>,
    transmit: SpinLock<VirtQueue>,
    receive_buffer: DmaStream,
    send_buffer: DmaStream,
    primary_open: AtomicBool,
}

impl ConsoleControl {
    pub(super) fn new(transport: &mut dyn VirtioTransport) -> Result<Arc<Self>, VirtioDeviceError> {
        let control = Arc::new(Self {
            receive: SpinLock::new(VirtQueue::new(2, 16, transport)?),
            transmit: SpinLock::new(VirtQueue::new(3, 16, transport)?),
            receive_buffer: DmaStream::alloc(1, false).map_err(VirtioDeviceError::ResourceAlloc)?,
            send_buffer: DmaStream::alloc(1, false).map_err(VirtioDeviceError::ResourceAlloc)?,
            primary_open: AtomicBool::new(false),
        });
        control.post_receive()?;
        let callback = control.clone();
        transport.register_queue_callback(
            2,
            Box::new(move |_: &TrapFrame| callback.handle_irq()),
            false,
        )?;
        Ok(control)
    }

    pub(super) fn start(&self) -> Result<(), VirtioDeviceError> {
        self.receive.disable_irq().lock().notify();
        self.send(0, 0, 1)?;
        for _ in 0..10_000_000 {
            self.handle_irq();
            if self.primary_open.load(Ordering::Acquire) {
                return Ok(());
            }
            spin_loop();
        }
        Err(VirtioDeviceError::UnsupportedConfig)
    }

    fn post_receive(&self) -> Result<(), VirtioDeviceError> {
        self.receive
            .disable_irq()
            .lock()
            .add_output_bufs(&[&Slice::new(&self.receive_buffer, 0..4096)])
            .map_err(|_| VirtioDeviceError::InvalidQueueArgs)?;
        Ok(())
    }

    fn send(&self, id: u32, event: u16, value: u16) -> Result<(), VirtioDeviceError> {
        let mut queue = self.transmit.disable_irq().lock();
        let bytes = (id as u64) | ((event as u64) << 32) | ((value as u64) << 48);
        self.send_buffer
            .writer()
            .unwrap()
            .write_val(&bytes)
            .unwrap();
        self.send_buffer.sync_to_device(0..8).unwrap();
        queue
            .add_input_bufs(&[&Slice::new(&self.send_buffer, 0..8)])
            .map_err(|_| VirtioDeviceError::InvalidQueueArgs)?;
        queue.notify();
        for _ in 0..10_000_000 {
            if queue.pop_used().is_ok() {
                return Ok(());
            }
            spin_loop();
        }
        Err(VirtioDeviceError::UnsupportedConfig)
    }

    fn handle_irq(&self) {
        let message = {
            let mut queue = self.receive.disable_irq().lock();
            let Ok((_, length)) = queue.pop_used() else {
                return;
            };
            let message = if (8..=4096).contains(&length) {
                self.receive_buffer
                    .sync_from_device(0..length as usize)
                    .unwrap();
                Some(
                    self.receive_buffer
                        .reader()
                        .unwrap()
                        .read_val::<[u8; 8]>()
                        .unwrap(),
                )
            } else {
                None
            };
            queue
                .add_output_bufs(&[&Slice::new(&self.receive_buffer, 0..4096)])
                .expect("completed control descriptor must be reusable");
            queue.notify();
            message
        };
        let Some(message) = message else { return };
        let id = u32::from_le_bytes(message[..4].try_into().unwrap());
        let event = u16::from_le_bytes(message[4..6].try_into().unwrap());
        let value = u16::from_le_bytes(message[6..].try_into().unwrap());
        if id == 0 && event == 1 {
            let _ = self.send(id, 3, 1);
            let _ = self.send(id, 6, 1);
        } else if id == 0 && event == 6 {
            self.primary_open.store(value == 1, Ordering::Release);
        }
    }
}
