//! Real Linux CPU QoS anchor lifecycle using the native pvisor launcher.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
use std::{fs, path::Path};

fn cpu_cookie(pid: u32) -> u64 {
    let mut cookie = 0_u64;
    assert_eq!(
        unsafe {
            libc::prctl(
                62,
                0 as libc::c_ulong,
                libc::c_ulong::from(pid),
                0 as libc::c_ulong,
                &mut cookie as *mut u64,
            )
        },
        0,
        "read actual core cookie for {pid}: {}",
        std::io::Error::last_os_error()
    );
    cookie
}

fn worker_children(pid: u32) -> std::collections::BTreeSet<u32> {
    let mut children = std::collections::BTreeSet::new();
    for thread in fs::read_dir(format!("/proc/{pid}/task")).unwrap() {
        if let Ok(ids) = fs::read_to_string(thread.unwrap().path().join("children")) {
            children.extend(ids.split_whitespace().map(|id| id.parse::<u32>().unwrap()));
        }
    }
    children
}

#[test]
#[ignore = "requires Linux core scheduling support"]
fn native_cpu_anchor_lifetime() {
    let parent = std::process::id();
    let original = cpu_cookie(parent);
    let policy = unsafe { libc::sched_getscheduler(0) };
    let before = worker_children(parent);
    let group =
        pvisor::CpuQosGroup::with_launcher(Path::new(env!("CARGO_BIN_EXE_pvisor"))).unwrap();
    let after = worker_children(parent);
    let owners: Vec<_> = after.difference(&before).copied().collect();
    assert_eq!(owners.len(), 1);
    let owner = owners[0];
    assert_ne!(cpu_cookie(owner), 0);
    assert_ne!(cpu_cookie(owner), original);
    assert_eq!(cpu_cookie(parent), original);
    assert_eq!(unsafe { libc::sched_getscheduler(0) }, policy);
    let environment = fs::read(format!("/proc/{owner}/environ")).unwrap();
    assert_eq!(environment, b"PVISOR_NATIVE_CPU_QOS_ANCHOR=1\0");
    let retained = group.clone();
    drop(group);
    assert!(Path::new(&format!("/proc/{owner}")).exists());
    drop(retained);
    assert!(
        !Path::new(&format!("/proc/{owner}")).exists(),
        "last owner must reap the anchor"
    );
}
