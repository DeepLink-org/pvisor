//! B-FS-TOOLS stock-kernel PID1. No distribution services or network device.
//! Compile statically with rustc --edition=2024 --target x86_64-unknown-linux-musl.
use std::{
    ffi::{CString, c_char, c_int, c_ulong, c_void},
    fs,
    io::{self, Write},
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
    fn umount(target: *const c_char) -> c_int;
    fn sync();
    fn reboot(command: c_int) -> c_int;
    fn getpid() -> c_int;
}
fn mounted(
    source: Option<&str>,
    target: &str,
    kind: Option<&str>,
    flags: c_ulong,
    options: Option<&str>,
) -> io::Result<()> {
    let source = source.map(|s| CString::new(s).unwrap());
    let target = CString::new(target).unwrap();
    let kind = kind.map(|s| CString::new(s).unwrap());
    let options = options.map(|s| CString::new(s).unwrap());
    let result = unsafe {
        mount(
            source.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            target.as_ptr(),
            kind.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            flags,
            options
                .as_ref()
                .map_or(std::ptr::null(), |s| s.as_ptr().cast()),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
fn run() -> io::Result<i32> {
    if unsafe { getpid() } != 1 {
        return Err(io::Error::other("expected PID1"));
    }
    mounted(Some("proc"), "/proc", Some("proc"), 0, None)?;
    mounted(Some("sysfs"), "/sys", Some("sysfs"), 0, None)?;
    mounted(
        Some("tmpfs"),
        "/tmp",
        Some("tmpfs"),
        0,
        Some("mode=1777,size=256m"),
    )?;
    mounted(
        Some("tmpfs"),
        "/run",
        Some("tmpfs"),
        0,
        Some("mode=0755,size=16m"),
    )?;
    mounted(None, "/", None, 32 | 1, None)?;
    let cmdline = fs::read_to_string("/proc/cmdline")?;
    let cases: Vec<_> = cmdline
        .split_whitespace()
        .filter_map(|s| s.strip_prefix("fsbench.case="))
        .collect();
    if cases.len() != 1
        || cases[0].len() != 3
        || !matches!(cases[0].as_bytes()[0], b'M' | b'R' | b'S')
        || !cases[0].as_bytes()[1..].iter().all(u8::is_ascii_digit)
    {
        return Err(io::Error::other("case ID"));
    }
    for _ in 0..500 {
        if std::path::Path::new("/dev/vdb").exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    mounted(Some("/dev/vdb"), "/bench-work", Some("ext4"), 0, None)?;
    mounted(
        Some("/fs-v2-fixture"),
        "/bench-work/input",
        None,
        4096,
        None,
    )?;
    mounted(None, "/bench-work/input", None, 4096 | 32 | 1, None)?;
    let status = Command::new("/bench-artifacts/payload")
        .args([cases[0], "output", "input", "3", "512"])
        .current_dir("/bench-work")
        .env("HOME", "/tmp")
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("PYTHONPYCACHEPREFIX", "/__pvisor_reference_no_pyc__")
        .status()?;
    std::env::set_current_dir("/")?;
    unsafe {
        sync();
    }
    if unsafe { umount(c"/bench-work/input".as_ptr()) } != 0
        || unsafe { umount(c"/bench-work".as_ptr()) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(status.code().unwrap_or(128))
}
fn main() {
    let result = run();
    if let Err(error) = &result {
        eprintln!("REFERENCE_INIT_ERROR {error}");
    }
    println!(
        "USER_WORKLOADS_VM_EXIT {{\"returncode\":{},\"teardown_ok\":{}}}",
        result.as_ref().copied().unwrap_or(1),
        result.is_ok()
    );
    let _ = std::io::stdout().flush();
    unsafe {
        sync();
        reboot(0x01234567);
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
