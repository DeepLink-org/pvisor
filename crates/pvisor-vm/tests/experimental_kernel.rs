//! Process-isolated public API checks: never mutate the test runner's environment.
#![cfg(unix)]

use proptest::prelude::*;
use pvisor_vm::api::{RuntimeSupport, VmBuilder, VmConfiguration, VmPlatform, VmRuntime};
use std::{
    io,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn selection_child() {
    let Some(case) = std::env::var_os("PVISOR_TEST_KERNEL_SELECTION") else {
        return;
    };
    match case.to_str().unwrap() {
        "invalid" => {
            assert_eq!(
                VmBuilder::new(1, 256).err().unwrap().kind(),
                io::ErrorKind::InvalidInput
            );
            assert!(VmPlatform::embedded_kernel().is_none());
            assert_eq!(
                VmPlatform::resolve_firmware_path(None).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
        "default" => {
            VmBuilder::new(2, 128).unwrap();
            assert_eq!(
                VmPlatform::embedded_kernel().is_some(),
                cfg!(pvisor_embedded_kernel),
            );
        }
        "rust" => {
            if !cfg!(any(
                all(
                    target_os = "linux",
                    any(target_arch = "x86_64", target_arch = "aarch64")
                ),
                all(target_os = "macos", target_arch = "aarch64")
            )) || cfg!(any(feature = "efi", feature = "tee", feature = "aws-nitro"))
            {
                assert_eq!(
                    VmBuilder::new(1, 256).err().unwrap().kind(),
                    io::ErrorKind::Unsupported
                );
                return;
            }
            let image = VmPlatform::embedded_kernel();
            assert_eq!(image.is_some(), cfg!(pvisor_embedded_native_kernel));
            if let Some(image) = &image {
                let (guest, entry) = if cfg!(target_arch = "aarch64") {
                    (0x80200000, 0x80200000)
                } else {
                    (0x08000000, 0x08001200)
                };
                assert_eq!(image.guest_address, guest);
                assert_eq!(image.entry_address, entry);
                assert!(!image.bytes.is_empty());
                assert!(std::sync::Arc::ptr_eq(
                    &image.bytes,
                    &VmPlatform::embedded_kernel().unwrap().bytes,
                ));
            }
            if cfg!(target_arch = "aarch64") {
                assert_eq!(
                    VmBuilder::new(2, 256).err().unwrap().kind(),
                    io::ErrorKind::Unsupported
                );
            } else {
                VmBuilder::new(2, 256).unwrap();
            }
            assert_eq!(
                VmBuilder::new(3, 256).err().unwrap().kind(),
                io::ErrorKind::Unsupported
            );
            assert_eq!(
                VmBuilder::new(1, 128).err().unwrap().kind(),
                io::ErrorKind::Unsupported
            );
            let mut vm = VmBuilder::new(1, 256).unwrap();
            assert_eq!(
                vm.snapshot_profile().unwrap_err().kind(),
                io::ErrorKind::Unsupported
            );
            assert_eq!(
                vm.guest_idle_handoff().unwrap_err().kind(),
                io::ErrorKind::Unsupported
            );
            assert_eq!(
                vm.task_stdio(-1, -1, -1).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            vm.task_stdio(-1, 1, 2).unwrap();
            assert_eq!(
                vm.task_stdio(-1, 1, 2).unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            );
            let root = tempfile::tempdir().unwrap();
            let file = root.path().join("kernel.bin");
            std::fs::write(&file, b"not a kernel").unwrap();
            assert_eq!(
                vm.set_firmware_path(file).unwrap_err().kind(),
                io::ErrorKind::Unsupported,
            );
            assert_eq!(
                VmPlatform::resolve_firmware_path(Some(root.path()))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::Unsupported,
            );
            if image.is_none() {
                assert_eq!(
                    vm.run(|_| panic!("missing embedded kernel reached VM creation"))
                        .unwrap_err()
                        .kind(),
                    io::ErrorKind::Unsupported,
                );
            }
        }
        _ => panic!("unknown child test case"),
    }
}

fn child(case: &str, selector: Option<&str>) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "selection_child", "--nocapture"])
        .env("PVISOR_TEST_KERNEL_SELECTION", case)
        .env_remove("PVISOR_GUEST_KERNEL")
        .env_remove("PVISOR_NATIVE_KERNEL_BUNDLE")
        .env("PVISOR_EXPERIMENTAL_KERNEL", "rust-native")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    if let Some(selector) = selector {
        command.env("PVISOR_GUEST_KERNEL", selector);
    }
    command.env(
        "PVISOR_NATIVE_KERNEL_BUNDLE",
        "/must-not-be-read-at-runtime",
    );
    command.env(
        "PVISOR_BUILD_NATIVE_KERNEL_BUNDLE",
        "/not-a-runtime-selector",
    );
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "selector {selector:?}: {status}");
            return;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("kernel selection child timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[rstest::rstest]
#[case::unset("default", None)]
#[case::linux("default", Some("linux"))]
#[case::rust_native("rust", Some("rust-native"))]
#[case::empty("invalid", Some(""))]
#[case::old_value("invalid", Some("rust"))]
fn explicit_selection_contract(#[case] case: &str, #[case] selector: Option<&str>) {
    child(case, selector);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    #[test]
    fn unknown_selectors_never_construct_a_vm(selector in "[a-zA-Z0-9_-]{1,24}") {
        prop_assume!(selector != "linux" && selector != "rust-native");
        child("invalid", Some(&selector));
    }
}
