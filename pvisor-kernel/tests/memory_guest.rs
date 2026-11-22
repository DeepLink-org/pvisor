//! Static workload for memory.py; runs inside the guest.
use std::{
    alloc::{Layout, alloc},
    io::{self, Read},
};

const PAGE: usize = 4096;
const PAGES: usize = 1024;
const MARKER: u64 = 0x8c774a135bf19a2d;

fn main() -> io::Result<()> {
    let mut input = io::stdin().lock();
    let layout = Layout::from_size_align(PAGE * PAGES, PAGE).unwrap();
    let buffer = unsafe { alloc(layout) };
    if buffer.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    for page in 0..PAGES {
        let start = unsafe { buffer.add(page * PAGE) };
        unsafe {
            start.write_bytes(0x5a, PAGE);
            start.cast::<u64>().write(MARKER ^ page as u64);
        }
    }
    std::fs::write("/ready", b"")?;
    let mut command = [0];
    input.read_exact(&mut command)?;
    assert!(matches!(command[0], b'r' | b'w'));
    for page in 0..PAGES {
        let start = unsafe { buffer.add(page * PAGE) };
        let marker = MARKER ^ page as u64;
        if command[0] == b'w' {
            assert_eq!(unsafe { start.cast::<u64>().read_volatile() }, marker);
            unsafe { start.cast::<u64>().write_volatile(!marker) };
        }
        let expected = if command[0] == b'w' { !marker } else { marker };
        assert_eq!(unsafe { start.cast::<u64>().read_volatile() }, expected);
        for offset in 8..PAGE {
            assert_eq!(unsafe { start.add(offset).read_volatile() }, 0x5a);
        }
    }
    std::fs::write("/checked", b"")?;
    input.read_exact(&mut command)?;
    assert_eq!(command[0], b'x');
    std::process::exit(7);
}
