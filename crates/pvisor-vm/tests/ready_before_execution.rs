//! Public ready contract against real KVM/HVF, isolated in one process per VM.
#![cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
use pvisor_vm::api::{VmBuilder, VmConfiguration, VmControl, VmRuntime};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    process::Command,
    time::{Duration, Instant},
};

fn ram_digest(file: &File) -> Vec<u8> {
    let mut file = file.try_clone().unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut digest = Sha256::new();
    let mut bytes = vec![0; 1024 * 1024];
    loop {
        let length = file.read(&mut bytes).unwrap();
        if length == 0 {
            break;
        }
        digest.update(&bytes[..length]);
    }
    digest.finalize().to_vec()
}

#[test]
#[ignore = "requires real KVM/HVF and PVISOR_CASE_VM_FIRMWARE_PATH; serial 1/2 CPU VMs"]
fn fresh_ready_holds_all_cpus_and_abort_never_changes_guest_ram() {
    const CHILD: &str = "PVISOR_READY_CPU_CHILD";
    if let Ok(cpus) = std::env::var(CHILD) {
        let root = tempfile::tempdir().unwrap();
        let backing = tempfile::tempfile().unwrap();
        let mut vm = VmBuilder::new(cpus.parse().unwrap(), 256).unwrap();
        vm.set_firmware_path(
            std::env::var_os("PVISOR_CASE_VM_FIRMWARE_PATH")
                .unwrap()
                .into(),
        )
        .unwrap();
        vm.filesystem("/dev/root", root.path(), 0).unwrap();
        vm.ram_backing(backing.try_clone().unwrap()).unwrap();
        let error = vm
            .run(|handle| {
                assert_eq!(handle.is_paused(), Ok(true));
                let before = ram_digest(&backing);
                std::thread::sleep(Duration::from_millis(250));
                assert_eq!(
                    ram_digest(&backing),
                    before,
                    "guest executed before readiness decision"
                );
                assert_eq!(handle.is_paused(), Ok(true));
                Err(io::Error::new(io::ErrorKind::Interrupted, "ready refused"))
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(error.to_string(), "ready refused");
        return;
    }
    assert!(
        std::env::var_os("PVISOR_CASE_VM_FIRMWARE_PATH").is_some(),
        "set trusted local firmware path"
    );
    for cpus in ["1", "2"] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "fresh_ready_holds_all_cpus_and_abort_never_changes_guest_ram",
                "--ignored",
                "--nocapture",
            ])
            .env(CHILD, cpus)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "{cpus}-CPU ready child: {status}");
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("ready child timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
