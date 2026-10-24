//! B-FS-ENG immutable-lower reopen/read cache mechanism subject.
use pvisor_overlay_core::LayerMutability;
use pvisor_vm::api::{OverlayConfig, PermissionSemantics, VmBuilder, VmConfiguration, VmRuntime};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 7 {
        return Err("expected ROOTFS FIXTURE FIRMWARE PAYLOAD PLAN STAGE ROUNDS".into());
    }
    let stage = PathBuf::from(&args[5]);
    let rounds = args[6].to_str().ok_or("rounds must be UTF-8")?;
    let mut vm = VmBuilder::new(2, 512)?;
    vm.set_firmware_path(PathBuf::from(&args[2]))?;
    vm.overlay(
        "/dev/root",
        OverlayConfig {
            lower_dirs: vec![PathBuf::from(&args[1]), PathBuf::from(&args[0])],
            // Only our private, fully verified fixture has this lifetime promise.
            lower_mutability: vec![LayerMutability::Immutable, LayerMutability::Mutable],
            upper_dir: stage.join("upper"),
            work_dir: Some(stage.join("work")),
            preimage_dir: None,
            apply_target: None,
            baseline_lower: None,
            baseline_content_index: None,
            excluded_paths: vec![],
            access_policy: Default::default(),
            semantics: PermissionSemantics::LinuxComplete,
        },
        0,
    )?;
    vm.virtual_file(
        "/dev/root",
        "/.read-cache-payload",
        std::fs::read(&args[3])?,
        0o500,
        true,
    )?;
    vm.virtual_file(
        "/dev/root",
        "/.read-cache-plan",
        std::fs::read(&args[4])?,
        0o400,
        true,
    )?;
    let profile = std::env::var("READ_CACHE_PROFILE").unwrap_or_else(|_| "0".into());
    vm.virtual_file(
        "/dev/root",
        "/.pvisor-guest.json",
        serde_json::to_vec(&serde_json::json!({
            "argv": ["/.read-cache-payload", "/.read-cache-plan", "/cache-input/files", rounds],
            "env": {"READ_CACHE_PROFILE": profile}, "cwd": "/"
        }))?,
        0o400,
        true,
    )?;
    vm.run(|_| Ok(()))?;
    Ok(())
}
