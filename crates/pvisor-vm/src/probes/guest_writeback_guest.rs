//! Standalone musl guest payload for asynchronous, multi-file writeback.
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::fd::AsRawFd,
    os::unix::fs::MetadataExt,
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
    fn mount(
        source: *const std::ffi::c_char,
        target: *const std::ffi::c_char,
        kind: *const std::ffi::c_char,
        flags: usize,
        data: *const std::ffi::c_void,
    ) -> i32;
    fn syncfs(fd: i32) -> i32;
    fn umount(target: *const std::ffi::c_char) -> i32;
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
    // More than the pending-byte high-water mark, with a live fd for fsync.
    let mut file = File::create("/probe/pressure").unwrap();
    file.write_all(&vec![0x5a; 32 << 20]).unwrap();
    file.sync_all().unwrap();
    assert_eq!(unsafe { syncfs(file.as_raw_fd()) }, 0);
    drop(file);
    // PID 1's final barrier must drain this last short commit before reporting success.
    fs::write("/probe/final", b"final").unwrap();
    println!("GUEST_WRITEBACK_PAYLOAD_PASS");
}
