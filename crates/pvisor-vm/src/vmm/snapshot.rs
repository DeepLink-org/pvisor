//! Restore input for the KVM/HVF builders. Durable publication, build
//! identity, backing-file sealing and execution ownership belong to the runner.
use crate::vmm::{CpuSnapshot, Vmm};
use std::{fs::File, os::unix::fs::FileExt, sync::Arc};
use vm_memory::{Address, Bytes, GuestAddress, GuestMemory, GuestMemoryMmap, GuestMemoryRegion};

pub use crate::api::RamMappingSnapshot;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineSnapshot {
    pub version: u32,
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
        crate::memory::validate_snapshot_ram(&self.state.ram, &self.ram_file)
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
        let mut ram = Vec::new();
        let mut file_offset = 0u64;
        let mut buffer = vec![0; 1024 * 1024];
        for region in self.guest_memory.iter() {
            let base = region.start_addr().raw_value();
            let len = region.len();
            ram.push(RamMappingSnapshot {
                base,
                len,
                file_offset,
            });
            let mut offset = 0;
            while offset < len {
                let count = (len - offset).min(buffer.len() as u64) as usize;
                self.guest_memory
                    .read_slice(&mut buffer[..count], GuestAddress(base + offset))
                    .map_err(|e| e.to_string())?;
                file.write_all_at(&buffer[..count], file_offset + offset)
                    .map_err(|e| e.to_string())?;
                offset += count as u64;
            }
            file_offset = file_offset
                .checked_add(len)
                .ok_or("RAM snapshot size overflow")?;
        }
        file.sync_all().map_err(|e| e.to_string())?;
        Ok(MachineSnapshot {
            version: 1,
            cpus,
            devices,
            ram,
            #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
            kvm,
            #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
            pio_devices,
        })
    }
}
