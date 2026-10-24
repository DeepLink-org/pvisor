//! A separately built, public-API VM subject for B-FS-ENG.
//! The parent owns the stage: normal guest shutdown exits this process.
use pvisor_vm::api::{OverlayConfig, PermissionSemantics, VmBuilder, VmConfiguration, VmRuntime};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 5 {
        return Err("expected ROOTFS FIRMWARE IMMUTABLE_DIRECTORY STAGE MODE".into());
    }
    let mode = args[4].to_str().ok_or("non UTF-8 mode")?;
    if !matches!(mode, "off" | "on") {
        return Err("mode must be off or on".into());
    }
    let stage = PathBuf::from(&args[3]);
    let mut vm = VmBuilder::new(2, 512)?;
    vm.set_firmware_path(PathBuf::from(&args[1]))?;
    vm.overlay(
        "/dev/root",
        OverlayConfig {
            lower_dirs: vec![PathBuf::from(&args[0])],
            lower_mutability: vec![],
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
    vm.immutable_filesystem("pvisor-dax", &PathBuf::from(&args[2]), 128 << 20)?;
    // The common static workload is a virtual file in rootfs, so its own
    // executable pages use the same ordinary data path in both conditions.
    vm.virtual_file(
        "/dev/root",
        "/.dax-workload",
        std::fs::read(PathBuf::from(&args[2]).join("workload"))?,
        0o500,
        true,
    )?;
    vm.virtual_file(
        "/dev/root",
        "/.pvisor-guest.json",
        serde_json::to_vec(&serde_json::json!({
            "argv": ["/.dax-workload", mode], "env": {}, "cwd": "/"
        }))?,
        0o400,
        true,
    )?;
    vm.run(|_| Ok(()))?;
    Ok(())
}
