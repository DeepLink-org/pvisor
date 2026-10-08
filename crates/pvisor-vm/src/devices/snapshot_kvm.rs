//! KVM MMIO and x86 port bus state; in-kernel irqchips belong to the VM snapshot.
//! CPU parking and RAM gating remain the VMM caller's responsibility.
use crate::devices::{
    legacy::{Cmos, CmosSnapshot, I8042Device, I8042Snapshot, Serial, SerialSnapshot},
    virtio::{MmioSnapshot, MmioTransport},
    Bus, BusDevice,
};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "state", deny_unknown_fields)]
pub enum BusDeviceSnapshot {
    Virtio(MmioSnapshot),
    Cmos(CmosSnapshot),
    I8042(I8042Snapshot),
    Serial(SerialSnapshot),
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusMappingSnapshot {
    pub base: u64,
    pub len: u64,
    pub device: BusDeviceSnapshot,
}

fn supported(device: &dyn BusDevice) -> bool {
    let any = device.as_any();
    any.is::<MmioTransport>() || any.is::<Cmos>() || any.is::<I8042Device>() || any.is::<Serial>()
}

impl Bus {
    /// Poll while CPUs are parked, releasing the VMM lock between attempts.
    /// Never close the RAM gate until every worker has stopped. Unsupported
    /// mappings are detected before requesting any device to freeze.
    pub fn freeze_snapshot_devices(&self) -> Result<bool, String> {
        for (base, _, device) in self.mapped_devices() {
            let device = match device.try_lock() {
                Ok(device) => device,
                Err(std::sync::TryLockError::WouldBlock) => return Ok(false),
                Err(_) => return Err(format!("poisoned bus device at {base:#x}")),
            };
            if !supported(&*device) {
                return Err(format!("unsupported snapshot device at {base:#x}"));
            }
        }
        let mut ready = true;
        for (base, _, device) in self.mapped_devices() {
            let mut device = match device.try_lock() {
                Ok(device) => device,
                Err(std::sync::TryLockError::WouldBlock) => {
                    ready = false;
                    continue;
                }
                Err(_) => return Err(format!("poisoned bus device at {base:#x}")),
            };
            let any = device.as_mut_any();
            if let Some(virtio) = any.downcast_mut::<MmioTransport>() {
                ready &= virtio.freeze()?;
            } else if let Some(serial) = any.downcast_mut::<Serial>() {
                serial.capture_state()?;
            }
        }
        Ok(ready)
    }

    /// Caller opens RAM gate first and resumes CPU only after this succeeds.
    pub fn thaw_snapshot_devices(&self) -> Result<(), String> {
        for (base, _, device) in self.mapped_devices() {
            let mut device = device
                .try_lock()
                .map_err(|_| format!("busy/poisoned device at {base:#x}"))?;
            let any = device.as_mut_any();
            if let Some(virtio) = any.downcast_mut::<MmioTransport>() {
                virtio.thaw()?;
            }
        }
        Ok(())
    }

    /// All device workers must be frozen and all CPU MMIO stopped.
    pub fn capture_snapshot_devices(&self) -> Result<Vec<BusMappingSnapshot>, String> {
        self.mapped_devices()
            .map(|(base, len, device)| {
                let mut device = device
                    .try_lock()
                    .map_err(|_| format!("busy/poisoned device at {base:#x}"))?;
                let any = device.as_mut_any();
                let device = if let Some(device) = any.downcast_mut::<MmioTransport>() {
                    BusDeviceSnapshot::Virtio(device.capture_state()?)
                } else if let Some(device) = any.downcast_mut::<Cmos>() {
                    BusDeviceSnapshot::Cmos(device.capture_state()?)
                } else if let Some(device) = any.downcast_mut::<I8042Device>() {
                    BusDeviceSnapshot::I8042(device.capture_state()?)
                } else if let Some(device) = any.downcast_mut::<Serial>() {
                    BusDeviceSnapshot::Serial(device.capture_state()?)
                } else {
                    return Err(format!("unsupported snapshot device at {base:#x}"));
                };
                Ok(BusMappingSnapshot { base, len, device })
            })
            .collect()
    }

    /// Fresh topology and RAM must already exist. On error discard the whole
    /// destination; partial restore must never become runnable.
    pub fn restore_snapshot_devices(&self, states: &[BusMappingSnapshot]) -> Result<(), String> {
        if states.len() != self.mapped_devices().count()
            || states
                .iter()
                .zip(self.mapped_devices())
                .any(|(state, (base, len, _))| state.base != base || state.len != len)
        {
            return Err("snapshot bus mapping topology mismatch".into());
        }
        // Check all type bindings before mutating the first destination device.
        for (state, (_, _, device)) in states.iter().zip(self.mapped_devices()) {
            let device = device
                .try_lock()
                .map_err(|_| "busy/poisoned destination device")?;
            let any = device.as_any();
            let matches = match &state.device {
                BusDeviceSnapshot::Virtio(_) => any.is::<MmioTransport>(),
                BusDeviceSnapshot::Cmos(_) => any.is::<Cmos>(),
                BusDeviceSnapshot::I8042(_) => any.is::<I8042Device>(),
                BusDeviceSnapshot::Serial(_) => any.is::<Serial>(),
            };
            if !matches {
                return Err("snapshot bus device type mismatch".into());
            }
        }
        for (state, (_, _, device)) in states.iter().zip(self.mapped_devices()) {
            let mut device = device
                .try_lock()
                .map_err(|_| "busy/poisoned destination device")?;
            let any = device.as_mut_any();
            match &state.device {
                BusDeviceSnapshot::Virtio(state) => any
                    .downcast_mut::<MmioTransport>()
                    .ok_or("virtio topology changed")?
                    .restore_state(state)?,
                BusDeviceSnapshot::Cmos(state) => any
                    .downcast_mut::<Cmos>()
                    .ok_or("CMOS topology changed")?
                    .restore_state(state)?,
                BusDeviceSnapshot::I8042(state) => any
                    .downcast_mut::<I8042Device>()
                    .ok_or("i8042 topology changed")?
                    .restore_state(state)?,
                BusDeviceSnapshot::Serial(state) => {
                    let serial = any
                        .downcast_mut::<Serial>()
                        .ok_or("serial topology changed")?;
                    serial.restore_state(state)?;
                }
            }
        }
        Ok(())
    }
}
