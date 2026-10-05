use std::convert::TryInto;

use crate::utils::eventfd::EventFd;
use vm_memory::{ByteValued, GuestMemory, GuestMemoryMmap};

use super::super::{
    ActivateError, ActivateResult, BalloonError, DeviceQueue, DeviceState, QueueConfig,
    VirtioDevice,
};
use super::{defs, defs::uapi};
use crate::devices::virtio::InterruptTransport;

// Inflate queue.
pub(crate) const IFQ_INDEX: usize = 0;
// Deflate queue.
pub(crate) const DFQ_INDEX: usize = 1;
// Stats queue.
pub(crate) const STQ_INDEX: usize = 2;
// Page-hinting queue.
pub(crate) const PHQ_INDEX: usize = 3;
// Free page reporting queue.
pub(crate) const FRQ_INDEX: usize = 4;

// Supported features.
pub(crate) const AVAIL_FEATURES: u64 = (1 << uapi::VIRTIO_F_VERSION_1 as u64)
    | (1 << uapi::VIRTIO_BALLOON_F_STATS_VQ as u64)
    | (1 << uapi::VIRTIO_BALLOON_F_FREE_PAGE_HINT as u64)
    | (1 << uapi::VIRTIO_BALLOON_F_REPORTING as u64);

#[derive(Copy, Clone, Debug, Default)]
#[repr(C, packed)]
pub struct VirtioBalloonConfig {
    /* Number of pages host wants Guest to give up. */
    num_pages: u32,
    /* Number of pages we've actually got in balloon. */
    actual: u32,
    /* Free page report command id, readonly by guest */
    free_page_report_cmd_id: u32,
    /* Stores PAGE_POISON if page poisoning is in use */
    poison_val: u32,
}

// Safe because it only has data and has no implicit padding.
unsafe impl ByteValued for VirtioBalloonConfig {}

pub struct Balloon {
    pub(crate) queues: Option<Vec<DeviceQueue>>,
    pub(crate) avail_features: u64,
    pub(crate) acked_features: u64,
    pub(crate) activate_evt: EventFd,
    pub(crate) device_state: DeviceState,
    frozen: bool,
    config: VirtioBalloonConfig,
}

impl Balloon {
    pub fn new() -> super::Result<Balloon> {
        Ok(Balloon {
            queues: None,
            avail_features: AVAIL_FEATURES,
            acked_features: 0,
            activate_evt: EventFd::new(crate::utils::eventfd::EFD_NONBLOCK)
                .map_err(BalloonError::EventFd)?,
            device_state: DeviceState::Inactive,
            frozen: false,
            config: VirtioBalloonConfig::default(),
        })
    }

    pub fn id(&self) -> &str {
        defs::BALLOON_DEV_ID
    }

    pub fn process_frq(&mut self) -> bool {
        if self.frozen {
            return false;
        }
        debug!("balloon: process_frq()");
        let mem = match self.device_state {
            DeviceState::Activated(ref mem, _) => mem,
            // This should never happen, it's been already validated in the event handler.
            DeviceState::Inactive => unreachable!(),
        };

        let queues = self
            .queues
            .as_mut()
            .expect("queues should exist when activated");
        let mut have_used = false;

        while let Some(head) = queues[FRQ_INDEX].queue.pop(mem) {
            let index = head.index;
            for desc in head.into_iter() {
                let host_addr = mem.get_host_address(desc.addr).unwrap();
                debug!(
                    "balloon: should release guest_addr={:?} host_addr={:p} len={}",
                    desc.addr, host_addr, desc.len
                );
                #[cfg(target_os = "linux")]
                let advice = libc::MADV_DONTNEED;
                #[cfg(target_os = "macos")]
                let advice = libc::MADV_FREE;
                unsafe {
                    libc::madvise(
                        host_addr as *mut libc::c_void,
                        desc.len.try_into().unwrap(),
                        advice,
                    )
                };
            }

            have_used = true;
            if let Err(e) = queues[FRQ_INDEX].queue.add_used(mem, index, 0) {
                error!("failed to add used elements to the queue: {e:?}");
            }
        }

        have_used
    }
}

impl VirtioDevice for Balloon {
    fn freeze(&mut self) -> Result<bool, String> {
        self.frozen = true;
        Ok(true)
    }
    fn thaw(&mut self) -> Result<(), String> {
        if self.frozen {
            if let Some(queues) = &self.queues {
                queues[FRQ_INDEX]
                    .event
                    .write(1)
                    .map_err(|e| e.to_string())?;
            }
            self.frozen = false;
        }
        Ok(())
    }

    fn capture_state(&self) -> Result<super::super::DeviceSnapshot, String> {
        if self.is_activated() && !self.frozen {
            return Err("device must be frozen before capture".into());
        }
        Ok(super::super::DeviceSnapshot {
            queues: self
                .queues
                .as_ref()
                .map(|queues| queues.iter().map(|dq| dq.queue.capture_state()).collect()),
            state: super::super::DeviceSnapshotState::Balloon([
                self.config.num_pages,
                self.config.actual,
                self.config.free_page_report_cmd_id,
                self.config.poison_val,
            ]),
        })
    }

    fn restore_state(&mut self, state: &super::super::DeviceSnapshotState) -> Result<(), String> {
        if self.is_activated() {
            return Err("restore requires a fresh device".into());
        }
        match state {
            super::super::DeviceSnapshotState::Balloon(values) => {
                self.frozen = true;
                self.config = VirtioBalloonConfig {
                    num_pages: values[0],
                    actual: values[1],
                    free_page_report_cmd_id: values[2],
                    poison_val: values[3],
                };
                Ok(())
            }
            _ => Err("balloon state type mismatch".into()),
        }
    }

    fn avail_features(&self) -> u64 {
        self.avail_features
    }

    fn acked_features(&self) -> u64 {
        self.acked_features
    }

    fn set_acked_features(&mut self, acked_features: u64) {
        self.acked_features = acked_features
    }

    fn device_type(&self) -> u32 {
        uapi::VIRTIO_ID_BALLOON
    }

    fn device_name(&self) -> &str {
        "balloon"
    }

    fn queue_config(&self) -> &[QueueConfig] {
        &defs::QUEUE_CONFIG
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        super::super::device::read_config_space(self.config.as_slice(), offset, data);
    }

    fn write_config(&mut self, offset: u64, data: &[u8]) {
        warn!(
            "balloon: guest driver attempted to write device config (offset={:x}, len={:x})",
            offset,
            data.len()
        );
    }

    fn activate(
        &mut self,
        mem: GuestMemoryMmap,
        interrupt: InterruptTransport,
        queues: Vec<DeviceQueue>,
    ) -> ActivateResult {
        if queues.len() != defs::NUM_QUEUES {
            error!(
                "Cannot perform activate. Expected {} queue(s), got {}",
                defs::NUM_QUEUES,
                queues.len()
            );
            return Err(ActivateError::BadActivate);
        }

        if self.activate_evt.write(1).is_err() {
            error!("Cannot write to activate_evt",);
            return Err(ActivateError::BadActivate);
        }

        self.queues = Some(queues);
        self.device_state = DeviceState::Activated(mem, interrupt);

        Ok(())
    }

    fn is_activated(&self) -> bool {
        self.device_state.is_activated()
    }
}
