//! Minimal init for reference VMs: mount, execute the same payload, report, reboot.
use std::{
    fs,
    os::raw::{c_char, c_int, c_ulong, c_void},
    process::Command,
};
unsafe extern "C" {
    fn mount(
        source: *const c_char,
        target: *const c_char,
        kind: *const c_char,
        flags: c_ulong,
        data: *const c_void,
    ) -> c_int;
    fn socket(domain: c_int, kind: c_int, protocol: c_int) -> c_int;
    fn ioctl(fd: c_int, request: c_ulong, value: *mut c_void) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn sync();
    fn reboot(command: c_int) -> c_int;
}
fn main() {
    for (kind, target) in [
        (c"proc", c"/proc"),
        (c"sysfs", c"/sys"),
        (c"devtmpfs", c"/dev"),
    ] {
        let result = unsafe {
            mount(
                kind.as_ptr(),
                target.as_ptr(),
                kind.as_ptr(),
                0,
                std::ptr::null(),
            )
        };
        if result != 0
            && !(target == c"/dev" && std::io::Error::last_os_error().raw_os_error() == Some(16))
        {
            panic!("mount {:?}: {}", target, std::io::Error::last_os_error());
        }
    }
    // Bring up guest loopback for the local deterministic model, without a NIC.
    let fd = unsafe { socket(2, 2, 0) };
    assert!(fd >= 0, "loopback socket");
    let mut interface = [0u8; 40];
    interface[..2].copy_from_slice(b"lo");
    let ptr = interface.as_mut_ptr().cast::<c_void>();
    assert_eq!(unsafe { ioctl(fd, 0x8913, ptr) }, 0, "get lo flags");
    let flags = i16::from_ne_bytes([interface[16], interface[17]]) | 1;
    interface[16..18].copy_from_slice(&flags.to_ne_bytes());
    assert_eq!(unsafe { ioctl(fd, 0x8914, ptr) }, 0, "bring up lo");
    unsafe {
        close(fd);
    }
    let cmdline = fs::read_to_string("/proc/cmdline").expect("cmdline");
    let mode = cmdline
        .split_whitespace()
        .find_map(|arg| arg.strip_prefix("pvbench.mode="))
        .unwrap_or("ready");
    let mut command = if mode == "ready" {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "printf 'REFERENCE_READY\\nREFERENCE_RESULT {\"mode\":\"ready\",\"correctness\":\"passed\"}\\n'"]);
        cmd
    } else {
        let mut cmd = Command::new("/usr/bin/python3");
        cmd.args(["/bench/reference_workload.py", "--mode", mode]);
        cmd
    };
    let status = command
        .current_dir("/work")
        .env("PATH", "/opt/toolchain/bin:/usr/local/bin:/usr/bin:/bin")
        .env("HOME", "/root")
        .status()
        .expect("payload");
    println!("REFERENCE_EXIT {}", status.code().unwrap_or(255));
    unsafe {
        sync();
        reboot(0x01234567);
    }
    panic!("reboot failed");
}
