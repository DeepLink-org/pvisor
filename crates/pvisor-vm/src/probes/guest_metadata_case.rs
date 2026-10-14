//! Real guest directory-cache and deferred-open contracts through the public API.
use pvisor_overlay_core::LayerMutability;
use pvisor_vm::api::{OverlayConfig, PermissionSemantics, VmBuilder, VmConfiguration, VmRuntime};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(3..=4).contains(&args.len()) {
        return Err("expected ROOTFS FIRMWARE GUEST_BINARY [full|baseline|warm]".into());
    }
    let mode = args
        .get(3)
        .map(|s| s.to_str().ok_or("mode must be UTF-8"))
        .transpose()?
        .unwrap_or("full");
    if !matches!(mode, "full" | "baseline" | "warm") {
        return Err("unsupported probe mode".into());
    }
    let root = tempfile::tempdir()?;
    let mutable = root.path().join("mutable-lower");
    let stable = root.path().join("stable-lower");
    let fixture = root.path().join("root-lower");
    std::fs::create_dir(&mutable)?;
    std::fs::create_dir(&stable)?;
    std::fs::create_dir(&fixture)?;
    std::fs::write(fixture.join("copyup-source"), b"original")?;
    for i in 0..300 {
        let directory = stable.join(format!("dir-{i:04}"));
        std::fs::create_dir(&directory)?;
        std::fs::write(directory.join("file"), b"x")?;
    }
    let overlay = |name: &str, lowers: Vec<PathBuf>, stability| -> std::io::Result<OverlayConfig> {
        let upper = root.path().join(format!("{name}-upper"));
        let work = root.path().join(format!("{name}-work"));
        std::fs::create_dir(&upper)?;
        std::fs::create_dir(&work)?;
        Ok(OverlayConfig {
            lower_dirs: lowers,
            lower_mutability: stability,
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
    let root_overlay = overlay(
        "root",
        vec![fixture, PathBuf::from(&args[0])],
        vec![LayerMutability::Immutable; 2],
    )?;
    std::fs::write(root_overlay.upper_dir.join("metadata-only"), b"metadata")?;
    let mut vm = VmBuilder::new(2, 512)?;
    vm.set_firmware_path(PathBuf::from(&args[1]))?;
    vm.overlay("/dev/root", root_overlay, 0)?;
    vm.overlay(
        "metadata-mutable",
        overlay(
            "mutable",
            vec![mutable.clone()],
            vec![LayerMutability::Mutable],
        )?,
        0,
    )?;
    vm.overlay(
        "metadata-stable",
        overlay("stable", vec![stable], vec![LayerMutability::Immutable])?,
        0,
    )?;
    vm.virtual_file(
        "/dev/root",
        "/metadata-probe",
        std::fs::read(&args[2])?,
        0o755,
        false,
    )?;
    let config = serde_json::json!({"argv": ["/metadata-probe", mode], "env": {}, "cwd": "/"});
    vm.virtual_file(
        "/dev/root",
        "/.pvisor-guest.json",
        serde_json::to_vec(&config)?,
        0o400,
        true,
    )?;
    let ready = root.path().join("root-upper/metadata-ready");
    let watcher = (mode == "full").then(|| {
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !ready.exists() {
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "guest did not become ready",
                    ));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            std::fs::write(mutable.join("created-after-import"), b"visible")
        })
    });
    vm.run(|_| Ok(()))?;
    if let Some(watcher) = watcher {
        watcher.join().map_err(|_| "watcher panicked")??;
    }
    Ok(())
}
