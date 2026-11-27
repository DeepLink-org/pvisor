//! KVM probe: `/subject` has 1027 pages, byte i equal to (i/4096 + i%251) % 256.

use std::{fs::File, os::unix::fs::FileExt, time::Instant};

const PAGE: usize = 4096;
const PAGES: usize = 1024;

fn verify(offset: usize, bytes: &[u8]) {
    for (i, &byte) in (offset..).zip(bytes) {
        assert_eq!(byte, (i / PAGE + i % 251) as u8, "offset {i}");
    }
}

fn main() {
    let count: usize = std::env::args().nth(1).unwrap().parse().unwrap();
    assert!((1..=256).contains(&count));
    let first = File::open("/subject").unwrap();
    let others: Vec<_> = (1..count)
        .map(|_| File::open("/subject").unwrap())
        .collect();

    let mut data = vec![0xa5; PAGES * PAGE];
    let start = Instant::now();
    for n in 0..PAGES {
        let offset = (n * 37 % PAGES) * PAGE;
        assert_eq!(
            first
                .read_at(&mut data[offset..offset + PAGE], offset as u64)
                .unwrap(),
            PAGE
        );
    }
    let elapsed = start.elapsed();
    verify(0, &data);

    // A newer write-only handle cannot service a cache miss.
    let recent = File::options()
        .read(true)
        .write(true)
        .open("/subject")
        .unwrap();
    let writer = File::options().write(true).open("/subject").unwrap();
    let mut page = [0u8; PAGE];
    let read_page = |index: usize, page: &mut [u8]| {
        let offset = index * PAGE;
        assert_eq!(first.read_at(page, offset as u64).unwrap(), PAGE);
        verify(offset, page);
    };
    read_page(PAGES, &mut page);
    drop(recent);
    drop(others);
    read_page(PAGES + 1, &mut page);
    drop(writer);
    read_page(PAGES + 2, &mut page);
    assert_eq!(
        first
            .read_at(&mut page, ((PAGES + 3) * PAGE) as u64)
            .unwrap(),
        0
    );
    println!("READ_HANDLES_OK handles={count} ns={}", elapsed.as_nanos());
}
