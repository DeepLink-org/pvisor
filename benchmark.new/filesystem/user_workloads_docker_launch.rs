//! B-FS-TOOLS: pin and describe the container payload, then exec frozen code.
use std::os::unix::process::CommandExt;
use std::process::Command;

unsafe extern "C" {
    fn sched_setaffinity(pid: i32, cpusetsize: usize, mask: *const u64) -> i32;
    fn sched_getaffinity(pid: i32, cpusetsize: usize, mask: *mut u64) -> i32;
}

fn main() {
    let mut mask = [0_u64; 16];
    mask[0] = 3;
    assert_eq!(unsafe { sched_setaffinity(0, std::mem::size_of_val(&mask), mask.as_ptr()) }, 0,
               "cannot pin Docker payload to CPUs 0,1");
    mask.fill(0);
    assert_eq!(unsafe { sched_getaffinity(0, std::mem::size_of_val(&mask), mask.as_mut_ptr()) }, 0,
               "cannot inspect Docker payload affinity");
    let cpus: Vec<usize> = (0..1024).filter(|cpu| mask[cpu / 64] & (1 << (cpu % 64)) != 0).collect();
    assert_eq!(cpus, vec![0, 1]);
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").expect("mountinfo");
    let root_type = mounts.lines().find_map(|line| {
        let (fields, filesystem) = line.split_once(" - ")?;
        (fields.split_whitespace().nth(4)? == "/").then(|| filesystem.split_whitespace().next().unwrap())
    }).expect("container root mount");
    assert_eq!(root_type, "overlay", "workspace must use Docker overlay2, not bind or tmpfs");
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").expect("cgroup");
    // Values are fixed ASCII configuration values; avoid adding a JSON library
    // to this static single-purpose launcher.
    println!("USER_WORKLOADS_DOCKER_RUNTIME {{\"affinity\":[0,1],\"root_filesystem\":\"overlay\",\"cgroup_namespace_path\":{:?}}}", cgroup.trim());
    let mut arguments = std::env::args().skip(1);
    let executable = arguments.next().expect("payload executable");
    let error = Command::new(executable).args(arguments).exec();
    panic!("payload exec: {error}");
}
