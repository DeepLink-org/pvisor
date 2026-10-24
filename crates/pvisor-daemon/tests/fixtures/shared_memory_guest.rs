//! Native acceptance payload; build for aarch64-unknown-linux-musl with rust-lld.
use std::{
    alloc::{Layout, alloc, dealloc},
    fs, thread,
    time::Duration,
};
const BYTES: usize = 64 * 1024 * 1024;
fn expected(index: usize, written: usize, token: u8) -> u8 {
    let mut x = (index / 8) as u64 + 0x9e3779b97f4a7c15;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^= x >> 31;
    (x >> (index % 8 * 8)) as u8 ^ if index < written { token } else { 0 }
}
fn main() {
    let token: u8 = std::env::args().nth(1).unwrap().parse().unwrap();
    let layout = Layout::from_size_align(BYTES, 16384).unwrap();
    let ptr = unsafe { alloc(layout) };
    assert!(!ptr.is_null());
    let data = unsafe { std::slice::from_raw_parts_mut(ptr, BYTES) };
    for (index, byte) in data.iter_mut().enumerate() {
        *byte = expected(index, 0, token);
    }
    fs::write("/control/result", "0 ready").unwrap();
    let mut previous = 0;
    let mut written = 0;
    loop {
        if let Ok(command) = fs::read_to_string("/control/command") {
            let fields: Vec<_> = command.split_whitespace().collect();
            let sequence: u32 = fields[0].parse().unwrap();
            if sequence > previous {
                let next = match fields[1] {
                    "read" => written,
                    "quarter" => BYTES / 4,
                    "all" => BYTES,
                    "exit" => break,
                    _ => panic!("unknown command"),
                };
                for (index, byte) in data.iter_mut().enumerate() {
                    assert_eq!(
                        *byte,
                        expected(index, written, token),
                        "private payload changed at {index}"
                    );
                    if next != written && index >= written && index < next {
                        *byte = expected(index, next, token);
                    }
                }
                written = next;
                let mut hash = 0xcbf29ce484222325u64;
                for (index, byte) in data.iter().enumerate() {
                    assert_eq!(*byte, expected(index, written, token));
                    hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
                }
                fs::write(
                    "/control/result",
                    format!("{sequence} passed {written} {token} {hash:016x}"),
                )
                .unwrap();
                previous = sequence;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    unsafe {
        dealloc(ptr, layout);
    }
}
