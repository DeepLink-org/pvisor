//! Structural checks for the new whole-machine builder input. No Linux/HVF
//! runtime is created by these tests; real guest continuation remains a gate.
#![cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
))]

use crate::vmm::snapshot::{KernelLayout, MachineRestore, MachineSnapshot, RamMappingSnapshot};
use std::sync::Arc;

fn restore() -> MachineRestore {
    let file = tempfile::tempfile().unwrap();
    file.set_len(0x2000).unwrap();
    MachineRestore {
        ram_file: Arc::new(file),
        state: MachineSnapshot {
            version: 1,
            kernel_layout: None,
            cpus: vec![],
            #[cfg(target_os = "linux")]
            kvm: None,
            #[cfg(target_os = "linux")]
            pio_devices: vec![],
            devices: vec![],
            ram: vec![
                RamMappingSnapshot {
                    base: 0x1000,
                    len: 0x1000,
                    file_offset: 0,
                },
                RamMappingSnapshot {
                    base: 0x5000,
                    len: 0x1000,
                    file_offset: 0x1000,
                },
            ],
        },
    }
}

#[test]
fn machine_ram_inventory_checks_offsets_overlap_and_exact_file_size() {
    let valid = restore();
    valid.validate_ram_file().unwrap();
    for case in 0..6 {
        let mut invalid = restore();
        match case {
            0 => invalid.state.ram[0].len = 0,
            1 => invalid.state.ram[1].file_offset += 1,
            2 => invalid.state.ram[1].base = 0x1800,
            3 => invalid.state.ram[1].base = u64::MAX - 0x100,
            4 => invalid.ram_file.set_len(0x2001).unwrap(),
            _ => invalid.state.ram.clear(),
        }
        assert!(invalid.validate_ram_file().is_err(), "case {case}");
    }
    // A structurally valid RAM file never substitutes for missing CPU state.
    assert!(valid.validate(1).is_err());
    assert!(valid.validate(0).is_err());
}

#[test]
fn machine_manifest_rejects_unknown_fields() {
    let mut value = serde_json::to_value(restore().state).unwrap();
    value["ram"][0]["unexpected"] = true.into();
    assert!(serde_json::from_value::<MachineSnapshot>(value).is_err());
}

#[test]
fn saved_kernel_geometry_is_optional_but_must_match_captured_ram() {
    let mut input = restore();
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    input.ram_file.set_len(page * 2).unwrap();
    input.state.ram = vec![RamMappingSnapshot {
        base: page,
        len: page * 2,
        file_offset: 0,
    }];
    let legacy = serde_json::to_value(&input.state).unwrap();
    assert!(legacy.get("kernel_layout").is_none());
    assert!(serde_json::from_value::<MachineSnapshot>(legacy)
        .unwrap()
        .kernel_layout
        .is_none());
    let valid = KernelLayout {
        guest_addr: page,
        size: page * 2,
    };
    input.state.kernel_layout = Some(valid);
    input.validate_ram_file().unwrap();
    let encoded = serde_json::to_value(&input.state).unwrap();
    assert_eq!(
        serde_json::from_value::<MachineSnapshot>(encoded.clone())
            .unwrap()
            .kernel_layout,
        Some(valid)
    );
    let mut unknown = encoded;
    unknown["kernel_layout"]["unexpected"] = true.into();
    assert!(serde_json::from_value::<MachineSnapshot>(unknown).is_err());
    for kernel in [
        KernelLayout {
            guest_addr: 0,
            ..valid
        },
        KernelLayout { size: 0, ..valid },
        KernelLayout {
            guest_addr: page + 1,
            ..valid
        },
        KernelLayout {
            size: page + 1,
            ..valid
        },
        KernelLayout {
            guest_addr: u64::MAX - page + 1,
            ..valid
        },
        KernelLayout {
            guest_addr: page * 8,
            ..valid
        },
        KernelLayout {
            size: page * 3,
            ..valid
        },
    ] {
        input.state.kernel_layout = Some(kernel);
        assert!(input.validate_ram_file().is_err(), "{kernel:?}");
    }
    // Geometry alone must never authorize a context lacking CPU/KVM state.
    input.state.kernel_layout = Some(valid);
    let mut builder =
        crate::builder::Builder::<crate::backend::NativeBackend>::new(1, 128).unwrap();
    assert!(builder.machine_restore(input).is_err());
    assert!(builder.machine_restore(restore()).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn restore_geometry_rebuilds_exact_layout_without_a_firmware_bundle() {
    use crate::vmm::{
        builder::{create_guest_memory, Payload},
        resources::VmResources,
    };
    use vm_memory::{GuestAddress, GuestMemory, GuestMemoryRegion};
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    let kernel = KernelLayout {
        guest_addr: 16 * 1024 * 1024,
        size: page,
    };
    let mut input = restore();
    input.state.kernel_layout = Some(kernel);
    let ranges = [
        (0, kernel.guest_addr),
        (kernel.guest_addr, kernel.size),
        (kernel.guest_addr + kernel.size, 64 * 1024 * 1024),
    ];
    let mut offset = 0;
    input.state.ram = ranges
        .iter()
        .map(|&(base, len)| {
            let region = RamMappingSnapshot {
                base,
                len,
                file_offset: offset,
            };
            offset += len;
            region
        })
        .collect();
    input.ram_file.set_len(offset).unwrap();
    let mut resources = VmResources::default();
    resources.machine_restore = Some(Arc::new(input));
    assert!(resources.kernel_bundle.is_none());
    let (memory, _, _, _) =
        create_guest_memory(64, &resources, &Payload::RestoredKernel(kernel)).unwrap();
    for (region, &(base, len)) in memory.iter().zip(&ranges) {
        assert_eq!(region.start_addr(), GuestAddress(base));
        assert_eq!(region.len(), len);
    }
    // A different budget must fail the exact topology check, not remap RAM.
    assert!(create_guest_memory(65, &resources, &Payload::RestoredKernel(kernel)).is_err());
    // The arch helper otherwise panics or underflows on out-of-budget kernels.
    assert!(create_guest_memory(1, &resources, &Payload::RestoredKernel(kernel)).is_err());
    let gap = KernelLayout {
        guest_addr: 4 * 1024 * 1024 * 1024,
        size: page,
    };
    assert!(create_guest_memory(8192, &resources, &Payload::RestoredKernel(gap)).is_err());
}

#[test]
fn restored_ram_is_file_mapped_and_forks_keep_private_writes() {
    use std::fs::File;
    use std::os::unix::fs::FileExt;
    use vm_memory::{Bytes, GuestAddress, GuestMemory};
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let mut input = restore();
    input.ram_file.set_len((page * 4) as u64).unwrap();
    input.ram_file.write_all_at(b"saved first", 0).unwrap();
    input
        .ram_file
        .write_all_at(b"saved second", (page * 2) as u64)
        .unwrap();
    input.state.ram = vec![
        RamMappingSnapshot {
            base: page as u64,
            len: (page * 2) as u64,
            file_offset: 0,
        },
        RamMappingSnapshot {
            base: (page * 8) as u64,
            len: (page * 2) as u64,
            file_offset: (page * 2) as u64,
        },
    ];
    // A read-only descriptor must still permit writable MAP_PRIVATE mappings.
    let path = tempfile::NamedTempFile::new().unwrap();
    std::io::copy(
        &mut input.ram_file.try_clone().unwrap(),
        &mut path.as_file(),
    )
    .unwrap();
    input.ram_file = Arc::new(File::open(path.path()).unwrap());
    let ranges = [
        (GuestAddress(page as u64), page * 2),
        (GuestAddress((page * 8) as u64), page * 2),
    ];
    let first = input.map_ram(&ranges).unwrap();
    let second = input.map_ram(&ranges).unwrap();
    for region in first.iter() {
        assert_eq!(region.flags() & libc::MAP_PRIVATE, libc::MAP_PRIVATE);
        #[cfg(target_os = "linux")]
        assert_eq!(region.flags() & libc::MAP_POPULATE, 0);
        assert!(region.file_offset().is_some());
    }
    first.write_slice(b"first fork!", ranges[0].0).unwrap();
    second.write_slice(b"second fork!", ranges[1].0).unwrap();
    let mut bytes = [0; 11];
    second.read_slice(&mut bytes, ranges[0].0).unwrap();
    assert_eq!(&bytes, b"saved first");
    let mut bytes = [0; 12];
    first.read_slice(&mut bytes, ranges[1].0).unwrap();
    assert_eq!(&bytes, b"saved second");
    input
        .ram_file
        .read_exact_at(&mut bytes, (page * 2) as u64)
        .unwrap();
    assert_eq!(&bytes, b"saved second");
    assert!(input.map_ram(&ranges[..1]).is_err());
    let mut invalid = ranges;
    invalid[0].0 = GuestAddress(0);
    assert!(input.map_ram(&invalid).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn mapping_restore_does_not_fault_in_untouched_ram() {
    use std::os::unix::fs::FileExt;
    use vm_memory::{Bytes, GuestAddress, GuestMemory};
    let mut input = restore();
    input.ram_file.set_len(32 * 1024 * 1024).unwrap();
    input.state.ram = vec![RamMappingSnapshot {
        base: 0,
        len: 32 * 1024 * 1024,
        file_offset: 0,
    }];
    let memory = input
        .map_ram(&[(GuestAddress(0), 32 * 1024 * 1024)])
        .unwrap();
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    let pagemap = std::fs::File::open("/proc/self/pagemap").unwrap();
    let present = |address: u64| {
        let mut bytes = [0; 8];
        pagemap
            .read_exact_at(&mut bytes, address / page * 8)
            .unwrap();
        u64::from_ne_bytes(bytes) & (1 << 63) != 0
    };
    let region = memory.iter().next().unwrap();
    let address = region.as_ptr() as u64;
    assert!(!present(address));
    assert!(!present(address + 16 * 1024 * 1024));
    let mut bytes = [1];
    memory.read_slice(&mut bytes, GuestAddress(0)).unwrap();
    assert_eq!(bytes, [0]);
    assert!(present(address));
    assert!(!present(address + 16 * 1024 * 1024));
}
