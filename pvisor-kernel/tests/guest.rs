//! Standalone static guest workload for shared-rootfs and exit-authority validation.

use std::{
    fs,
    io::{Read, Seek, Write},
    os::fd::AsRawFd,
};

unsafe extern "C" {
    fn ioctl(fd: i32, request: i32, ...) -> i32;
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
    if std::env::args().nth(2).as_deref() == Some("--echo-stdin") {
        std::io::copy(&mut std::io::stdin().lock(), &mut std::io::stdout().lock()).unwrap();
    }
    println!("RUST_NATIVE_KERNEL_SHARED_ROOTFS_OK");
    eprintln!("RUST_NATIVE_KERNEL_STDERR_OK");
    std::process::exit(std::env::args().nth(1).unwrap().parse().unwrap());
}
