//! B-FS-ENG guest-writeback firmware A/B, separate from ordinary CLI timing.
//! Same host/payload; only explicit CURVES_FIRMWARE selects the old baseline.
//! The observer owns the stage and verifies its bytes after normal VM shutdown.
use pvisor_vm::api::{OverlayConfig, PermissionSemantics, VmBuilder, VmConfiguration, VmRuntime};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 6 {
        return Err("expected ROOTFS FIRMWARE PAYLOAD PLAN STAGE ROUNDS".into());
    }
    let stage = PathBuf::from(&args[4]);
    let rounds = args[5].to_str().ok_or("non UTF-8 rounds")?;
    let mut vm = VmBuilder::new(2, 512)?;
    let selected = std::env::var_os("CURVES_FIRMWARE").filter(|value| !value.is_empty());
    let selection = if selected.is_some() {
        "condition_env"
    } else {
        "argv"
    };
    let firmware = selected
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&args[1]));
    let firmware = firmware.canonicalize()?;
    eprintln!(
        "CURVES_FIRMWARE_SELECTED {}",
        serde_json::to_string(&serde_json::json!({
            "path": firmware, "selection": selection
        }))?
    );
    vm.set_firmware_path(firmware)?;
    vm.overlay(
        "/dev/root",
        OverlayConfig {
            lower_dirs: vec![PathBuf::from(&args[0])],
            // This is a caller-owned prepared rootfs, never a live workspace.
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
    vm.virtual_file(
        "/dev/root",
        "/.curve-payload",
        std::fs::read(&args[2])?,
        0o500,
        true,
    )?;
    vm.virtual_file(
        "/dev/root",
        "/.curve-plan",
        std::fs::read(&args[3])?,
        0o400,
        true,
    )?;
    let profile = std::env::var("CURVES_PROFILE").unwrap_or_else(|_| "0".into());
    vm.virtual_file(
        "/dev/root",
        "/.pvisor-guest.json",
        serde_json::to_vec(&serde_json::json!({
            "argv": ["/.curve-payload", "/.curve-plan", "/curve-work", rounds],
            "env": {"CURVES_PROFILE": profile}, "cwd": "/"
        }))?,
        0o400,
        true,
    )?;
    vm.run(|_| Ok(()))?;
    Ok(())
}
