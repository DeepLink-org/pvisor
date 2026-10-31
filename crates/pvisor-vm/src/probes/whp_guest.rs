//! Standalone static Linux payload, compiled with rustc by the Windows CI workflow.
//! This is deliberately not a Cargo example: it runs inside the Linux guest.
use std::os::fd::AsRawFd;
use std::{
    fs, io,
    time::{Duration, Instant},
};

unsafe extern "C" {
    fn ioctl(fd: i32, request: usize, ...) -> i32;
}

fn probe(cpus: &str) -> io::Result<()> {
    assert_eq!(fs::read("/input")?, b"pvisor-whp-input");
    assert_eq!(
        fs::read_to_string("/sys/devices/system/cpu/online")?.trim(),
        if cpus == "1" { "0" } else { "0-1" }
    );
    assert_eq!(fs::read("/readonly/fixture")?, b"immutable");
    assert!(fs::write("/readonly/fixture", b"changed").is_err());
    let start = Instant::now();
    std::thread::sleep(Duration::from_millis(100));
    assert!(start.elapsed() >= Duration::from_millis(100));
    assert!(start.elapsed() < Duration::from_secs(10));
    fs::write("/result", b"pvisor-whp-read-write-ok")?;
    let root = fs::File::open("/")?;
    // An ordinary root workload must not bypass the PID 1 token handshake.
    assert_eq!(unsafe { ioctl(root.as_raw_fd(), 0x7602, 0usize) }, -1);
    assert_eq!(unsafe { ioctl(root.as_raw_fd(), 0x7603, 0usize) }, -1);
    Ok(())
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if let Err(error) = probe(&args[2]) {
        let _ = fs::write("/probe-error", error.to_string());
        std::process::exit(124);
    }
    std::process::exit(args[1].parse().unwrap());
}
