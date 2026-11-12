#![cfg(unix)]
//! Architectural guards and ownership checks; no hypervisor is started.
use pvisor_vm::api::{self, RestoreState, SnapshotState, VmBuilder, VmConfiguration};

#[test]
fn branch_offload_rejects_unauthorized_machine_state_without_poisoning_builder() {
    let state = serde_json::from_value(serde_json::json!({
        "version":1,"cpus":[],"devices":[],"ram":[]
    }))
    .unwrap();
    let mut builder = VmBuilder::new(1, 128).unwrap();
    let error = builder
        .machine_restore_with_branch_offload(
            api::MachineRestore {
                state,
                ram_file: std::sync::Arc::new(tempfile::tempfile().unwrap()),
                ram_extents: vec![],
            },
            tempfile::tempfile().unwrap(),
        )
        .unwrap_err();
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    }
    builder.ram_backing(tempfile::tempfile().unwrap()).unwrap();
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn branch_listener_rebinding_requires_one_exact_binding_and_is_atomic() {
    let value = serde_json::json!({
        "version":1,"cpus":[],"ram":[],"pio_devices":[],"kvm":null,
        "devices":[{"base":4096,"len":4096,"device":{"kind":"Virtio","state":{
            "version":1,"device_type":19,"avail_features":0,"acked_features":0,
            "features_select":0,"acked_features_select":0,"queue_select":0,"device_status":0,
            "config_generation":0,"shm_region_select":0,"interrupt_status":0,"irq_line":null,"queues":null,
            "device":{"queues":null,"state":{"kind":"Vsock","state":{
                "cid":3,"host_port_map":null,"unix_ipc_port_map":{"7000":["/tmp/source.sock",true]},"tsi_flags":0
            }}}
        }}}]
    });
    // Backend state is intentionally host-specific. This fixture checks the
    // KVM snapshot's public atomic-rebinding contract without starting a VM.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        let mut state: api::MachineSnapshot = serde_json::from_value(value).unwrap();
        let before = serde_json::to_value(&state).unwrap();
        assert!(state
            .rebind_vsock_listener(
                7001,
                "/tmp/source.sock".as_ref(),
                "/tmp/child.sock".as_ref()
            )
            .is_err());
        assert_eq!(before, serde_json::to_value(&state).unwrap());
        assert!(state
            .rebind_vsock_listener(7000, "/tmp/wrong.sock".as_ref(), "/tmp/child.sock".as_ref())
            .is_err());
        assert_eq!(before, serde_json::to_value(&state).unwrap());
        state
            .rebind_vsock_listener(
                7000,
                "/tmp/source.sock".as_ref(),
                "/tmp/child.sock".as_ref(),
            )
            .unwrap();
        let after = serde_json::to_string(&state).unwrap();
        assert!(after.contains("/tmp/child.sock"));
        assert!(!after.contains("/tmp/source.sock"));
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    let _ = value;
}

#[test]
fn managed_filesystem_validation_is_atomic_and_excludes_snapshots() {
    let root = tempfile::tempdir().unwrap();
    let mut builder = VmBuilder::new(1, 128).unwrap();
    if cfg!(all(
        target_os = "linux",
        not(any(feature = "tee", feature = "aws-nitro"))
    )) {
        assert!(builder
            .managed_filesystem("data", std::path::Path::new("relative"), false)
            .is_err());
        assert!(builder.managed_filesystem("", root.path(), false).is_err());
        builder
            .managed_filesystem("data", root.path(), false)
            .unwrap();
        assert!(builder
            .managed_filesystem("data", root.path(), true)
            .is_err());
        assert_eq!(
            builder.snapshot_profile().unwrap_err().kind(),
            std::io::ErrorKind::Unsupported
        );
        let mut reverse = VmBuilder::new(1, 128).unwrap();
        reverse.snapshot_profile().unwrap();
        assert_eq!(
            reverse
                .managed_filesystem("data", root.path(), false)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::Unsupported
        );
    } else {
        assert_eq!(
            builder
                .managed_filesystem("data", root.path(), false)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::Unsupported
        );
    }
}

#[test]
fn shared_kernel_baseline_is_explicit_atomic_and_excludes_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let mut builder = VmBuilder::new(1, 128).unwrap();
    if cfg!(all(target_os = "macos", target_arch = "aarch64"))
        && !cfg!(any(feature = "tee", feature = "aws-nitro", feature = "efi"))
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(builder.shared_kernel_base("relative".into()).is_err());
        builder
            .shared_kernel_base(directory.path().to_path_buf())
            .unwrap();
        assert!(builder
            .shared_kernel_base(directory.path().join("missing"))
            .is_err());
        assert_eq!(
            builder.snapshot_profile().unwrap_err().kind(),
            std::io::ErrorKind::Unsupported
        );
        let mut reverse = VmBuilder::new(1, 128).unwrap();
        reverse.snapshot_profile().unwrap();
        assert_eq!(
            reverse
                .shared_kernel_base(directory.path().to_path_buf())
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::Unsupported
        );
    } else {
        assert_eq!(
            builder
                .shared_kernel_base(directory.path().to_path_buf())
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::Unsupported
        );
    }
}

#[test]
fn read_only_filesystem_configuration_is_atomic_without_dax() {
    let root = tempfile::tempdir().unwrap();
    let mut builder = VmBuilder::new(1, 128).unwrap();
    #[cfg(not(any(feature = "tee", feature = "aws-nitro")))]
    {
        assert_eq!(
            builder
                .read_only_filesystem("data", std::path::Path::new("relative"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        builder.read_only_filesystem("data", root.path()).unwrap();
        assert!(builder.read_only_filesystem("data", root.path()).is_err());
        // An ordinary read-only share does not acquire DAX's snapshot exclusion.
        builder.snapshot_profile().unwrap();
    }
    #[cfg(any(feature = "tee", feature = "aws-nitro"))]
    assert_eq!(
        builder
            .read_only_filesystem("data", root.path())
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Unsupported
    );
}

#[test]
fn immutable_dax_configuration_validates_before_attachment_and_rejects_snapshots() {
    let root = tempfile::tempdir().unwrap();
    let mut builder = VmBuilder::new(1, 128).unwrap();
    #[cfg(all(
        target_os = "linux",
        target_arch = "x86_64",
        not(any(feature = "tee", feature = "aws-nitro"))
    ))]
    {
        for size in [0, 4096, (2 << 20) + 4096, (1 << 30) + (2 << 20)] {
            assert_eq!(
                builder
                    .immutable_filesystem("data", root.path(), size)
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::InvalidInput
            );
        }
        assert_eq!(
            builder
                .immutable_filesystem("data", std::path::Path::new("relative"), 2 << 20)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        builder
            .immutable_filesystem("data", root.path(), 2 << 20)
            .unwrap();
        assert!(builder
            .immutable_filesystem("data", root.path(), 2 << 20)
            .is_err());
        assert_eq!(
            builder.snapshot_profile().unwrap_err().kind(),
            std::io::ErrorKind::Unsupported
        );
        builder.filesystem("ordinary", root.path(), 0).unwrap();
        let mut snapshots = VmBuilder::new(1, 128).unwrap();
        snapshots.snapshot_profile().unwrap();
        assert_eq!(
            snapshots
                .immutable_filesystem("data", root.path(), 2 << 20)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::Unsupported
        );
        snapshots.filesystem("data", root.path(), 0).unwrap();
    }
    #[cfg(not(all(
        target_os = "linux",
        target_arch = "x86_64",
        not(any(feature = "tee", feature = "aws-nitro"))
    )))]
    assert_eq!(
        builder
            .immutable_filesystem("data", root.path(), 2 << 20)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Unsupported
    );
}

#[test]
fn invalid_attachment_does_not_poison_configuration() {
    let root = tempfile::tempdir().unwrap();
    let mut vm = VmBuilder::new(1, 128).unwrap();
    vm.filesystem("/dev/root", root.path(), 0).unwrap();
    assert!(vm.disable_implicit_init().is_err());
    assert!(vm.filesystem("/dev/root", root.path(), 0).is_err());
    assert!(vm
        .virtual_file("/dev/root", "/../escape", vec![1], 0o400, false)
        .is_err());
    vm.virtual_file("/dev/root", "/config", vec![1, 2, 3], 0o400, false)
        .unwrap();
    assert!(vm
        .virtual_file("/dev/root", "/config", vec![4], 0o400, false)
        .is_err());
    vm.virtual_file("/dev/root", "/another", vec![5], 0o400, false)
        .unwrap();
}

#[test]
fn snapshot_ram_shape_is_portable_and_mapping_is_private() {
    use std::sync::Arc;
    use vm_memory::{Bytes, GuestAddress};
    let file = tempfile::tempfile().unwrap();
    // macOS host pages are 16 KiB, Linux usually uses 4 KiB.
    file.set_len(65536).unwrap();
    let state = serde_json::from_value(serde_json::json!({
        "version":1,"cpus":[],"devices":[],
        "ram":[{"base":0,"len":65536,"file_offset":0}]
    }))
    .unwrap();
    let restore = api::MachineRestore {
        ram_extents: Vec::new(),
        state,
        ram_file: Arc::new(file),
    };
    let first = restore.map_ram(&[(GuestAddress(0), 65536)]).unwrap();
    let second = restore.map_ram(&[(GuestAddress(0), 65536)]).unwrap();
    first.write_slice(b"private", GuestAddress(0)).unwrap();
    let mut bytes = [1; 7];
    second.read_slice(&mut bytes, GuestAddress(0)).unwrap();
    assert_eq!(bytes, [0; 7]);
    assert!(restore.map_ram(&[(GuestAddress(4096), 65536)]).is_err());
    assert!(restore.validate(1).is_err()); // RAM mapping is not full-machine validation.
}

#[test]
fn invalid_overlay_path_does_not_poison_the_builder() {
    use std::os::unix::ffi::OsStringExt;
    let root = tempfile::tempdir().unwrap();
    let lower = root.path().join("lower");
    let upper = root.path().join("upper");
    std::fs::create_dir_all(&lower).unwrap();
    std::fs::create_dir_all(&upper).unwrap();
    let mut builder = VmBuilder::new(1, 64).unwrap();
    let mut overlay = api::OverlayConfig {
        lower_mutability: Vec::new(),
        lower_dirs: vec![lower],
        upper_dir: upper,
        work_dir: None,
        preimage_dir: None,
        apply_target: None,
        baseline_lower: None,
        baseline_content_index: None,
        excluded_paths: vec![],
        access_policy: Default::default(),
        semantics: api::PermissionSemantics::LinuxComplete,
    };
    assert!(overlay.lower_mutability.is_empty());
    let valid = overlay.clone();
    let mut invalid_mutability = valid.clone();
    invalid_mutability.lower_mutability = vec![pvisor_overlay_core::LayerMutability::Immutable; 2];
    assert_eq!(
        builder
            .overlay("workspace", invalid_mutability, 0)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    overlay
        .excluded_paths
        .push(std::ffi::OsString::from_vec(vec![0xff]).into());
    assert_eq!(
        builder.overlay("workspace", overlay, 0).unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
    let mut invalid_index = valid.clone();
    invalid_index.baseline_content_index = Some(api::BaselineContentIndex {
        root: std::ffi::OsString::from_vec(vec![0xff]).into(),
        file: root.path().join("index"),
        sha256: "00".repeat(32),
    });
    assert_eq!(
        builder
            .overlay("workspace", invalid_index, 0)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    builder.overlay("workspace", valid, 0).unwrap();
}

#[test]
fn guest_idle_handoff_accepts_at_most_two_cpus_and_excludes_snapshot_profile() {
    assert!(VmBuilder::new(3, 128)
        .unwrap()
        .guest_idle_handoff()
        .is_err());
    let mut vm = VmBuilder::new(1, 128).unwrap();
    if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        not(any(feature = "tee", feature = "efi", feature = "aws-nitro"))
    )) {
        VmBuilder::new(2, 128)
            .unwrap()
            .guest_idle_handoff()
            .unwrap();
        vm.guest_idle_handoff().unwrap();
        assert!(vm.snapshot_profile().is_err());
        let mut vm = VmBuilder::new(1, 128).unwrap();
        vm.snapshot_profile().unwrap();
        assert!(vm.guest_idle_handoff().is_err());
    } else {
        assert_eq!(
            vm.guest_idle_handoff().unwrap_err().kind(),
            std::io::ErrorKind::Unsupported
        );
    }
}

#[test]
fn guest_idle_offload_has_an_explicit_optional_reclaim_contract() {
    use pvisor_vm::api::{GuestIdleControl, RamReclaim, VmmHandle};
    let _method: fn(&VmmHandle, u64) -> Result<Option<RamReclaim>, String> =
        <VmmHandle as GuestIdleControl>::offload_guest_idle;
}

#[test]
fn idle_host_policy_validates_budget_and_platform_without_starting_vm() {
    use std::time::Duration;
    let mut vm = VmBuilder::new(1, 128).unwrap();
    for value in [
        Duration::ZERO,
        Duration::from_millis(2),
        Duration::from_secs(86401),
    ] {
        assert_eq!(
            vm.guest_idle_handoff_with_policy(true, value)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
    let result = vm.guest_idle_handoff_with_policy(true, Duration::from_secs(1));
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        result.unwrap();
        VmBuilder::new(2, 128)
            .unwrap()
            .guest_idle_handoff_with_policy(true, Duration::from_secs(1))
            .unwrap();
        let mut smp = VmBuilder::new(3, 128).unwrap();
        assert!(smp
            .guest_idle_handoff_with_policy(true, Duration::from_secs(1))
            .is_err());
    } else {
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
    }
}

#[test]
fn guest_idle_offer_ordinal_is_optional_observation_on_the_wire() {
    let mut wire = serde_json::json!({
        "epoch": 7, "sequence": 9, "enabled": true, "offered": false,
        "claimed": false, "remaining_ns": 0, "wake_reason": "guest_timer",
        "pending_host_reads": 0, "offload_minimum_ns": 1000000000u64,
        "offload_deadline_skips": 0
    });
    let old: api::GuestIdleStatus = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(old.offer_sequence, None);
    wire["offer_sequence"] = serde_json::json!(42);
    let recorded: api::GuestIdleStatus = serde_json::from_value(wire).unwrap();
    assert_eq!(recorded.offer_sequence, Some(42));
    assert!(!recorded.offered);
    assert_eq!(recorded.wake_reason.as_deref(), Some("guest_timer"));
    assert_eq!(
        serde_json::to_value(recorded).unwrap()["offer_sequence"],
        42
    );
}

#[test]
fn protected_workspace_seed_api_validates_tags_without_mutating_state() {
    let mut wire = serde_json::json!({"version":1,"cpus":[],"devices":[],"ram":[]});
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        wire["kvm"] = serde_json::Value::Null;
        wire["pio_devices"] = serde_json::json!([]);
    }
    let mut state: api::MachineSnapshot = serde_json::from_value(wire).unwrap();
    let before = serde_json::to_value(&state).unwrap();
    assert!(state.prepare_protected_workspace_seed("").is_err());
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    if cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    )) {
        assert_eq!(state.prepare_protected_workspace_seed("absent").unwrap(), 0);
    } else {
        assert!(state.prepare_protected_workspace_seed("absent").is_err());
    }
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
}

#[test]
fn compact_ram_zero_extents_preserve_topology_and_private_writes() {
    use std::{os::unix::fs::FileExt, sync::Arc};
    use vm_memory::{Bytes, GuestAddress, GuestMemory};
    let file = tempfile::NamedTempFile::new().unwrap();
    let block = 65536u64;
    file.as_file().set_len(block * 2).unwrap();
    file.as_file().write_all_at(b"first", 0).unwrap();
    file.as_file().write_all_at(b"last!", block).unwrap();
    let readonly = std::fs::File::open(file.path()).unwrap();
    let state = serde_json::from_value(serde_json::json!({
        "version":1,"cpus":[],"devices":[],
        "ram":[{"base":0,"len":block*4,"file_offset":0}]
    }))
    .unwrap();
    let mut restore = api::MachineRestore {
        state,
        ram_file: Arc::new(readonly),
        ram_extents: vec![
            api::RamRestoreExtent {
                offset: 0,
                length: block,
                backing_offset: Some(0),
            },
            api::RamRestoreExtent {
                offset: block,
                length: block * 2,
                backing_offset: None,
            },
            api::RamRestoreExtent {
                offset: block * 3,
                length: block,
                backing_offset: Some(block),
            },
        ],
    };
    let ranges = [(GuestAddress(0), (block * 4) as usize)];
    let a = restore.map_ram(&ranges).unwrap();
    let b = restore.map_ram(&ranges).unwrap();
    assert_eq!(a.num_regions(), 1);
    for (address, expected) in [(0, *b"first"), (block, [0; 5]), (block * 3, *b"last!")] {
        let mut got = [7; 5];
        a.read_slice(&mut got, GuestAddress(address)).unwrap();
        assert_eq!(got, expected);
        a.write_slice(b"write", GuestAddress(address)).unwrap();
        b.read_slice(&mut got, GuestAddress(address)).unwrap();
        assert_eq!(got, expected);
    }
    let mut original = [0; 5];
    restore.ram_file.read_exact_at(&mut original, 0).unwrap();
    assert_eq!(&original, b"first");
    // Geometry errors are rejected before exposing any partially mapped region.
    for (offset, length, backing) in [
        (block + 1, block * 2, None),
        (block, 0, None),
        (block, block * 2, Some(0)),
        (block, u64::MAX, None),
    ] {
        restore.ram_extents[1] = api::RamRestoreExtent {
            offset,
            length,
            backing_offset: backing,
        };
        assert!(restore.map_ram(&ranges).is_err());
    }
}

#[test]
fn shared_backing_is_an_explicit_cold_ram_profile() {
    let defaults = api::ColdRamOptions::default();
    assert!(!defaults.shared_backing);
    let managed = api::ColdRamOptions {
        shared_backing: true,
        page_granular: true,
        share_resident: true,
        ..defaults
    };
    assert!(managed.shared_backing && managed.page_granular && managed.share_resident);
}
