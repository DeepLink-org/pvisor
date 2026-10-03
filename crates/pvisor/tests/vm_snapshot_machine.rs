//! Structural checks for the new whole-machine builder input. No Linux/HVF
//! runtime is created by these tests; real guest continuation remains a gate.
#![cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
))]

use krun_vmm::snapshot::{MachineRestore, MachineSnapshot, RamMappingSnapshot};
use std::sync::Arc;

fn restore() -> MachineRestore {
    let file = tempfile::tempfile().unwrap();
    file.set_len(0x2000).unwrap();
    MachineRestore {
        ram_file: Arc::new(file),
        state: MachineSnapshot {
            version: 1,
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
