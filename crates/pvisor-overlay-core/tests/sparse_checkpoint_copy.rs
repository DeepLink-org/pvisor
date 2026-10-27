#![cfg(all(feature = "sparse-checkpoint-copy", target_os = "linux"))]

use pvisor_overlay_core::apply::{UpperCopySite, restore_overlay_upper_as};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

const SELECTOR: &str = "PVISOR_SPARSE_CHECKPOINT_COPY";

struct SelectorGuard;

impl Drop for SelectorGuard {
    fn drop(&mut self) {
        // SAFETY: this integration test binary contains one test, so no sibling
        // test in the same process reads the selector concurrently.
        unsafe { std::env::remove_var(SELECTOR) };
    }
}

fn write_at(file: &File, offset: u64, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(bytes, offset)
}

fn holes_are_supported(path: &Path) -> bool {
    let file = File::open(path).unwrap();
    let descriptor = file.as_raw_fd();
    // SAFETY: the descriptor remains open for both calls and the offsets are
    // valid `off_t` values.
    let data = unsafe { libc::lseek(descriptor, 0, libc::SEEK_DATA) };
    if data < 0 {
        return false;
    }
    // SAFETY: same descriptor and offset class as above.
    let hole = unsafe { libc::lseek(descriptor, data, libc::SEEK_HOLE) };
    hole > data
}

fn assert_same_bytes(source: &Path, destination: &Path) {
    let mut source = File::open(source).unwrap();
    let mut destination = File::open(destination).unwrap();
    assert_eq!(
        source.metadata().unwrap().len(),
        destination.metadata().unwrap().len()
    );

    let mut left = vec![0u8; 1 << 20];
    let mut right = vec![0u8; 1 << 20];
    loop {
        let read = source.read(&mut left).unwrap();
        destination.read_exact(&mut right[..read]).unwrap();
        assert_eq!(&left[..read], &right[..read]);
        if read == 0 {
            break;
        }
    }
}

fn create_sparse(path: &Path) -> (u64, u64) {
    let logical = 8u64 << 20;
    let data_bytes = 1u64 << 20;
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o640)
        .open(path)
        .unwrap();
    file.set_len(logical).unwrap();
    write_at(&file, 0, &vec![0x5a; data_bytes as usize]).unwrap();
    write_at(&file, logical - 4, b"tail").unwrap();
    (logical, data_bytes + 4)
}

#[test]
fn checkpoint_restore_preserves_sparse_bytes_and_independent_inodes() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let destination = temp.path().join("destination");
    fs::create_dir(&source).unwrap();

    let sparse = source.join("sparse.bin");
    let (logical, data_bytes) = create_sparse(&sparse);
    let sparse_supported = holes_are_supported(&sparse);

    let dense = source.join("dense.bin");
    let dense_bytes = vec![0x33; 96 << 10];
    fs::write(&dense, &dense_bytes).unwrap();

    let all_hole = source.join("all-hole.bin");
    File::create(&all_hole).unwrap().set_len(2 << 20).unwrap();

    let linked = source.join("linked.bin");
    fs::write(&linked, b"shared hardlink bytes").unwrap();
    let alias = source.join("alias.bin");
    fs::hard_link(&linked, &alias).unwrap();

    let guard = SelectorGuard;
    // SAFETY: this integration test binary contains one test, so no sibling
    // test in the same process reads the selector concurrently.
    unsafe { std::env::set_var(SELECTOR, "checkpoint") };

    restore_overlay_upper_as(
        &source,
        &destination,
        Some(UpperCopySite::CheckpointSnapshot),
    )
    .unwrap();

    let copied_sparse = destination.join("sparse.bin");
    assert_same_bytes(&sparse, &copied_sparse);
    assert_same_bytes(&dense, &destination.join("dense.bin"));
    assert_same_bytes(&all_hole, &destination.join("all-hole.bin"));
    assert_same_bytes(&linked, &destination.join("linked.bin"));

    let source_metadata = fs::metadata(&sparse).unwrap();
    let destination_metadata = fs::metadata(&copied_sparse).unwrap();
    assert_ne!(source_metadata.ino(), destination_metadata.ino());
    assert_eq!(destination_metadata.len(), logical);
    if sparse_supported {
        assert!(
            destination_metadata.blocks() * 512 < data_bytes + (1 << 20),
            "expected sparse destination: {} bytes allocated for {logical} logical bytes",
            destination_metadata.blocks() * 512
        );
        assert!(
            fs::metadata(destination.join("all-hole.bin"))
                .unwrap()
                .blocks()
                * 512
                < 1 << 20,
            "an all-hole file must not be materialized densely"
        );
    }

    let linked_metadata = fs::metadata(destination.join("linked.bin")).unwrap();
    let alias_metadata = fs::metadata(destination.join("alias.bin")).unwrap();
    assert_eq!(linked_metadata.ino(), alias_metadata.ino());
    assert_eq!(linked_metadata.nlink(), 2);
    assert_eq!(
        fs::read(destination.join("alias.bin")).unwrap(),
        b"shared hardlink bytes"
    );

    drop(guard);
}
