//! Production-library teardown, not the unit-test vCPU exit implementation.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use pvisor_vm::api::{VmBuilder, VmConfiguration, VmControl, VmRuntime};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read},
    os::unix::{fs::FileExt, net::UnixStream},
    path::PathBuf,
    time::Duration,
};

fn resources() -> (BTreeSet<(PathBuf, PathBuf)>, BTreeSet<PathBuf>) {
    // Close read_dir before resolving links so its own fd is not counted.
    let paths: Vec<_> = fs::read_dir("/proc/self/fd")
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    let fds = paths
        .into_iter()
        .filter_map(|path| fs::read_link(&path).ok().map(|target| (path, target)))
        .collect();
    let tids = fs::read_dir("/proc/self/task")
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    (fds, tids)
}

#[test]
#[ignore = "requires /dev/kvm and PVISOR_CASE_VM_FIRMWARE_PATH; one serial 256 MiB VM"]
fn ready_abort_releases_vcpu_bus_and_owned_descriptors_before_retry() {
    let firmware = PathBuf::from(
        std::env::var_os("PVISOR_CASE_VM_FIRMWARE_PATH")
            .expect("set an absolute trusted local firmware path"),
    );
    let root = tempfile::tempdir().unwrap();
    let baseline = resources();
    let mut stale = None;
    // A second readiness receipt proves later startup is not poisoned.
    for cycle in 0..2 {
        let backing = tempfile::tempfile().unwrap();
        let (network, mut peer) = UnixStream::pair().unwrap();
        let mut vm = VmBuilder::new(1, 256).unwrap();
        vm.set_firmware_path(firmware.clone()).unwrap();
        vm.filesystem("/dev/root", root.path(), 0).unwrap();
        vm.ram_backing(backing.try_clone().unwrap()).unwrap();
        vm.network(network, [2, 0, 0, 0, 0, 44]).unwrap();
        let error = vm
            .run(|handle| {
                assert!(handle.is_paused().is_ok());
                if cycle == 0 {
                    stale = Some(handle);
                } else {
                    handle.pause().unwrap();
                    assert_eq!(handle.is_paused(), Ok(true));
                }
                Err(io::Error::new(io::ErrorKind::Interrupted, "ready rejected"))
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(error.to_string(), "ready rejected");
        assert!(stale.as_ref().unwrap().is_paused().is_err());
        // Caller ownership remains usable; only runtime duplicates must close.
        backing.write_all_at(b"owned", 0).unwrap();
        drop(backing);
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        drop(peer);
        assert_eq!(
            resources(),
            baseline,
            "cycle {cycle} leaked fd/thread owners"
        );
    }
    drop(stale);
    assert_eq!(resources(), baseline);
}
