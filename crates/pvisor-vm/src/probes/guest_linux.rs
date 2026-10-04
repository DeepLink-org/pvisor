//! Real Linux PID 1: preserve a running thread, heap and an open seekable file.
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};
unsafe extern "C" {
    fn mount(
        source: *const std::ffi::c_char,
        target: *const std::ffi::c_char,
        kind: *const std::ffi::c_char,
        flags: usize,
        data: *const std::ffi::c_void,
    ) -> i32;
    fn getpid() -> i32;
}
fn main() {
    fs::create_dir_all("/proc").unwrap();
    assert_eq!(
        unsafe {
            mount(
                c"proc".as_ptr(),
                c"/proc".as_ptr(),
                c"proc".as_ptr(),
                0,
                std::ptr::null(),
            )
        },
        0
    );
    let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap();
    let pid = unsafe { getpid() };
    let mut starts = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/starts")
        .unwrap();
    writeln!(starts, "{} {}", boot.trim(), pid).unwrap();
    starts.sync_all().unwrap();
    drop(starts);
    let mut bytes = vec![0u8; 32 * 1024 * 1024];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(17) ^ (i >> 11)) as u8;
    }
    let mut held = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open("/held")
        .unwrap();
    held.write_all(b"before-resume-after").unwrap();
    held.seek(SeekFrom::Start(7)).unwrap();
    held.sync_all().unwrap();
    let counter = Arc::new(AtomicU64::new(0));
    let worker = counter.clone();
    thread::spawn(move || {
        loop {
            worker.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(5));
        }
    });
    loop {
        let count = counter.load(Ordering::SeqCst);
        // Publish a complete observation atomically: the host may read while
        // the guest runs, and a snapshot may freeze between separate syscalls.
        fs::write("/ready.next", format!("{} {} {}", boot.trim(), pid, count)).unwrap();
        fs::rename("/ready.next", "/ready").unwrap();
        if fs::metadata("/release").is_ok() {
            assert!(
                bytes
                    .iter()
                    .enumerate()
                    .all(|(i, b)| *b == (i.wrapping_mul(17) ^ (i >> 11)) as u8)
            );
            assert_eq!(held.stream_position().unwrap(), 7);
            let mut text = [0u8; 6];
            held.read_exact(&mut text).unwrap();
            assert_eq!(&text, b"resume");
            assert_eq!(
                fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap(),
                boot
            );
            assert_eq!(unsafe { getpid() }, pid);
            fs::write(
                "/result",
                format!("linux-cold-restore-ok {} {} {}", boot.trim(), pid, count),
            )
            .unwrap();
            loop {
                thread::sleep(Duration::from_secs(60));
            }
        }
        std::hint::black_box(&mut bytes);
        thread::sleep(Duration::from_millis(50));
    }
}
