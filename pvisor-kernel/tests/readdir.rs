//! Standalone getdents64 regression for a disposable shared-rootfs fixture.

use std::{
    fs::File,
    io::{self, Seek, SeekFrom},
    os::fd::AsRawFd,
    time::Instant,
};

#[cfg(target_arch = "x86_64")]
const SYS_GETDENTS64: std::ffi::c_long = 217;
#[cfg(target_arch = "aarch64")]
const SYS_GETDENTS64: std::ffi::c_long = 61;

const ENTRY_COUNT: usize = 8192;

unsafe extern "C" {
    fn syscall(number: std::ffi::c_long, ...) -> std::ffi::c_long;
}

fn getdents(dir: &File, buf: &mut [u8]) -> io::Result<usize> {
    let result = unsafe { syscall(SYS_GETDENTS64, dir.as_raw_fd(), buf.as_mut_ptr(), buf.len()) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(result as usize)
}

fn scan(dir: &File, buf: &mut [u8]) -> (usize, u64, Vec<String>) {
    let mut calls = 0;
    let mut first_cookie = None;
    let mut names = Vec::new();
    loop {
        calls += 1;
        let len = getdents(dir, buf).unwrap();
        if len == 0 {
            break;
        }
        let mut bytes = &buf[..len];
        while !bytes.is_empty() {
            let cookie = u64::from_ne_bytes(bytes[8..16].try_into().unwrap());
            let record_len = u16::from_ne_bytes(bytes[16..18].try_into().unwrap()) as usize;
            assert!(record_len >= 24 && record_len % 8 == 0);
            let name = &bytes[19..record_len];
            let end = name.iter().position(|&b| b == 0).unwrap();
            first_cookie.get_or_insert(cookie);
            names.push(std::str::from_utf8(&name[..end]).unwrap().to_owned());
            assert!(names.len() <= ENTRY_COUNT + 2, "too many directory entries");
            bytes = &bytes[record_len..];
        }
    }
    (calls, first_cookie.unwrap(), names)
}

fn main() {
    let mut dir = File::open("/readdir-fixture").unwrap();

    // Failure to fit even one record must leave the directory cursor unchanged.
    assert_eq!(
        getdents(&dir, &mut [0; 16]).unwrap_err().raw_os_error(),
        Some(22)
    );
    let (_, first_cookie, full) = scan(&dir, &mut [0; 257]);
    let mut sorted: Vec<_> = full
        .iter()
        .map(String::as_str)
        .filter(|name| !matches!(*name, "." | ".."))
        .collect();
    sorted.sort_unstable();
    assert_eq!(sorted.len(), ENTRY_COUNT);
    for (i, name) in sorted.into_iter().enumerate() {
        assert_eq!(name, format!("entry-{i:05}"));
    }
    assert_eq!(getdents(&dir, &mut [0; 257]).unwrap(), 0);

    // Resume using a server cookie, not a record count.
    dir.seek(SeekFrom::Start(first_cookie)).unwrap();
    let (_, _, resumed) = scan(&dir, &mut [0; 32 * 1024]);
    assert!(resumed == full[1..], "seek did not resume at the cookie");

    for size in [4096, 8191, 32 * 1024, 64 * 1024] {
        dir.rewind().unwrap();
        let mut buf = vec![0; size];
        let start = Instant::now();
        let (calls, _, names) = scan(&dir, &mut buf);
        let elapsed = start.elapsed();
        assert!(
            names == full,
            "directory scan differed at buffer size {size}"
        );
        println!(
            "READDIR_OK size={size} calls={calls} ns={}",
            elapsed.as_nanos()
        );
    }
}
