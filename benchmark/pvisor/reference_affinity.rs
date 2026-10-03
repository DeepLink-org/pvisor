//! Set the container workload's affinity before exec (runc resets inherited affinity).
use std::{
    os::{raw::c_int, unix::process::CommandExt},
    process::Command,
};
unsafe extern "C" {
    fn sched_setaffinity(pid: c_int, size: usize, mask: *const u64) -> c_int;
}
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    assert!(args.len() >= 2, "usage: affinity cpu-list command [args]");
    let mut mask = [0u64; 16];
    for cpu in args[0]
        .split(',')
        .map(|v| v.parse::<usize>().expect("CPU number"))
    {
        assert!(cpu < 1024);
        mask[cpu / 64] |= 1 << (cpu % 64);
    }
    assert_eq!(
        unsafe { sched_setaffinity(0, std::mem::size_of_val(&mask), mask.as_ptr()) },
        0,
        "set affinity: {}",
        std::io::Error::last_os_error()
    );
    panic!("exec: {}", Command::new(&args[1]).args(&args[2..]).exec());
}
