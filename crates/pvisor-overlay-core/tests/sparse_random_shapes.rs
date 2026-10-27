#![cfg(all(feature = "sparse-checkpoint-copy", target_os = "linux"))]

use pvisor_overlay_core::apply::{UpperCopySite, restore_overlay_upper_as};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

const SELECTOR: &str = "PVISOR_SPARSE_CHECKPOINT_COPY";
const SAMPLE_COUNT: usize = 160;
const BOUNDARIES: [u64; 8] = [0, 1, 4095, 4096, 4097, 64 << 10, 1 << 20, 8 << 20];

struct SelectorGuard;

impl Drop for SelectorGuard {
    fn drop(&mut self) {
        // SAFETY: this integration test binary contains one test.
        unsafe { std::env::remove_var(SELECTOR) };
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: u64) -> u64 {
        if limit == 0 { 0 } else { self.next() % limit }
    }
}

fn write_extent(file: &File, offset: u64, length: usize, rng: &mut Rng) -> io::Result<()> {
    let mut bytes = vec![0u8; length];
    for chunk in bytes.chunks_mut(8) {
        chunk.copy_from_slice(&rng.next().to_le_bytes()[..chunk.len()]);
    }
    file.write_all_at(&bytes, offset)
}

fn data_bytes(path: &Path) -> (bool, u64) {
    let file = File::open(path).unwrap();
    let descriptor = file.as_raw_fd();
    let size = file.metadata().unwrap().len();
    let mut offset = 0u64;
    let mut total = 0u64;
    while offset < size {
        // SAFETY: the descriptor remains open and both offsets are valid `off_t` values.
        let data = unsafe { libc::lseek(descriptor, offset as libc::off_t, libc::SEEK_DATA) };
        if data < 0 {
            return (false, size);
        }
        let data = data as u64;
        if data >= size {
            break;
        }
        // SAFETY: same descriptor and offset class as above.
        let hole = unsafe { libc::lseek(descriptor, data as libc::off_t, libc::SEEK_HOLE) };
        if hole <= 0 {
            return (false, size);
        }
        let end = (hole as u64).min(size);
        if end <= data {
            return (false, size);
        }
        total += end - data;
        offset = end;
    }
    (true, total)
}

fn assert_same_bytes(source: &Path, destination: &Path) {
    let mut source = File::open(source).unwrap();
    let mut destination = File::open(destination).unwrap();
    assert_eq!(
        source.metadata().unwrap().len(),
        destination.metadata().unwrap().len()
    );
    let mut left = [0u8; 64 << 10];
    let mut right = [0u8; 64 << 10];
    loop {
        let read = source.read(&mut left).unwrap();
        destination.read_exact(&mut right[..read]).unwrap();
        assert_eq!(&left[..read], &right[..read]);
        if read == 0 {
            break;
        }
    }
}

#[test]
fn seeded_sparse_shapes_round_trip_bytes_metadata_and_allocation() {
    let temp = tempfile::tempdir().unwrap();
    let guard = SelectorGuard;
    // SAFETY: this integration test binary contains one test.
    unsafe { std::env::set_var(SELECTOR, "1") };
    let mut rng = Rng(0x5eed_2026_1022);

    for index in 0..SAMPLE_COUNT {
        let case = temp.path().join(format!("case-{index:03}"));
        let source = case.join("source");
        let destination = case.join("destination");
        fs::create_dir_all(&source).unwrap();

        let size = BOUNDARIES
            .get(index)
            .copied()
            .unwrap_or_else(|| rng.below((1 << 20) + 1));
        let path = source.join("shape.bin");
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o640)
            .open(&path)
            .unwrap();
        file.set_len(size).unwrap();

        let extent_count = if size == 0 { 0 } else { rng.below(7) as usize };
        for _ in 0..extent_count {
            let start = rng.below(size);
            let maximum = (size - start).min(128 << 10);
            let length = 1 + rng.below(maximum);
            write_extent(&file, start, length as usize, &mut rng).unwrap();
        }
        if index == BOUNDARIES.len() - 1 {
            write_extent(&file, 4 << 20, 4096, &mut rng).unwrap();
        }
        file.sync_all().unwrap();

        restore_overlay_upper_as(
            &source,
            &destination,
            Some(UpperCopySite::CheckpointSnapshot),
        )
        .unwrap();

        let copied = destination.join("shape.bin");
        assert_same_bytes(&path, &copied);
        let source_metadata = fs::metadata(&path).unwrap();
        let copied_metadata = fs::metadata(&copied).unwrap();
        assert_eq!(
            copied_metadata.mode() & 0o7777,
            source_metadata.mode() & 0o7777
        );
        assert_ne!(copied_metadata.ino(), source_metadata.ino());

        let (supported, allocated_data) = data_bytes(&path);
        if supported {
            let destination_blocks = copied_metadata.blocks() * 512;
            assert!(
                destination_blocks <= allocated_data + (128 << 10),
                "case {index}: destination allocated {destination_blocks} bytes for \
                 {allocated_data} source data bytes (logical {size})"
            );
        }
    }

    drop(guard);
}
