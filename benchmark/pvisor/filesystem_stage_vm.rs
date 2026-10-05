//! Diagnostic driver only: CLI VM jobs always stage their workspace.
use pvisor_vm::api::{OverlayConfig, PermissionSemantics, VmBuilder, VmConfiguration, VmRuntime};
use std::path::{Path, PathBuf};
fn overlay(lower: &Path, stage: &Path) -> OverlayConfig {
    for name in ["upper", "work", "preimages"] {
        std::fs::create_dir_all(stage.join(name)).unwrap();
    }
    OverlayConfig {
        lower_dirs: vec![lower.to_owned()],
        upper_dir: stage.join("upper"),
        work_dir: Some(stage.join("work")),
        preimage_dir: Some(stage.join("preimages")),
        apply_target: None,
        baseline_lower: None,
        baseline_content_index: None,
        excluded_paths: vec![],
        access_policy: Default::default(),
        semantics: PermissionSemantics::LinuxComplete,
    }
}
fn main() -> std::io::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 6, "mode rootfs workspace stage launch-json");
    let mode = &args[1];
    assert!(["overlay", "workspace-direct", "passthrough"].contains(&mode.as_str()));
    let (rootfs, workspace, stage) = (
        Path::new(&args[2]),
        Path::new(&args[3]),
        PathBuf::from(&args[4]),
    );
    let mut vm = VmBuilder::new(2, 4096)?;
    if mode == "passthrough" {
        vm.filesystem("/dev/root", rootfs, 0)?;
    } else {
        vm.overlay("/dev/root", overlay(rootfs, &stage.join("root")), 0)?;
    }
    vm.virtual_file(
        "/dev/root",
        "/.pvisor-guest.json",
        std::fs::read(&args[5])?,
        0o400,
        true,
    )?;
    if mode == "overlay" {
        vm.overlay("pvisor-workspace", overlay(workspace, &stage), 0)?;
    } else {
        vm.filesystem("pvisor-workspace", workspace, 0)?;
    }
    vm.run(|_| Ok(()))
}
