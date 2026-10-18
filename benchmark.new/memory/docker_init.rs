//! Fixed-affinity launcher for the Docker memory experiment, then execs its oracle.
//! Rootless Docker may lack delegated cpuset control; pin before payload creation.
use std::os::unix::process::CommandExt;
use std::process::Command;

unsafe extern "C" {
    fn sched_setaffinity(pid: i32, size: usize, mask: *const u8) -> i32;
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    assert!(!args.is_empty(), "oracle executable required");
    let mut mask = [0u8; 128];
    mask[0] = 0x0f;
    if unsafe { sched_setaffinity(0, mask.len(), mask.as_ptr()) } != 0 {
        panic!("cannot install benchmark affinity: {}", std::io::Error::last_os_error());
    }
    panic!("oracle exec failed: {}", Command::new(&args[0]).args(&args[1..]).exec());
}
