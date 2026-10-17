//! Standalone musl guest payload for asynchronous, multi-file writeback.
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::fd::{AsRawFd, IntoRawFd},
    os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    },
    time::{Duration, Instant},
};

fn check_writeback_budget(mount: &str) {
    let device = fs::metadata(mount).unwrap().dev();
    let major = ((device >> 8) & 0xfff) | ((device >> 32) & 0xffff_f000);
    let minor = (device & 0xff) | ((device >> 12) & 0xffff_ff00);
    let path = format!("/sys/class/bdi/{major}:{minor}");
    assert_eq!(
        fs::read_to_string(format!("{path}/strict_limit"))
            .unwrap()
            .trim(),
        "1"
    );
    assert_eq!(
        fs::read_to_string(format!("{path}/max_ratio"))
            .unwrap()
            .trim(),
        "100"
    );
    let minimum: u64 = fs::read_to_string(format!("{path}/min_bytes"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        minimum > 8 << 20,
        "missing writeback working-set reservation for {mount}"
    );
    println!("WB_BDI_VERIFIED {mount} {major}:{minor} strict=1 max=100 min_bytes={minimum}");
}

unsafe extern "C" {
    fn close(fd: i32) -> i32;
    fn mount(
        source: *const std::ffi::c_char,
        target: *const std::ffi::c_char,
        kind: *const std::ffi::c_char,
        flags: usize,
        data: *const std::ffi::c_void,
    ) -> i32;
    fn syncfs(fd: i32) -> i32;
    fn umount(target: *const std::ffi::c_char) -> i32;
    fn mmap(
        address: *mut std::ffi::c_void,
        length: usize,
        protection: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut std::ffi::c_void;
    fn msync(address: *mut std::ffi::c_void, length: usize, flags: i32) -> i32;
    fn munmap(address: *mut std::ffi::c_void, length: usize) -> i32;
}

const REDIRTY_THREADS: usize = 4;
const REDIRTY_PASSES: usize = 3;
const REDIRTY_SLOTS: usize = 512;
const REDIRTY_SLOT_BYTES: usize = 1024;
const REDIRTY_PREFIX: usize = 256;

fn redirty_bytes(worker: usize, pass: usize, slot: usize) -> [u8; REDIRTY_SLOT_BYTES] {
    std::array::from_fn(|index| {
        (worker as u8).wrapping_mul(0x35)
            ^ (pass as u8).wrapping_mul(0x43)
            ^ (slot as u8).wrapping_mul(0x1d)
            ^ (index as u8).wrapping_mul(7)
    })
}

fn checked_close(file: File) {
    // close may have consumed the descriptor even on error; never retry it.
    assert_eq!(
        unsafe { close(file.into_raw_fd()) },
        0,
        "close failed: {}",
        std::io::Error::last_os_error()
    );
}

fn concurrent_redirty() {
    const PATH: &str = "/probe/multifd-redirty";
    let length = REDIRTY_PREFIX + REDIRTY_THREADS * REDIRTY_SLOTS * REDIRTY_SLOT_BYTES;
    let initial = File::create(PATH).unwrap();
    initial.set_len(length as u64).unwrap();
    let inode = initial.metadata().unwrap().ino();
    checked_close(initial);
    let barrier = Arc::new(Barrier::new(REDIRTY_THREADS));
    let active = Arc::new(AtomicUsize::new(0));
    let reopens_with_peer = Arc::new(AtomicUsize::new(0));
    let mut threads = Vec::new();
    for worker in 0..REDIRTY_THREADS {
        let barrier = barrier.clone();
        let active = active.clone();
        let reopens_with_peer = reopens_with_peer.clone();
        threads.push(std::thread::spawn(move || {
            let open = || {
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(PATH)
                    .unwrap()
            };
            let mut file = open();
            assert_eq!(file.metadata().unwrap().ino(), inode);
            active.fetch_add(1, Ordering::SeqCst);
            let mut reopens = 0;
            for pass in 0..REDIRTY_PASSES {
                barrier.wait();
                for slot in 0..REDIRTY_SLOTS {
                    // Bytes never overlap between workers. Their 1-KiB slots
                    // share 4-KiB pages; the 256-byte prefix also makes every
                    // fourth slot straddle a page boundary. Each pass dirties
                    // these same pages again, without a per-pass fsync.
                    let offset =
                        REDIRTY_PREFIX + (slot * REDIRTY_THREADS + worker) * REDIRTY_SLOT_BYTES;
                    assert_eq!(
                        file.write_at(&redirty_bytes(worker, pass, slot), offset as u64)
                            .unwrap(),
                        REDIRTY_SLOT_BYTES,
                        "short concurrent pwrite"
                    );
                    if (slot + worker * 11 + 1) % 53 == 0 {
                        if active.load(Ordering::SeqCst) > 1 {
                            reopens_with_peer.fetch_add(1, Ordering::SeqCst);
                        }
                        checked_close(file);
                        file = open();
                        reopens += 1;
                    }
                    if (slot + worker * 29 + 1) % 127 == 0 {
                        // Let the 10-ms submit timer run while all writers are
                        // still live. This is a lifecycle oracle, not timing.
                        std::thread::sleep(Duration::from_millis(15));
                    }
                }
            }
            checked_close(file);
            active.fetch_sub(1, Ordering::SeqCst);
            reopens
        }));
    }
    let reopens: usize = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .sum();
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert!(reopens_with_peer.load(Ordering::SeqCst) > 0);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(PATH)
        .unwrap();
    file.sync_all().unwrap();
    assert_eq!(
        unsafe { syncfs(file.as_raw_fd()) },
        0,
        "concurrent syncfs failed: {}",
        std::io::Error::last_os_error()
    );
    checked_close(file);
    let mut expected = vec![0; length];
    for slot in 0..REDIRTY_SLOTS {
        for worker in 0..REDIRTY_THREADS {
            let offset = REDIRTY_PREFIX + (slot * REDIRTY_THREADS + worker) * REDIRTY_SLOT_BYTES;
            expected[offset..offset + REDIRTY_SLOT_BYTES].copy_from_slice(&redirty_bytes(
                worker,
                REDIRTY_PASSES - 1,
                slot,
            ));
        }
    }
    assert_eq!(
        fs::read(PATH).unwrap(),
        expected,
        "concurrent redirty lost bytes"
    );
    println!("GUEST_WRITEBACK_MULTI_FD_PASS threads={REDIRTY_THREADS} passes={REDIRTY_PASSES} reopens={reopens} bytes={length}");
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        eprintln!("{info}");
        use std::os::unix::fs::OpenOptionsExt;
        if let Ok(mut log) = OpenOptions::new()
            .read(true)
            .custom_flags(0x800)
            .open("/dev/kmsg")
        {
            let mut bytes = [0; 8192];
            while let Ok(count) = log.read(&mut bytes) {
                if count == 0 {
                    break;
                }
                let line = String::from_utf8_lossy(&bytes[..count]);
                if line.contains("pvisor") || line.contains("WARNING") {
                    eprintln!("{line}");
                }
            }
        }
    }));
    if std::env::var("PVISOR_TEST_WRITEBACK_BUDGET").as_deref() == Ok("1") {
        // Other registered devices (for example loop devices) do not negotiate
        // this extension. Check the exact filesystem, not every system BDI.
        check_writeback_budget("/");
    }
    fs::create_dir("/probe").unwrap();
    fs::create_dir("/batch-share").unwrap();
    assert_eq!(
        unsafe {
            mount(
                c"writeback-extra".as_ptr(),
                c"/batch-share".as_ptr(),
                c"virtiofs".as_ptr(),
                0,
                std::ptr::null(),
            )
        },
        0
    );
    if std::env::var("PVISOR_TEST_WRITEBACK_BUDGET").as_deref() == Ok("1") {
        check_writeback_budget("/batch-share");
    }
    if std::env::args().nth(1).as_deref() == Some("error") {
        fs::write("/batch-share/failure", vec![0x5a; 128 << 10]).unwrap();
        fs::create_dir("/probe/error-armed").unwrap();
        println!("GUEST_WRITEBACK_ERROR_ARMED");
        return;
    }
    for index in 0..8 {
        let path = format!("/probe/closed-unlinked-{index}");
        fs::write(&path, b"pending metadata").unwrap();
        if index % 2 == 0 {
            std::thread::sleep(Duration::from_millis(40));
        }
        fs::remove_file(&path).unwrap();
        let directory = File::open("/probe").unwrap();
        assert_eq!(
            unsafe { syncfs(directory.as_raw_fd()) },
            0,
            "closed/unlinked metadata was not synchronized: {}",
            std::io::Error::last_os_error()
        );
    }
    fs::write("/probe/closed-victim", b"victim").unwrap();
    fs::write("/probe/closed-source", b"replacement").unwrap();
    std::thread::sleep(Duration::from_millis(40));
    fs::rename("/probe/closed-source", "/probe/closed-victim").unwrap();
    let directory = File::open("/probe").unwrap();
    assert_eq!(
        unsafe { syncfs(directory.as_raw_fd()) },
        0,
        "closed rename victim metadata was not synchronized: {}",
        std::io::Error::last_os_error()
    );
    assert_eq!(fs::read("/probe/closed-victim").unwrap(), b"replacement");
    if let Ok(budget) = fs::read_to_string("/sys/module/fuse/parameters/pvisor_wb_budget_bytes") {
        let budget: u64 = budget.trim().parse().unwrap();
        assert!((16 << 20..=256 << 20).contains(&budget));
        println!("WB_PRESSURE_TARGET_VERIFIED bytes={budget}");
    }
    println!("GUEST_WRITEBACK_DETACHED_PASS");
    fs::write("/batch-share/closed-before-unmount", b"drained").unwrap();
    assert_eq!(unsafe { umount(c"/batch-share".as_ptr()) }, 0);
    fs::create_dir("/control").unwrap();
    assert_eq!(
        unsafe {
            mount(
                c"writeback-control".as_ptr(),
                c"/control".as_ptr(),
                c"virtiofs".as_ptr(),
                0,
                std::ptr::null(),
            )
        },
        0
    );
    fs::write("/probe/timer", b"timer").unwrap();
    fs::create_dir("/probe/timer-ready").unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    while fs::read("/control/timer-ack").is_err() {
        assert!(
            Instant::now() < deadline,
            "idle write never reached the host"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(unsafe { umount(c"/control".as_ptr()) }, 0);
    for index in 0..256 {
        let path = format!("/probe/file-{index:04}");
        let data = vec![index as u8; 65536];
        fs::write(&path, &data).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), data.len() as u64);
        assert_eq!(
            fs::read(&path).unwrap(),
            data,
            "close/reopen lost cached bytes"
        );
    }
    fs::write("/probe/deleted", b"retained").unwrap();
    let mut held = File::open("/probe/deleted").unwrap();
    fs::remove_file("/probe/deleted").unwrap();
    let mut data = Vec::new();
    held.read_to_end(&mut data).unwrap();
    assert_eq!(data, b"retained");
    fs::write("/probe/source", b"renamed").unwrap();
    fs::write("/probe/renamed", b"original").unwrap();
    let mut held = File::open("/probe/renamed").unwrap();
    fs::rename("/probe/source", "/probe/renamed").unwrap();
    let mut data = Vec::new();
    held.read_to_end(&mut data).unwrap();
    assert_eq!(data, b"original");
    assert_eq!(fs::read("/probe/renamed").unwrap(), b"renamed");
    fs::write("/probe/truncate", vec![0x44; 131072]).unwrap();
    let mut file = OpenOptions::new()
        .write(true)
        .open("/probe/truncate")
        .unwrap();
    file.set_len(3).unwrap();
    file.seek(SeekFrom::Start(1)).unwrap();
    file.write_all(b"x").unwrap();
    file.sync_all().unwrap();
    assert_eq!(fs::read("/probe/truncate").unwrap(), b"DxD");
    // One write exceeds the RAM/8 pressure target in the 512 MiB guest.
    let mut file = File::create("/probe/pressure").unwrap();
    file.write_all(&vec![0x5a; 128 << 20]).unwrap();
    file.sync_all().unwrap();
    assert_eq!(unsafe { syncfs(file.as_raw_fd()) }, 0);
    drop(file);
    // Repeated writes of the same page must not consume a cumulative-byte budget.
    let mut overwritten = File::create("/probe/overwritten").unwrap();
    for index in 0..16384 {
        overwritten.seek(SeekFrom::Start(0)).unwrap();
        overwritten.write_all(&[index as u8; 4096]).unwrap();
    }
    overwritten.sync_all().unwrap();
    assert_eq!(fs::read("/probe/overwritten").unwrap(), vec![255; 4096]);
    let mut synchronous = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .custom_flags(0x101000)
        .open("/probe/synchronous")
        .unwrap(); // Linux O_SYNC
    synchronous.write_all(&[0x73; 8192]).unwrap();
    drop(synchronous);
    let mapped = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open("/probe/mapped")
        .unwrap();
    let length = 8 << 20;
    mapped.set_len(length as u64).unwrap();
    let address = unsafe { mmap(std::ptr::null_mut(), length, 3, 1, mapped.as_raw_fd(), 0) }; // shared R/W
    assert_ne!(
        address as isize,
        -1,
        "mmap failed: {}",
        std::io::Error::last_os_error()
    );
    unsafe {
        std::ptr::write_bytes(address.cast::<u8>(), 0x6b, length);
    }
    assert_eq!(unsafe { msync(address, length, 4) }, 0); // MS_SYNC
    mapped.sync_all().unwrap();
    assert_eq!(unsafe { munmap(address, length) }, 0);
    assert_eq!(fs::read("/probe/mapped").unwrap(), vec![0x6b; length]);
    concurrent_redirty();
    // PID 1's final barrier must drain this last short commit before reporting success.
    fs::write("/probe/final", b"final").unwrap();
    println!("GUEST_WRITEBACK_PAYLOAD_PASS");
}
