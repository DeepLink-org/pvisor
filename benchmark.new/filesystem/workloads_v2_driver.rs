//! B-FS-ENG: one V2 case per fresh SDK VM; identical firmware in both arms.
use pvisor_vm::api::{OverlayConfig, PermissionSemantics, VmBuilder, VmConfiguration, VmRuntime};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 8 {
        return Err(
            "expected ROOTFS FIXTURE_LAYER FIRMWARE PAYLOAD STAGE CASE ROUNDS RAM_MIB".into(),
        );
    }
    let stage = PathBuf::from(&args[4]);
    let case = args[5].to_str().ok_or("case must be UTF-8")?;
    let rounds = args[6].to_str().ok_or("rounds must be UTF-8")?;
    let ram: u32 = args[7].to_str().ok_or("RAM must be UTF-8")?.parse()?;
    let mut vm = VmBuilder::new(2, ram)?;
    let firmware = PathBuf::from(&args[2]).canonicalize()?;
    eprintln!(
        "WORKLOADS_V2_FIRMWARE {}",
        serde_json::to_string(&firmware)?
    );
    vm.set_firmware_path(firmware)?;
    vm.overlay(
        "/dev/root",
        OverlayConfig {
            lower_dirs: vec![PathBuf::from(&args[1]), PathBuf::from(&args[0])],
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
        "/.workloads-v2-payload",
        std::fs::read(&args[3])?,
        0o500,
        true,
    )?;
    vm.virtual_file("/dev/root", "/.pvisor-guest.json", serde_json::to_vec(&serde_json::json!({
        "argv": ["/.workloads-v2-payload", case, "/fs-v2-work", "/fs-v2-fixture", rounds, ram.to_string()],
        "env": {"PATH": "/usr/bin:/bin", "HOME": "/tmp", "LC_ALL": "C", "GIT_CONFIG_NOSYSTEM": "1"},
        "cwd": "/"
    }))?, 0o400, true)?;
    vm.run(|_| Ok(()))?;
    Ok(())
}
