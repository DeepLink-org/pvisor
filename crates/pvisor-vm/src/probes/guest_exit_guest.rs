//! Standalone static Linux workload for the real-VM exit authority regression.
use std::{fs::File, os::fd::AsRawFd};

unsafe extern "C" {
    fn ioctl(fd: i32, request: i32, ...) -> i32;
}

fn main() {
    let root = File::open("/").unwrap();
    let mut token = [0u8; 16];
    // This workload runs as guest root, but is not the supervisor/PID 1.
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
    println!("VM_EXIT_SOURCE_OK");
    std::process::exit(std::env::args().nth(1).unwrap().parse().unwrap());
}
