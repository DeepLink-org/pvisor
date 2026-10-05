//! Restore input for the KVM/HVF builders. Durable publication, build
//! identity, backing-file sealing and execution ownership belong to the runner.
mod ram;

use crate::{CpuSnapshot, Vmm};
use std::{fs::File, sync::Arc};
use vm_memory::{
    Address, FileOffset, GuestAddress, GuestMemoryMmap, GuestRegionMmap, mmap::MmapRegionBuilder,
};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RamMappingSnapshot {
    pub base: u64,
    pub len: u64,
    pub file_offset: u64,
}

/// Supervisor-bound immutable MAP_PRIVATE baseline. This is an optional
/// capture optimization, never authority to restore a partial RAM file.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RamDeltaSpec {
    pub device: u64,
    pub inode: u64,
    pub length: u64,
    pub block_bytes: u32,
    pub base_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RamDeltaCapture {
    pub version: u32,
    pub length: u64,
    pub block_bytes: u32,
    pub base_sha256: String,
    /// Sorted unique blocks captured from live memory, including cleared blocks.
    pub changed_blocks: Vec<u64>,
}
impl RamDeltaSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.length == 0
            || self.length > 64 * 1024 * 1024 * 1024
            || !self.block_bytes.is_power_of_two()
            || !(4096..=1024 * 1024).contains(&self.block_bytes)
            || self.length.div_ceil(u64::from(self.block_bytes)) > 1 << 20
            || self.base_sha256.len() != 64
            || !self
                .base_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid incremental RAM baseline".into());
        }
        Ok(())
    }
}
impl RamDeltaCapture {
    pub fn validate(&self) -> Result<(), String> {
        RamDeltaSpec {
            device: 0,
            inode: 0,
            length: self.length,
            block_bytes: self.block_bytes,
            base_sha256: self.base_sha256.clone(),
        }
        .validate()?;
        if self.version != 1
            || self.changed_blocks.len() as u64 > self.length.div_ceil(u64::from(self.block_bytes))
            || self
                .changed_blocks
                .iter()
                .any(|index| *index >= self.length.div_ceil(u64::from(self.block_bytes)))
            || self
                .changed_blocks
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err("invalid incremental RAM capture inventory".into());
        }
        Ok(())
    }
}

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
            || self.guest_addr % page as u64 != 0
            || self.size % page as u64 != 0
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
    pub devices: Vec<devices::snapshot::BusMappingSnapshot>,
    pub ram: Vec<RamMappingSnapshot>,
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub kvm: Option<crate::linux::vstate::VmState>,
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub pio_devices: Vec<devices::snapshot::BusMappingSnapshot>,
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
        if self.state.ram.is_empty() {
            return Err("empty RAM snapshot inventory".into());
        }
        let mut end = 0u64;
        let mut address_end = 0u64;
        for mapping in &self.state.ram {
            if mapping.len == 0 || mapping.file_offset != end || mapping.base < address_end {
                return Err("invalid RAM mapping inventory".into());
            }
            end = end
                .checked_add(mapping.len)
                .ok_or("RAM file size overflow")?;
            address_end = mapping
                .base
                .checked_add(mapping.len)
                .ok_or("RAM address overflow")?;
        }
        if self.ram_file.metadata().map_err(|e| e.to_string())?.len() != end {
            return Err("RAM snapshot file size mismatch".into());
        }
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
        self.validate_ram_file()?;
        if ranges.len() != self.state.ram.len()
            || ranges
                .iter()
                .zip(&self.state.ram)
                .any(|((base, len), saved)| {
                    base.raw_value() != saved.base || *len as u64 != saved.len
                })
        {
            return Err("RAM snapshot topology mismatch".into());
        }
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page <= 0 {
            return Err("cannot determine host page size".into());
        }
        let mut regions = Vec::with_capacity(ranges.len());
        for mapping in &self.state.ram {
            if mapping.file_offset % page as u64 != 0 || mapping.len % page as u64 != 0 {
                return Err("RAM snapshot mappings must be host-page aligned".into());
            }
            let size = usize::try_from(mapping.len).map_err(|e| e.to_string())?;
            let region = MmapRegionBuilder::new(size)
                .with_file_offset(FileOffset::from_arc(
                    self.ram_file.clone(),
                    mapping.file_offset,
                ))
                .with_mmap_prot(libc::PROT_READ | libc::PROT_WRITE)
                .with_mmap_flags(libc::MAP_PRIVATE)
                .build()
                .map_err(|e| e.to_string())?;
            regions.push(Arc::new(
                GuestRegionMmap::new(region, GuestAddress(mapping.base))
                    .ok_or("invalid RAM snapshot mapping")?,
            ));
        }
        GuestMemoryMmap::from_arc_regions(regions).map_err(|e| format!("{e:?}"))
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
