//! Production HVF owners must release the process's VM before returning setup errors.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use pvisor_vm::api::{VmBuilder, VmConfiguration, VmControl, VmRuntime};
use std::{
    collections::BTreeSet,
    io::{self, Read},
    os::unix::{fs::FileExt, net::UnixStream},
    process::Command,
    time::{Duration, Instant},
};

fn resources() -> (BTreeSet<i32>, BTreeSet<u64>) {
    let fds = (0..unsafe { libc::getdtablesize() })
        .filter(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } >= 0)
        .collect();
    // Darwin's PROC_PIDLISTTHREADS returns uint64_t thread identifiers.
    let mut threads = [0u64; 1024];
    let bytes = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            6,
            0,
            threads.as_mut_ptr().cast(),
            std::mem::size_of_val(&threads) as i32,
        )
    };
    assert!(bytes > 0 && (bytes as usize) < std::mem::size_of_val(&threads));
    assert_eq!(bytes as usize % size_of::<u64>(), 0);
    (
        fds,
        threads[..bytes as usize / size_of::<u64>()]
            .iter()
            .copied()
            .collect(),
    )
}

#[test]
#[ignore = "requires real HVF and PVISOR_CASE_VM_FIRMWARE_PATH; serial 1/2 CPU retries"]
fn ready_abort_releases_hvf_owners_before_same_process_retry() {
    const CHILD: &str = "PVISOR_HVF_ABORT_CPUS";
    if let Ok(cpus) = std::env::var(CHILD) {
        let root = tempfile::tempdir().unwrap();
        let baseline = resources();
        let mut stale = None;
        for cycle in 0..4 {
            let backing = tempfile::tempfile().unwrap();
            let (network, mut peer) = UnixStream::pair().unwrap();
            let mut vm = VmBuilder::new(cpus.parse().unwrap(), 256).unwrap();
            vm.set_firmware_path(
                std::env::var_os("PVISOR_CASE_VM_FIRMWARE_PATH")
                    .unwrap()
                    .into(),
            )
            .unwrap();
            vm.filesystem("/dev/root", root.path(), 0).unwrap();
            vm.ram_backing(backing.try_clone().unwrap()).unwrap();
            vm.network(network, [2, 0, 0, 0, 0, 44]).unwrap();
            const CACHE_SETTING: &str = "PVISOR_VM_CACHE_EXPERIMENT";
            let previous = std::env::var_os(CACHE_SETTING);
            if cycle == 0 {
                // Device setup happens after native VM creation, before CPU threads.
                std::env::set_var(CACHE_SETTING, "invalid-cleanup-regression");
            }
            let error = vm
                .run(|handle| {
                    assert_ne!(
                        cycle, 0,
                        "invalid device profile unexpectedly reached ready"
                    );
                    assert_eq!(handle.is_paused(), Ok(true));
                    if cycle == 2 {
                        handle.resume().unwrap();
                    }
                    if cycle == 1 {
                        stale = Some(handle);
                    }
                    Err(io::Error::new(io::ErrorKind::Interrupted, "ready rejected"))
                })
                .unwrap_err();
            if cycle == 0 {
                match previous {
                    Some(value) => std::env::set_var(CACHE_SETTING, value),
                    None => std::env::remove_var(CACHE_SETTING),
                }
                assert!(error.to_string().contains("CreateFsDevice"), "{error}");
            } else {
                assert_eq!(
                    error.kind(),
                    io::ErrorKind::Interrupted,
                    "cycle {cycle}: {error}"
                );
                assert_eq!(error.to_string(), "ready rejected");
                assert!(stale.as_ref().unwrap().is_paused().is_err());
            }
            backing.write_all_at(b"owned", 0).unwrap();
            drop(backing);
            peer.set_nonblocking(true).unwrap();
            assert_eq!(peer.read(&mut [0]).unwrap(), 0);
            drop(peer);
            let current = resources();
            assert_eq!(current, baseline, "cycle {cycle} leaked fd/thread owners");
        }
        return;
    }
    assert!(std::env::var_os("PVISOR_CASE_VM_FIRMWARE_PATH").is_some());
    for cpus in ["1", "2"] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ready_abort_releases_hvf_owners_before_same_process_retry",
                "--ignored",
                "--nocapture",
            ])
            .env(CHILD, cpus)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "{cpus}-CPU retry child: {status}");
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("HVF abort/retry child timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
