//! Static Linux guest for immutable_read_case; compile with rustc for musl.
use std::{fs::File, os::fd::AsRawFd, os::unix::fs::FileExt, time::Duration};

unsafe extern "C" {
    fn mmap(
        address: *mut u8,
        length: usize,
        protection: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut u8;
    fn munmap(address: *mut u8, length: usize) -> i32;
    fn mount(
        source: *const i8,
        target: *const i8,
        kind: *const i8,
        flags: u64,
        data: *const i8,
    ) -> i32;
    fn statx(dir: i32, path: *const i8, flags: i32, mask: u32, output: *mut u64) -> i32;
}

fn main() {
    let dax = std::env::args().nth(1).unwrap() == "dax";
    let path = if dax {
        std::fs::create_dir("/mnt").unwrap();
        assert_eq!(
            unsafe {
                mount(
                    c"files".as_ptr(),
                    c"/mnt".as_ptr(),
                    c"virtiofs".as_ptr(),
                    1,
                    c"dax=inode".as_ptr(),
                )
            },
            0,
            "DAX mount: {}",
            std::io::Error::last_os_error()
        );
        let mut attributes = [0u64; 32];
        assert_eq!(
            unsafe {
                statx(
                    -100,
                    c"/mnt/payload".as_ptr(),
                    0,
                    0x7ff,
                    attributes.as_mut_ptr(),
                )
            },
            0
        );
        assert_ne!(attributes[1] & 0x200000, 0, "guest must actually use DAX");
        "/mnt/payload"
    } else {
        "/payload"
    };
    let file = File::open(path).unwrap();
    let length = file.metadata().unwrap().len() as usize;
    assert_eq!(length, 4 << 20);
    let memory = unsafe { mmap(std::ptr::null_mut(), length, 3, 2, file.as_raw_fd(), 0) };
    assert_ne!(memory as isize, -1);
    // Fault every file page into guest page cache/DAX. Allow the local pager
    // to process READ completions, then reread after the mapping transition.
    for _ in 0..2 {
        for offset in 0..length {
            assert_eq!(
                unsafe { std::ptr::read_volatile(memory.add(offset)) },
                (offset % 251) as u8
            );
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    for offset in (0..length).step_by(16384) {
        unsafe { std::ptr::write_volatile(memory.add(offset), 0xff) };
    }
    std::thread::sleep(Duration::from_secs(2));
    for offset in 0..length {
        let expected = if offset % 16384 == 0 {
            0xff
        } else {
            (offset % 251) as u8
        };
        assert_eq!(
            unsafe { std::ptr::read_volatile(memory.add(offset)) },
            expected
        );
    }
    let mut original = vec![0; length];
    file.read_exact_at(&mut original, 0).unwrap();
    assert!(original
        .iter()
        .enumerate()
        .all(|(index, byte)| *byte == (index % 251) as u8));
    assert_eq!(unsafe { munmap(memory, length) }, 0);
    println!("IMMUTABLE_READ_GUEST_PASS dax={dax}");
}
