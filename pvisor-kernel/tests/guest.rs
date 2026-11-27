//! Standalone static guest workload for shared-rootfs and exit-authority validation.

use std::{
    fs,
    io::{Read, Seek, Write},
    os::fd::AsRawFd,
    os::unix::fs::{FileExt, PermissionsExt},
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

#[repr(C)]
struct CapHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

unsafe extern "C" {
    fn ioctl(fd: i32, request: i32, ...) -> i32;
    fn sched_getaffinity(pid: i32, len: usize, mask: *mut u64) -> i32;
    fn sched_getcpu() -> i32;
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
    fn fallocate(fd: i32, mode: i32, offset: i64, len: i64) -> i32;
    fn capget(header: *mut CapHeader, data: *mut CapData) -> i32;
    fn capset(header: *const CapHeader, data: *const CapData) -> i32;
    fn fsetxattr(fd: i32, name: *const i8, value: *const u8, size: usize, flags: i32) -> i32;
    fn fgetxattr(fd: i32, name: *const i8, value: *mut u8, size: usize) -> isize;
}

fn check_setid_writes() {
    // Use guest tmpfs so the host filesystem cannot clear these mode bits for us.
    let file = fs::File::create_new("/dev/shm/write-contract").unwrap();
    file.set_permissions(fs::Permissions::from_mode(0o6755))
        .unwrap();
    let name = c"security.capability".as_ptr().cast();
    let mut empty_caps = [0u8; 20];
    empty_caps[..4].copy_from_slice(&0x0200_0000u32.to_le_bytes());
    assert_eq!(
        unsafe {
            fsetxattr(
                file.as_raw_fd(),
                name,
                empty_caps.as_ptr(),
                empty_caps.len(),
                0,
            )
        },
        0
    );
    assert!(unsafe { fgetxattr(file.as_raw_fd(), name, std::ptr::null_mut(), 0) } > 0);
    file.write_all_at(b"x", 0).unwrap();
    assert_eq!(
        file.metadata().unwrap().permissions().mode() & 0o7777,
        0o6755
    );
    assert_eq!(
        unsafe { fgetxattr(file.as_raw_fd(), name, std::ptr::null_mut(), 0) },
        -1
    );
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(61)); // ENODATA
    let write_mode = |mode| {
        file.set_permissions(fs::Permissions::from_mode(mode))
            .unwrap();
        file.write_all_at(b"x", 0).unwrap();
        file.metadata().unwrap().permissions().mode() & 0o7777
    };
    let mut header = CapHeader {
        version: 0x2008_0522,
        pid: 0,
    };
    let mut data = [CapData::default(); 2];
    assert_eq!(unsafe { capget(&mut header, data.as_mut_ptr()) }, 0);
    const FSETID: u32 = 1 << 4;
    assert_ne!(data[0].effective & FSETID, 0);
    data[0].effective &= !FSETID;
    assert_eq!(unsafe { capset(&header, data.as_ptr()) }, 0);
    assert_eq!(write_mode(0o6755), 0o755);
    assert_eq!(write_mode(0o6644), 0o2644);
}

fn check_write_cache() {
    for mapped in [false, true] {
        let path = if mapped { "/mapped" } else { "/buffered" };
        let writer = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .unwrap();
        let reader = fs::File::open(path).unwrap();
        writer.write_all_at(b"abc", 8192).unwrap();
        writer.sync_all().unwrap();
        let mut expected = vec![0; 8195];
        expected[8192..].copy_from_slice(b"abc");
        if mapped {
            // MAP_SHARED, PROT_READ; the writer remains open across the first mapping.
            let addr = unsafe {
                mmap(
                    std::ptr::null_mut(),
                    expected.len(),
                    1,
                    1,
                    reader.as_raw_fd(),
                    0,
                )
            };
            assert_ne!(addr as isize, -1);
            assert_eq!(unsafe { addr.add(8192).read_volatile() }, b'a');
            writer.write_all_at(b"xyz", 8192).unwrap();
            assert_eq!(unsafe { addr.add(8192).read_volatile() }, b'x');
            assert_eq!(unsafe { munmap(addr, expected.len()) }, 0);
        } else {
            let mut bytes = vec![0xff; expected.len()];
            reader.read_exact_at(&mut bytes, 0).unwrap();
            assert_eq!(bytes, expected);
            writer.write_all_at(b"xyz", 8192).unwrap();
        }
        writer.set_len(8193).unwrap();
        writer.set_len(8195).unwrap();
        writer.sync_all().unwrap();
        expected[8192..].copy_from_slice(b"x\0\0");
        assert_eq!(fs::read(path).unwrap(), expected);
    }
}

fn check_write_mapping_race() {
    let path = "/write-map-race";
    for attempt in 0..8 {
        let file = fs::File::create_new(path).unwrap();
        file.write_all_at(b"ab", 0).unwrap();
        let mut bytes = [0; 2];
        file.read_exact_at(&mut bytes, 0).unwrap();
        assert_eq!(bytes, *b"ab");
        let start = std::sync::Barrier::new(2);
        thread::scope(|scope| {
            let write = scope.spawn(|| {
                start.wait();
                file.write_all_at(b"W", 0).unwrap();
            });
            start.wait();
            let addr = unsafe { mmap(std::ptr::null_mut(), 2, 3, 1, file.as_raw_fd(), 0) };
            assert_ne!(addr as isize, -1);
            write.join().unwrap();
            assert_eq!(unsafe { addr.read_volatile() }, b'W');
            unsafe { addr.add(1).write_volatile(b'M') };
            assert_eq!(unsafe { munmap(addr, 2) }, 0);
        });
        // A dirty page can outlive its last mapping.
        file.sync_all().unwrap();
        file.read_exact_at(&mut bytes, 0).unwrap();
        assert_eq!(bytes, *b"WM");
        if attempt < 7 {
            fs::remove_file(path).unwrap();
        }
    }
}

fn check_tmpfs_read() {
    const PAGE: usize = 4096;
    let file = fs::File::create_new("/dev/shm/read-contract").unwrap();
    file.set_len((2 * PAGE + 7) as u64).unwrap();
    file.write_all_at(b"abcd", PAGE as u64).unwrap();

    let mut buf = vec![0xa5; PAGE + 32];
    let n = file.read_at(&mut buf, (PAGE - 3) as u64).unwrap();
    assert_eq!(n, PAGE + 10);
    assert_eq!(&buf[..7], b"\0\0\0abcd");
    assert!(buf[7..n].iter().all(|&b| b == 0));
    assert!(buf[n..].iter().all(|&b| b == 0xa5));

    file.set_len((PAGE + 1) as u64).unwrap();
    buf.fill(0xa5);
    assert_eq!(file.read_at(&mut buf, (PAGE + 1) as u64).unwrap(), 0);
    assert_eq!(file.read_at(&mut buf, (PAGE + 9) as u64).unwrap(), 0);
    assert!(buf.iter().all(|&b| b == 0xa5));
    assert_eq!(file.read_at(&mut buf, PAGE as u64).unwrap(), 1);
    assert_eq!(buf[0], b'a');
    assert!(buf[1..].iter().all(|&b| b == 0xa5));

    file.write_all_at(&[0x31; 8], (3 * PAGE - 4) as u64)
        .unwrap();
    // FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE: span data, a hole, and more data.
    assert_eq!(
        unsafe { fallocate(file.as_raw_fd(), 3, PAGE as i64, (2 * PAGE) as i64) },
        0
    );
    assert_eq!(file.metadata().unwrap().len(), (3 * PAGE + 4) as u64);
    let mut punched = vec![0xa5; 2 * PAGE + 4];
    file.read_exact_at(&mut punched, PAGE as u64).unwrap();
    assert!(punched[..2 * PAGE].iter().all(|&b| b == 0));
    assert_eq!(&punched[2 * PAGE..], &[0x31; 4]);

    file.set_len(1).unwrap();
    let mapping = unsafe { mmap(std::ptr::null_mut(), PAGE, 3, 1, file.as_raw_fd(), 0) };
    assert_ne!(mapping as isize, -1);
    // Stores in the mapped final page past EOF must not become file data on growth.
    unsafe { mapping.add(10).write_volatile(0x55) };
    file.write_all_at(b"x", 11).unwrap();
    assert_eq!(unsafe { munmap(mapping, PAGE) }, 0);
    let mut gap = [0xff; 2];
    file.read_exact_at(&mut gap, 10).unwrap();
    assert_eq!(&gap, b"\0x");

    // Each reader must observe the length and contents of the same resize epoch.
    let short = PAGE - 3;
    let long = 2 * PAGE + 11;
    file.set_len(0).unwrap();
    file.set_len(short as u64).unwrap();
    file.write_all_at(b"q", 0).unwrap();
    let start = std::sync::Barrier::new(3);
    thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                let mut buf = vec![0; long + 1];
                start.wait();
                for _ in 0..512 {
                    buf.fill(0xa5);
                    let n = file.read_at(&mut buf, 0).unwrap();
                    assert!(n == short || n == long);
                    assert_eq!(buf[0], b'q');
                    assert!(buf[1..n].iter().all(|&b| b == 0));
                    assert!(buf[n..].iter().all(|&b| b == 0xa5));
                }
            });
        }
        start.wait();
        for _ in 0..256 {
            file.set_len(long as u64).unwrap();
            file.set_len(short as u64).unwrap();
        }
    });

    // Larger than the 64 MiB CI guest. Reading every hole must not require a frame per page.
    let sparse_len = 96 * 1024 * 1024;
    file.set_len(0).unwrap();
    file.set_len(sparse_len as u64).unwrap();
    for offset in (0..sparse_len).step_by(PAGE) {
        let mut byte = [0xa5];
        file.read_exact_at(&mut byte, offset as u64).unwrap();
        assert_eq!(byte, [0]);
    }
}

fn check_keep_cache_copy_up() {
    let path = "/keep-cache-source";
    let lower = vec![b'a'; 8192];
    assert_eq!(fs::read(path).unwrap(), lower);
    assert_eq!(fs::read(path).unwrap(), lower);

    let writer = fs::OpenOptions::new().write(true).open(path).unwrap();
    writer.write_all_at(b"new", 4095).unwrap();
    writer.sync_all().unwrap();
    drop(writer);

    let mut upper = lower;
    upper[4095..4098].copy_from_slice(b"new");
    assert_eq!(fs::read(path).unwrap(), upper);
    assert_eq!(fs::read(path).unwrap(), upper);
}

fn check_smp(cpus: usize) {
    assert!((1..=64).contains(&cpus));
    let expected = u64::MAX >> (64 - cpus);
    let mut affinity = [0u64; 16];
    assert_eq!(
        unsafe { sched_getaffinity(0, size_of_val(&affinity), affinity.as_mut_ptr()) },
        0,
        "sched_getaffinity: {}",
        std::io::Error::last_os_error(),
    );
    assert_eq!(affinity[0], expected);
    assert!(affinity[1..].iter().all(|&part| part == 0));

    let spawning = AtomicBool::new(true);
    let observed = thread::scope(|scope| {
        let workers: Vec<_> = (0..2 * cpus)
            .map(|_| {
                thread::Builder::new()
                    .stack_size(64 * 1024)
                    .spawn_scoped(scope, || {
                        let cpu = unsafe { sched_getcpu() };
                        while spawning.load(Ordering::Acquire) {
                            std::hint::spin_loop();
                        }
                        cpu
                    })
            })
            .collect();
        spawning.store(false, Ordering::Release);
        workers.into_iter().fold(0, |mask, worker| {
            let cpu = worker.unwrap().join().unwrap();
            assert!((0..cpus as i32).contains(&cpu), "unexpected CPU {cpu}");
            mask | (1 << cpu)
        })
    });
    assert_eq!(observed, expected, "workers did not execute on every CPU");
    println!("RUST_NATIVE_KERNEL_SMP_OK {observed:016x}");
}

fn main() {
    let boot_id_path = "/proc/sys/kernel/random/boot_id";
    let boot_id = fs::read(boot_id_path).unwrap();
    assert_eq!(boot_id.len(), 37);
    assert_eq!(boot_id[14], b'4');
    assert!(matches!(boot_id[19], b'8' | b'9' | b'a' | b'b'));
    for (index, byte) in boot_id.iter().copied().enumerate() {
        match index {
            8 | 13 | 18 | 23 => assert_eq!(byte, b'-'),
            36 => assert_eq!(byte, b'\n'),
            _ => assert!(byte.is_ascii_hexdigit()),
        }
    }
    assert_eq!(fs::read(boot_id_path).unwrap(), boot_id);
    let uuid_path = "/proc/sys/kernel/random/uuid";
    let mut uuid = fs::File::open(uuid_path).unwrap();
    let mut id = [0; 37];
    uuid.read_exact(&mut id[..9]).unwrap();
    uuid.read_exact(&mut id[9..]).unwrap();
    assert_eq!(uuid.read(&mut [0; 1]).unwrap(), 0);
    uuid.rewind().unwrap();
    let mut reread = [0; 37];
    uuid.read_exact(&mut reread).unwrap();
    assert_eq!(id, reread);
    assert_ne!(id.as_slice(), fs::read(uuid_path).unwrap());
    fs::write("/pvisor-kernel-smoke", b"shared rootfs\n").unwrap();
    assert_eq!(
        fs::read("/pvisor-kernel-smoke").unwrap(),
        b"shared rootfs\n"
    );
    let mut append = fs::OpenOptions::new()
        .append(true)
        .open("/pvisor-kernel-smoke")
        .unwrap();
    append.write_all(b"appended\n").unwrap();
    assert_eq!(
        fs::read("/pvisor-kernel-smoke").unwrap(),
        b"shared rootfs\nappended\n"
    );
    let root = fs::File::open("/").unwrap();
    let mut token = [0u8; 16];
    assert_eq!(
        unsafe { ioctl(root.as_raw_fd(), 0x8010_7603u32 as i32, token.as_mut_ptr()) },
        -1
    );
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(1));
    let report = [0u8; 20];
    assert_eq!(
        unsafe { ioctl(root.as_raw_fd(), 0x4014_7604, report.as_ptr()) },
        -1
    );
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(1));
    assert_eq!(unsafe { ioctl(root.as_raw_fd(), 0x7602, 0u64) }, -1);
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(25));
    let mut args = std::env::args().skip(1);
    let code = args.next().unwrap().parse().unwrap();
    while let Some(option) = args.next() {
        match option.as_str() {
            "--echo-stdin" => {
                std::io::copy(&mut std::io::stdin().lock(), &mut std::io::stdout().lock()).unwrap();
            }
            "--smp" => check_smp(args.next().unwrap().parse().unwrap()),
            "--check-tmpfs-read" => check_tmpfs_read(),
            "--check-write-cache" => {
                check_write_cache();
                check_write_mapping_race();
                check_setid_writes();
            }
            "--check-keep-cache-copy-up" => check_keep_cache_copy_up(),
            "--check-read-fixture" => {
                let file = fs::File::open("/read-fixture").unwrap();
                // A preexisting fixture avoids warming the guest cache by writing the data.
                let expected = |offset: usize| (offset / 4096 + offset % 251) as u8;
                for (offset, count) in [(8192, 1), (17, 256 * 1024), (4 * 1024 * 1024 - 9, 64)] {
                    let mut bytes = vec![0; count];
                    let read = file.read_at(&mut bytes, offset as u64).unwrap();
                    assert_eq!(read, count.min(4 * 1024 * 1024 - offset));
                    assert!((0..read).all(|i| bytes[i] == expected(offset + i)));
                }
            }
            _ => panic!("unknown probe option: {option}"),
        }
    }
    println!("RUST_NATIVE_KERNEL_SHARED_ROOTFS_OK");
    eprintln!("RUST_NATIVE_KERNEL_STDERR_OK");
    std::process::exit(code);
}
