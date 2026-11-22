//! ACPI topology for the Rust native kernel on x86 KVM.

use crate::arch_gen::x86::bootparam::boot_params;
use std::io;
use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};

// This BIOS window is outside the E820 RAM ranges and the MP table.
const RSDP: u64 = 0xe0000;
const RSDT: u64 = RSDP + 0x40;
const MADT: u64 = RSDP + 0x80;
const FADT: u64 = RSDP + 0x1000;
// Imported bootparam bindings predate this field; it occupies the first 8 bytes of _pad3.
const RSDP_BOOT_OFFSET: usize = std::mem::offset_of!(boot_params, _pad3);

fn checksum(bytes: &mut [u8], offset: usize) {
    bytes[offset] = 0;
    bytes[offset] = 0u8.wrapping_sub(bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)));
}

fn table(signature: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0; 36];
    bytes[..4].copy_from_slice(signature);
    bytes[4..8].copy_from_slice(&(36u32 + body.len() as u32).to_le_bytes());
    bytes[8] = 1;
    bytes[10..16].copy_from_slice(b"PVISOR");
    bytes[16..24].copy_from_slice(b"PVKERNEL");
    bytes[24..28].copy_from_slice(&1u32.to_le_bytes());
    bytes[28..32].copy_from_slice(b"PVIS");
    bytes[32..36].copy_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(body);
    checksum(&mut bytes, 9);
    bytes
}

fn tables(cpus: u8) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut rsdp = vec![0; 20];
    rsdp[..8].copy_from_slice(b"RSD PTR ");
    rsdp[9..15].copy_from_slice(b"PVISOR");
    rsdp[16..20].copy_from_slice(&(RSDT as u32).to_le_bytes());
    checksum(&mut rsdp, 8);

    let rsdt = table(
        b"RSDT",
        &[(MADT as u32).to_le_bytes(), (FADT as u32).to_le_bytes()].concat(),
    );
    let mut body = Vec::new();
    body.extend_from_slice(&0xfee0_0000u32.to_le_bytes());
    body.extend_from_slice(&1u32.to_le_bytes());
    for id in 0..cpus {
        body.extend_from_slice(&[0, 8, id, id, 1, 0, 0, 0]);
    }
    // Match the I/O APIC ID in the MP table.
    body.extend_from_slice(&[1, 12, cpus + 1, 0, 0, 0, 0xc0, 0xfe, 0, 0, 0, 0]);
    // PIT IRQ 0 is wired to GSI 2.
    body.extend_from_slice(&[2, 10, 0, 0, 2, 0, 0, 0, 0, 0]);
    let mut fixed = vec![0; 240];
    // FADT RESET_REG_SUP and an 8-bit reset through the VMM's existing i8042 port.
    fixed[76..80].copy_from_slice(&(1u32 << 10).to_le_bytes());
    fixed[80..84].copy_from_slice(&[1, 8, 0, 1]);
    fixed[84..92].copy_from_slice(&0x64u64.to_le_bytes());
    fixed[92] = 0xfe;
    let mut fadt = table(b"FACP", &fixed);
    fadt[8] = 6;
    checksum(&mut fadt, 9);
    (rsdp, rsdt, table(b"APIC", &body), fadt)
}

pub(crate) fn install(memory: &GuestMemoryMmap, cpus: u8) -> io::Result<()> {
    let (rsdp, rsdt, madt, fadt) = tables(cpus);
    for (address, bytes) in [(RSDP, rsdp), (RSDT, rsdt), (MADT, madt), (FADT, fadt)] {
        memory
            .write_slice(&bytes, GuestAddress(address))
            .map_err(io::Error::other)?;
    }
    let pointer = super::layout::ZERO_PAGE_START + RSDP_BOOT_OFFSET as u64;
    memory
        .write_obj(RSDP, GuestAddress(pointer))
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case(1)]
    #[case(2)]
    #[case(64)]
    fn topology_has_valid_lengths_checksums_and_reserved_addresses(#[case] cpus: u8) {
        let (rsdp, rsdt, madt, fadt) = tables(cpus);
        for bytes in [&rsdp, &rsdt, &madt, &fadt] {
            assert_eq!(
                bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
                0
            );
        }
        assert_eq!(&rsdp[16..20], &(RSDT as u32).to_le_bytes());
        assert_eq!(&rsdt[36..40], &(MADT as u32).to_le_bytes());
        assert_eq!(&rsdt[40..44], &(FADT as u32).to_le_bytes());
        assert_eq!(&fadt[112..116], &(1u32 << 10).to_le_bytes());
        assert_eq!(&fadt[116..120], &[1, 8, 0, 1]);
        assert_eq!(&fadt[120..128], &0x64u64.to_le_bytes());
        assert_eq!(fadt[128], 0xfe);
        assert_eq!(&madt[4..8], &(madt.len() as u32).to_le_bytes());
        for id in 0..cpus {
            let start = 44 + usize::from(id) * 8;
            assert_eq!(&madt[start..start + 8], &[0, 8, id, id, 1, 0, 0, 0]);
        }
        const { assert!(RSDP > super::super::layout::EBDA_START) };
        assert!(MADT + madt.len() as u64 <= FADT);
        assert!(FADT + fadt.len() as u64 <= super::super::layout::HIMEM_START);
    }

    #[test]
    fn installs_rsdp_in_linux_boot_parameters_without_overwriting_neighbors() {
        let memory = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x100000)]).unwrap();
        assert_eq!(RSDP_BOOT_OFFSET, 0x70);
        let offset = super::super::layout::ZERO_PAGE_START + RSDP_BOOT_OFFSET as u64;
        memory.write_obj(0xaau8, GuestAddress(offset - 1)).unwrap();
        memory.write_obj(0xbbu8, GuestAddress(offset + 8)).unwrap();
        install(&memory, 2).unwrap();
        assert_eq!(memory.read_obj::<u64>(GuestAddress(offset)).unwrap(), RSDP);
        assert_eq!(
            memory.read_obj::<u8>(GuestAddress(offset - 1)).unwrap(),
            0xaa
        );
        assert_eq!(
            memory.read_obj::<u8>(GuestAddress(offset + 8)).unwrap(),
            0xbb
        );
    }
}
