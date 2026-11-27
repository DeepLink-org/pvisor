// SPDX-License-Identifier: MPL-2.0

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::fmt::Debug;

use aster_rights::{ReadOp, TRightSet, WriteOp};
use aster_util::safe_ptr::SafePtr;
use ostd::{
    arch::trap::TrapFrame,
    io::IoMem,
    irq::{IrqCallbackFunction, IrqLine},
    sync::{RwLock, WriteIrqDisabled},
};

/// Dispatch the acknowledged causes of a shared virtio-mmio interrupt.
pub(super) struct MultiplexIrq {
    irq: IrqLine,
    queue_callbacks: Vec<Box<IrqCallbackFunction>>,
    cfg_callbacks: Vec<Box<IrqCallbackFunction>>,
    interrupt_ack: SafePtr<u32, IoMem, TRightSet<WriteOp>>,
    interrupt_status: SafePtr<u32, IoMem, TRightSet<ReadOp>>,
    pub(super) read_ack: Option<SafePtr<u32, IoMem, TRightSet<ReadOp>>>,
}

impl MultiplexIrq {
    pub(super) fn new(
        irq: IrqLine,
        interrupt_ack: SafePtr<u32, IoMem, TRightSet<WriteOp>>,
        interrupt_status: SafePtr<u32, IoMem, TRightSet<ReadOp>>,
    ) -> Arc<RwLock<Self, WriteIrqDisabled>> {
        let irq = Arc::new(RwLock::new(Self {
            irq,
            queue_callbacks: Vec::new(),
            cfg_callbacks: Vec::new(),
            interrupt_ack,
            interrupt_status,
            read_ack: None,
        }));

        // Holding a weak reference to prevent memory leakage due to
        // circular reference.
        let weak = Arc::downgrade(&irq);
        let callback = move |trap_frame: &TrapFrame| {
            let Some(multiplex_irq) = weak.upgrade() else {
                return;
            };
            let irq = multiplex_irq.read();

            // Acknowledge before draining rings: a later completion needs its own notification.
            let interrupt_status = if let Some(read_ack) = &irq.read_ack {
                read_ack.read_once().unwrap()
            } else {
                let status = irq.interrupt_status.read_once().unwrap();
                irq.interrupt_ack.write_once(&status).unwrap();
                status
            };

            // Used buffer notification
            if interrupt_status & 0x01 != 0 {
                for callback in irq.queue_callbacks.iter() {
                    (callback)(trap_frame);
                }
            }

            // Configuration change notification
            if interrupt_status & 0x02 != 0 {
                for callback in irq.cfg_callbacks.iter() {
                    (callback)(trap_frame);
                }
            }
        };

        let mut lock = irq.write();
        lock.irq.on_active(callback);
        drop(lock);

        irq
    }

    pub(super) fn register_queue_callback(&mut self, func: Box<IrqCallbackFunction>) {
        self.queue_callbacks.push(func);
    }

    pub(super) fn register_cfg_callback(&mut self, func: Box<IrqCallbackFunction>) {
        self.cfg_callbacks.push(func);
    }
}

impl Debug for MultiplexIrq {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MultiplexIrq")
            .field("irq", &self.irq)
            .field("interrupt_ack", &self.interrupt_ack)
            .field("interrupt_status", &self.interrupt_status)
            .finish()
    }
}
