//! Native Windows checks that do not require an enabled hypervisor.
#![cfg(windows)]
use pvisor_vm::api::{
    Architecture, Hypervisor, RuntimeSupport, VmBuilder, VmConfiguration, VmPlatform,
};
use std::{io::ErrorKind, path::Path};

#[test]
fn whp_capabilities_are_explicit_without_opening_a_partition() {
    let capabilities = VmPlatform::capabilities();
    assert_eq!(capabilities.architecture, Architecture::X86_64);
    assert_eq!(capabilities.hypervisor, Hypervisor::Whp);
    assert!(!capabilities.machine_snapshot);
    assert!(!capabilities.cold_ram_faults);
    assert_eq!(VmPlatform::firmware_name(), "libkrunfw.dll");
}

#[test]
fn invalid_sizing_and_kernel_geometry_leave_configuration_usable() {
    for (cpus, memory) in [(0, 128), (65, 128), (1, 0), (1, 65537)] {
        assert!(VmBuilder::new(cpus, memory).is_err());
    }
    let mut vm = VmBuilder::new(2, 256).unwrap();
    for (bytes, load, entry) in [(&[][..], 0, 0), (&[0][..], 1, 1), (&[0][..], 4096, 0)] {
        assert_eq!(
            vm.embedded_kernel(bytes, load, entry).unwrap_err().kind(),
            ErrorKind::InvalidInput
        );
    }
    // The builder takes an owned copy, without loading WHP or running these bytes.
    vm.embedded_kernel(&vec![0; 4096], 0x100000, 0x100000)
        .unwrap();
    assert!(vm.embedded_kernel(&[], 0, 0).is_err());
    vm.embedded_kernel(&vec![1; 4096], 0x100000, 0x100000)
        .unwrap();
}

#[test]
fn filesystem_and_virtual_file_validation_is_atomic() {
    let root = tempfile::tempdir().unwrap();
    let mut vm = VmBuilder::new(1, 128).unwrap();
    assert!(vm.filesystem("root", Path::new("relative"), 0).is_err());
    assert_eq!(
        vm.filesystem("root", root.path(), 4096).unwrap_err().kind(),
        ErrorKind::Unsupported
    );
    vm.filesystem("/dev/root", root.path(), 0).unwrap();
    assert!(vm.disable_implicit_init().is_err());
    assert!(vm.filesystem("/dev/root", root.path(), 0).is_err());
    for name in ["", "/", ".", "..", "../escape", "a\\b", "x\0y"] {
        assert!(vm
            .virtual_file("/dev/root", name, vec![1], 0o400, false)
            .is_err());
    }
    assert!(vm
        .virtual_file("missing", "/config", vec![1], 0o400, false)
        .is_err());
    vm.virtual_file("/dev/root", "/config", vec![1, 2], 0o400, true)
        .unwrap();
    assert!(vm
        .virtual_file("/dev/root", "/config", vec![3], 0o400, false)
        .is_err());
    vm.read_only_filesystem("data", root.path()).unwrap();
}

#[test]
fn unsupported_operations_do_not_poison_later_attachments() {
    let root = tempfile::tempdir().unwrap();
    let mut vm = VmBuilder::new(1, 128).unwrap();
    assert_eq!(
        vm.snapshot_profile().unwrap_err().kind(),
        ErrorKind::Unsupported
    );
    assert_eq!(
        vm.guest_idle_handoff().unwrap_err().kind(),
        ErrorKind::Unsupported
    );
    assert_eq!(
        vm.ram_backing(tempfile::tempfile().unwrap())
            .unwrap_err()
            .kind(),
        ErrorKind::Unsupported
    );
    assert_eq!(
        vm.task_stdio(-1, 1, 2).unwrap_err().kind(),
        ErrorKind::Unsupported
    );
    vm.disable_implicit_init().unwrap();
    vm.filesystem("/dev/root", root.path(), 0).unwrap();
    vm.virtual_file("/dev/root", "/init.krun", vec![1], 0o755, false)
        .unwrap();
}

#[test]
fn explicit_firmware_directory_never_falls_back() {
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(
        VmPlatform::resolve_firmware_path(Some(directory.path()))
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    );
    let firmware = directory.path().join("libkrunfw.dll");
    std::fs::write(&firmware, b"not loaded during configuration").unwrap();
    assert_eq!(
        VmPlatform::resolve_firmware_path(Some(directory.path())).unwrap(),
        firmware.canonicalize().unwrap()
    );
    let mut vm = VmBuilder::new(1, 128).unwrap();
    vm.set_firmware_path(firmware).unwrap();
    assert!(vm
        .set_firmware_path(directory.path().to_path_buf())
        .is_err());
}
