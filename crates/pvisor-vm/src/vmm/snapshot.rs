//! Restore input for the KVM/HVF builders. Durable publication, build
//! identity, backing-file sealing and execution ownership belong to the runner.
mod ram;

use crate::vmm::{CpuSnapshot, Vmm};
use std::{fs::File, sync::Arc};
use vm_memory::{GuestAddress, GuestMemoryMmap};

pub use crate::api::RamMappingSnapshot;

pub use crate::api::{RamDeltaCapture, RamDeltaSpec};
/// Payload geometry only. A restore never dereferences a fresh kernel pointer:
/// the captured RAM already includes the complete, possibly modified kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelLayout {
    pub guest_addr: u64,
    pub size: u64,
}

impl KernelLayout {
    pub fn validate(&self) -> Result<(), String> {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page <= 0
            || self.guest_addr == 0
            || self.size == 0
            || !self.guest_addr.is_multiple_of(page as u64)
            || !self.size.is_multiple_of(page as u64)
            || self.guest_addr.checked_add(self.size).is_none()
            || usize::try_from(self.size).is_err()
        {
            return Err("invalid snapshot bundled-kernel geometry".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineSnapshot {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_layout: Option<KernelLayout>,
    pub cpus: Vec<CpuSnapshot>,
    pub devices: Vec<crate::devices::snapshot::BusMappingSnapshot>,
    pub ram: Vec<RamMappingSnapshot>,
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub kvm: Option<crate::vmm::linux::vstate::VmState>,
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub pio_devices: Vec<crate::devices::snapshot::BusMappingSnapshot>,
}

pub struct MachineRestore {
    pub state: MachineSnapshot,
    pub ram_file: Arc<File>,
}

impl MachineRestore {
    pub fn validate(&self, vcpu_count: usize) -> Result<(), String> {
        if vcpu_count == 0
            || vcpu_count > 256
            || self.state.version != 1
            || self.state.cpus.len() != vcpu_count
            || self.state.ram.is_empty()
        {
            return Err("invalid machine snapshot version or CPU/RAM inventory".into());
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        if self.state.kvm.is_none() {
            return Err("missing KVM machine state".into());
        }
        for (id, cpu) in self.state.cpus.iter().enumerate() {
            cpu.validate(id as u8)?;
        }
        self.validate_ram_file()
    }

    pub fn validate_ram_file(&self) -> Result<(), String> {
        crate::memory::validate_snapshot_ram(&self.state.ram, &self.ram_file)?;
        if let Some(kernel) = self.state.kernel_layout {
            kernel.validate()?;
            if !self.state.ram.iter().any(|region| {
                #[cfg(target_arch = "x86_64")]
                {
                    region.base == kernel.guest_addr && region.len == kernel.size
                }
                #[cfg(not(target_arch = "x86_64"))]
                {
                    region.base <= kernel.guest_addr
                        && region.base + region.len >= kernel.guest_addr + kernel.size
                }
            }) {
                return Err("snapshot kernel geometry does not match captured RAM".into());
            }
        }
        Ok(())
    }

    /// Map sealed RAM without reading it. Host page faults fetch the backing
    /// file; guest/device writes become private COW pages, never snapshot writes.
    /// The caller must retain any pager serving `ram_file` for the VM lifetime.
    pub fn map_ram(&self, ranges: &[(GuestAddress, usize)]) -> Result<GuestMemoryMmap, String> {
        crate::memory::map_snapshot_ram(&self.state.ram, self.ram_file.clone(), ranges)
    }
}

impl Vmm {
    /// Capture every CPU/device and copy RAM while the full-machine freeze is
    /// held. File must be a fresh private artifact, never the live RAM backing.
    pub fn capture_machine_state(&self, file: &File) -> Result<MachineSnapshot, String> {
        self.capture_machine_state_with_ram_delta(file, None)
            .map(|(state, _)| state)
    }

    pub fn capture_machine_state_with_ram_delta(
        &self,
        file: &File,
        baseline: Option<&RamDeltaSpec>,
    ) -> Result<(MachineSnapshot, Option<RamDeltaCapture>), String> {
        self.require_ram_quiesced()?;
        if !self.snapshot_devices_frozen {
            return Err("full machine freeze required".into());
        }
        if file.metadata().map_err(|e| e.to_string())?.len() != 0 {
            return Err("RAM snapshot destination must be empty".into());
        }
        let cpus = self.capture_cpu_states()?;
        let devices = self.capture_device_states()?;
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        let kvm = Some(self.vm.save_state().map_err(|e| e.to_string())?);
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        let pio_devices = self.pio_device_manager.io_bus.capture_snapshot_devices()?;
        let (ram, delta) = ram::capture_ram_with_delta(&self.guest_memory, file, baseline)?;
        file.sync_all().map_err(|e| e.to_string())?;
        Ok((
            MachineSnapshot {
                version: 1,
                kernel_layout: self.snapshot_kernel_layout,
                cpus,
                devices,
                ram,
                #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
                kvm,
                #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
                pio_devices,
            },
            delta,
        ))
    }
}
