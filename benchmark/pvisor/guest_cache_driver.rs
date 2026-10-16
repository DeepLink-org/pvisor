//! SDK subject for B-FS-ENG guest-cache experiments; no production API changes.
use pvisor_overlay_core::LayerMutability;
use pvisor_vm::api::{OverlayConfig, PermissionSemantics, VmBuilder, VmConfiguration, VmRuntime};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 6 {
        return Err("expected ROOTFS FIRMWARE GUEST FIXTURE STAGE MODE".into());
    }
    let stage = PathBuf::from(&args[4]);
    let overlay = |name: &str, lower: PathBuf, stability| -> std::io::Result<OverlayConfig> {
        let upper = stage.join(format!("{name}-upper"));
        let work = stage.join(format!("{name}-work"));
        std::fs::create_dir(&upper)?;
        std::fs::create_dir(&work)?;
        Ok(OverlayConfig {
            lower_dirs: vec![lower],
            lower_mutability: vec![stability],
            upper_dir: upper,
            work_dir: Some(work),
            preimage_dir: None,
            apply_target: None,
            baseline_lower: None,
            baseline_content_index: None,
            excluded_paths: vec![],
            access_policy: Default::default(),
            semantics: PermissionSemantics::LinuxComplete,
        })
    };
    let mut vm = VmBuilder::new(2, 4096)?;
    vm.set_firmware_path(PathBuf::from(&args[1]))?;
    // Rootfs is deliberately ordinary; only the subject share declares stability.
    vm.overlay(
        "/dev/root",
        overlay("root", PathBuf::from(&args[0]), LayerMutability::Mutable)?,
        0,
    )?;
    vm.overlay(
        "cache-input",
        overlay("input", PathBuf::from(&args[3]), LayerMutability::Immutable)?,
        0,
    )?;
    vm.virtual_file(
        "/dev/root",
        "/cache-probe",
        std::fs::read(&args[2])?,
        0o755,
        false,
    )?;
    let mode = args[5].to_str().ok_or("mode must be UTF-8")?;
    let config = serde_json::json!({"argv":["/cache-probe","/input",mode],"env":{},"cwd":"/"});
    vm.virtual_file(
        "/dev/root",
        "/.pvisor-guest.json",
        serde_json::to_vec(&config)?,
        0o400,
        true,
    )?;
    vm.run(|_| Ok(()))?;
    Err("VM returned without guest completion".into())
}
