// SPDX-License-Identifier: MPL-2.0

use alloc::{boxed::Box, sync::Arc};
use core::mem::offset_of;

use aster_rights::{ReadOp, WriteOp};
use aster_util::{field_ptr, safe_ptr::SafePtr};
use ostd::{
    io::IoMem,
    irq::IrqCallbackFunction,
    mm::{HasDaddr, PAGE_SIZE, dma::DmaCoherent},
    sync::{RwLock, WriteIrqDisabled},
    warn,
};

use super::{
    bus::{
        bus::MmioDevice,
        common_device::{MmioCommonDevice, VirtioMmioVersion},
    },
    layout::VirtioMmioLayout,
    multiplex::MultiplexIrq,
};
use crate::{
    VirtioDeviceType,
    queue::{AvailRing, Descriptor, UsedRing},
    transport::{ConfigManager, DeviceStatus, VirtioTransport, VirtioTransportError},
};

const FS_IRQ_READ_ACK: u64 = 1 << 22;
const FS_IRQ_TAKE: usize = 0x1fc;

#[derive(Debug)]
pub(super) struct VirtioMmioDevice {
    device_id: u32,
}

#[derive(Debug)]
pub(crate) struct VirtioMmioTransport {
    layout: SafePtr<VirtioMmioLayout, IoMem>,
    device: Arc<VirtioMmioDevice>,
    common_device: MmioCommonDevice,
    multiplex: Arc<RwLock<MultiplexIrq, WriteIrqDisabled>>,
    fs_irq_read_ack_selected: bool,
}

impl MmioDevice for VirtioMmioDevice {
    fn device_id(&self) -> u32 {
        self.device_id
    }
}

impl MmioDevice for VirtioMmioTransport {
    fn device_id(&self) -> u32 {
        self.device.device_id()
    }
}

impl VirtioMmioTransport {
    pub(super) fn mmio_device(&self) -> &Arc<VirtioMmioDevice> {
        &self.device
    }

    pub(super) fn new(device: MmioCommonDevice) -> Self {
        let irq = device.irq().clone();
        let layout = SafePtr::new(device.io_mem().clone(), 0);
        let device_id = device.read_device_id().unwrap();
        let (interrupt_ack, interrupt_status) = {
            let interrupt_ack_offset = offset_of!(VirtioMmioLayout, interrupt_ack);
            let interrupt_status_offset = offset_of!(VirtioMmioLayout, interrupt_status);
            let mut interrupt_ack = layout.clone();
            interrupt_ack.byte_add(interrupt_ack_offset);
            let mut interrupt_status = layout.clone();
            interrupt_status.byte_add(interrupt_status_offset);
            (
                interrupt_ack.cast::<u32>().restrict::<WriteOp>(),
                interrupt_status.cast::<u32>().restrict::<ReadOp>(),
            )
        };
        let device = Self {
            layout,
            device: Arc::new(VirtioMmioDevice { device_id }),
            common_device: device,
            multiplex: MultiplexIrq::new(irq, interrupt_ack, interrupt_status),
            fs_irq_read_ack_selected: false,
        };
        if device.common_device.read_version().unwrap() == VirtioMmioVersion::Legacy {
            field_ptr!(&device.layout, VirtioMmioLayout, legacy_guest_page_size)
                .write_once(&(PAGE_SIZE as u32))
                .unwrap();
        }
        device
    }
}

impl VirtioTransport for VirtioMmioTransport {
    fn device_type(&self) -> VirtioDeviceType {
        VirtioDeviceType::try_from(self.device.device_id() as u8).unwrap()
    }

    fn set_queue(
        &mut self,
        idx: u16,
        queue_size: u16,
        descriptor_ptr: &SafePtr<Descriptor, Arc<DmaCoherent>>,
        driver_ptr: &SafePtr<AvailRing, Arc<DmaCoherent>>,
        device_ptr: &SafePtr<UsedRing, Arc<DmaCoherent>>,
    ) -> Result<(), VirtioTransportError> {
        field_ptr!(&self.layout, VirtioMmioLayout, queue_sel)
            .write_once(&(idx as u32))
            .unwrap();

        let queue_num_max: u32 = field_ptr!(&self.layout, VirtioMmioLayout, queue_num_max)
            .read_once()
            .unwrap();

        if queue_size as u32 > queue_num_max {
            warn!("Set queue failed, queue size is bigger than maximum virtual queue size.");
            return Err(VirtioTransportError::InvalidArgs);
        }

        let descriptor_daddr = descriptor_ptr.daddr();
        let driver_daddr = driver_ptr.daddr();
        let device_daddr = device_ptr.daddr();

        field_ptr!(&self.layout, VirtioMmioLayout, queue_num)
            .write_once(&(queue_size as u32))
            .unwrap();

        match self.common_device.read_version().unwrap() {
            VirtioMmioVersion::Legacy => {
                // The area should be continuous.
                assert_eq!(
                    driver_daddr - descriptor_daddr,
                    size_of::<Descriptor>() * queue_size as usize
                );
                // Descriptor device addresses should be aligned.
                assert_eq!(descriptor_daddr % PAGE_SIZE, 0);
                let pfn = (descriptor_daddr / PAGE_SIZE) as u32;
                field_ptr!(&self.layout, VirtioMmioLayout, legacy_queue_align)
                    .write_once(&(PAGE_SIZE as u32))
                    .unwrap();
                field_ptr!(&self.layout, VirtioMmioLayout, legacy_queue_pfn)
                    .write_once(&pfn)
                    .unwrap();
            }
            VirtioMmioVersion::Modern => {
                field_ptr!(&self.layout, VirtioMmioLayout, queue_desc_low)
                    .write_once(&(descriptor_daddr as u32))
                    .unwrap();
                field_ptr!(&self.layout, VirtioMmioLayout, queue_desc_high)
                    .write_once(&((descriptor_daddr >> 32) as u32))
                    .unwrap();

                field_ptr!(&self.layout, VirtioMmioLayout, queue_driver_low)
                    .write_once(&(driver_daddr as u32))
                    .unwrap();
                field_ptr!(&self.layout, VirtioMmioLayout, queue_driver_high)
                    .write_once(&((driver_daddr >> 32) as u32))
                    .unwrap();

                field_ptr!(&self.layout, VirtioMmioLayout, queue_device_low)
                    .write_once(&(device_daddr as u32))
                    .unwrap();
                field_ptr!(&self.layout, VirtioMmioLayout, queue_device_high)
                    .write_once(&((device_daddr >> 32) as u32))
                    .unwrap();
                // enable queue
                field_ptr!(&self.layout, VirtioMmioLayout, queue_sel)
                    .write_once(&(idx as u32))
                    .unwrap();
                field_ptr!(&self.layout, VirtioMmioLayout, queue_ready)
                    .write_once(&1u32)
                    .unwrap();
            }
        };
        Ok(())
    }

    fn notify_config(&self, _idx: usize) -> ConfigManager<u32> {
        let offset = offset_of!(VirtioMmioLayout, queue_notify);
        let safe_ptr = Some(SafePtr::new(self.common_device.io_mem().clone(), offset));

        ConfigManager::new(safe_ptr, None)
    }

    fn num_queues(&self) -> u16 {
        // We use the field `queue_num_max` to get queue size.
        // If the queue is not exists, the field should be zero
        let mut num_queues = 0;
        const MAX_QUEUES: u32 = 512;
        while num_queues < MAX_QUEUES {
            field_ptr!(&self.layout, VirtioMmioLayout, queue_sel)
                .write_once(&num_queues)
                .unwrap();
            if field_ptr!(&self.layout, VirtioMmioLayout, queue_num_max)
                .read_once()
                .unwrap()
                == 0u32
            {
                return num_queues as u16;
            }
            num_queues += 1;
        }
        todo!()
    }

    fn device_config_mem(&self) -> Option<IoMem> {
        // offset: 0x100~0x200
        Some(self.common_device.io_mem().slice(0x100..0x200))
    }

    fn device_config_bar(&self) -> Option<(aster_pci::cfg_space::BarAccess, usize)> {
        None
    }

    fn read_device_features(&self) -> u64 {
        // select low
        field_ptr!(&self.layout, VirtioMmioLayout, device_features_select)
            .write_once(&0u32)
            .unwrap();
        let device_feature_low = field_ptr!(&self.layout, VirtioMmioLayout, device_features)
            .read_once()
            .unwrap();
        // select high
        field_ptr!(&self.layout, VirtioMmioLayout, device_features_select)
            .write_once(&1u32)
            .unwrap();
        let device_feature_high = field_ptr!(&self.layout, VirtioMmioLayout, device_features)
            .read_once()
            .unwrap() as u64;
        (device_feature_high << 32) | device_feature_low as u64
    }

    fn select_transport_features(&self, offered: u64) -> u64 {
        if cfg!(target_arch = "x86_64")
            && offered & FS_IRQ_READ_ACK != 0
            && self.device_type() == VirtioDeviceType::FileSystem
            && !self.is_legacy_version()
            && field_ptr!(&self.layout, VirtioMmioLayout, vendor_id)
                .read_once()
                .unwrap()
                == u32::from_le_bytes(*b"pVis")
        {
            FS_IRQ_READ_ACK
                | if ostd::cpu::num_cpus() == 1 {
                    offered & crate::transport::FS_QUEUE_HANDOFF
                } else {
                    0
                }
        } else {
            0
        }
    }

    fn write_driver_features(&mut self, features: u64) -> Result<(), VirtioTransportError> {
        self.fs_irq_read_ack_selected =
            self.device_type() == VirtioDeviceType::FileSystem && features & FS_IRQ_READ_ACK != 0;
        let low = features as u32;
        let high = (features >> 32) as u32;
        field_ptr!(&self.layout, VirtioMmioLayout, driver_features_select)
            .write_once(&0u32)
            .unwrap();
        field_ptr!(&self.layout, VirtioMmioLayout, driver_features)
            .write_once(&low)
            .unwrap();
        field_ptr!(&self.layout, VirtioMmioLayout, driver_features_select)
            .write_once(&1u32)
            .unwrap();
        field_ptr!(&self.layout, VirtioMmioLayout, driver_features)
            .write_once(&high)
            .unwrap();
        Ok(())
    }

    fn read_device_status(&self) -> DeviceStatus {
        DeviceStatus::from_bits(
            field_ptr!(&self.layout, VirtioMmioLayout, status)
                .read_once()
                .unwrap() as u8,
        )
        .unwrap()
    }

    fn write_device_status(&mut self, status: DeviceStatus) -> Result<(), VirtioTransportError> {
        // Serialize changing the ISR operation with handlers on every vCPU.
        let mut irq = self.multiplex.write();
        field_ptr!(&self.layout, VirtioMmioLayout, status)
            .write_once(&(status.bits() as u32))
            .unwrap();
        if status.is_empty() || status.contains(DeviceStatus::FAILED) {
            self.fs_irq_read_ack_selected = false;
        }
        let ready = DeviceStatus::FEATURES_OK | DeviceStatus::DRIVER_OK;
        irq.read_ack = if self.fs_irq_read_ack_selected
            && status.contains(ready)
            && (self.read_device_status() & (ready | DeviceStatus::FAILED)) == ready
        {
            Some(
                SafePtr::new(self.common_device.io_mem().clone(), FS_IRQ_TAKE).restrict::<ReadOp>(),
            )
        } else {
            None
        };
        Ok(())
    }

    fn is_legacy_version(&self) -> bool {
        self.common_device.read_version().unwrap() == VirtioMmioVersion::Legacy
    }

    fn max_queue_size(&self, idx: u16) -> Result<u16, VirtioTransportError> {
        field_ptr!(&self.layout, VirtioMmioLayout, queue_sel)
            .write_once(&(idx as u32))
            .unwrap();
        Ok(field_ptr!(&self.layout, VirtioMmioLayout, queue_num_max)
            .read_once()
            .unwrap() as u16)
    }

    fn register_queue_callback(
        &mut self,
        _index: u16,
        func: Box<IrqCallbackFunction>,
        single_interrupt: bool,
    ) -> Result<(), VirtioTransportError> {
        if single_interrupt {
            warn!(
                "{:?}: `single_interrupt` ignored: no support for virtio-mmio devices",
                self.device_type()
            );
        }
        self.multiplex.write().register_queue_callback(func);
        Ok(())
    }

    fn register_cfg_callback(
        &mut self,
        func: Box<IrqCallbackFunction>,
    ) -> Result<(), VirtioTransportError> {
        self.multiplex.write().register_cfg_callback(func);
        Ok(())
    }
}
