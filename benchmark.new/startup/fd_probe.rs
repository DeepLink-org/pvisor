// Benchmark: B-STARTUP-DIAG; diagnostic only, never VM performance evidence.
// Question: is the first high-FD duplicate slow only after threads share the table?
// Fresh process per trial; compare single, threaded, and preallocated modes.
// No VM, global settings, descriptor-limit changes or unrelated-process cleanup.
#[cfg(not(target_os = "linux"))]
compile_error!("fd_probe models Linux F_DUPFD_CLOEXEC only");

use std::{fs::File, os::fd::AsRawFd, sync::Arc, time::Instant};
unsafe extern "C" {
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    fn close(fd: i32) -> i32;
}
fn duplicate(fd: i32) -> i32 {
    let copy = unsafe { fcntl(fd, 1030, 202) }; // Linux F_DUPFD_CLOEXEC, exact runner minimum.
    assert!(copy >= 202, "{}", std::io::Error::last_os_error());
    copy
}
fn main() {
    let mode = std::env::args()
        .nth(1)
        .expect("single|threaded|preallocated");
    assert!(matches!(
        mode.as_str(),
        "single" | "threaded" | "preallocated"
    ));
    let file = File::open("/dev/null").unwrap();
    if mode == "preallocated" {
        unsafe {
            close(duplicate(file.as_raw_fd()));
        }
    }
    let ready = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    let thread = if mode != "single" {
        let child_ready = ready.clone();
        let child_release = release.clone();
        let thread = std::thread::spawn(move || {
            child_ready.wait();
            child_release.wait();
        });
        ready.wait();
        Some(thread)
    } else {
        None
    };
    let before = Instant::now();
    let first = duplicate(file.as_raw_fd());
    let first_ns = before.elapsed().as_nanos();
    let before = Instant::now();
    let second = duplicate(file.as_raw_fd());
    let second_ns = before.elapsed().as_nanos();
    assert_ne!(first, second, "each duplicate owns a distinct descriptor");
    unsafe {
        close(first);
        close(second);
    }
    if let Some(thread) = thread {
        release.wait();
        thread.join().unwrap();
    }
    println!("{{\"mode\":\"{mode}\",\"first_ns\":{first_ns},\"second_ns\":{second_ns}}}");
}
