//! Public-API guest writeback regression; the parent owns and verifies the stage.
use pvisor_overlay_core::LayerMutability;
use pvisor_vm::api::{OverlayConfig, PermissionSemantics, VmBuilder, VmConfiguration, VmRuntime};
use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

fn expected_multifd_redirty() -> Vec<u8> {
    // Four independent opens write disjoint 1-KiB slots of the same inode for
    // three passes. The first 256 bytes stay zero, and slots share/cross pages.
    let mut expected = vec![0; 256 + 2 * 1024 * 1024];
    for (index, bytes) in expected[256..]
        .as_chunks_mut::<1024>()
        .0
        .iter_mut()
        .enumerate()
    {
        let worker = index % 4;
        let slot = index / 4;
        for (offset, byte) in bytes.iter_mut().enumerate() {
            *byte = (worker as u8).wrapping_mul(0x35)
                ^ 2u8.wrapping_mul(0x43)
                ^ (slot as u8).wrapping_mul(0x1d)
                ^ (offset as u8).wrapping_mul(7);
        }
    }
    expected
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--runner") {
        if args.len() != 6 {
            return Err("invalid runner arguments".into());
        }
        let directory = PathBuf::from(&args[1]);
        let mut vm = VmBuilder::new(2, 512)?;
        vm.set_firmware_path(PathBuf::from(&args[3]))?;
        let overlay = OverlayConfig {
            lower_dirs: vec![PathBuf::from(&args[2])],
            lower_mutability: vec![LayerMutability::Immutable],
            upper_dir: directory.join("upper"),
            work_dir: Some(directory.join("work")),
            preimage_dir: None,
            apply_target: None,
            baseline_lower: None,
            baseline_content_index: None,
            excluded_paths: vec![],
            access_policy: Default::default(),
            semantics: PermissionSemantics::LinuxComplete,
        };
        vm.overlay("/dev/root", overlay.clone(), 0)?;
        vm.overlay(
            "writeback-extra",
            OverlayConfig {
                lower_dirs: vec![directory.join("empty")],
                upper_dir: directory.join("extra-upper"),
                work_dir: Some(directory.join("extra-work")),
                ..overlay
            },
            0,
        )?;
        vm.filesystem("writeback-control", &directory.join("control"), 0)?;
        vm.virtual_file(
            "/dev/root",
            "/writeback-probe",
            std::fs::read(&args[4])?,
            0o755,
            false,
        )?;
        let mode = args[5].to_str().ok_or("invalid probe mode")?;
        let config = serde_json::json!({"argv":["/writeback-probe", mode],
            "env":{"PVISOR_TEST_WRITEBACK_BUDGET":std::env::var("PVISOR_VM_FS_WRITEBACK").unwrap_or_default()}, "cwd":"/"});
        vm.virtual_file(
            "/dev/root",
            "/.pvisor-guest.json",
            serde_json::to_vec(&config)?,
            0o400,
            true,
        )?;
        if mode == "error" {
            // Fail only backing writes, after fixture construction and OPEN.
            unsafe {
                libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
                let limit = libc::rlimit {
                    rlim_cur: 65536,
                    rlim_max: 65536,
                };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
            }
        }
        vm.run(|_| Ok(()))?;
        return Err("VM runner returned without exiting".into());
    }
    if !(3..=4).contains(&args.len()) {
        return Err("expected ROOTFS FIRMWARE GUEST_BINARY [full|error]".into());
    }
    let mode = args
        .get(3)
        .map(|arg| arg.to_str().ok_or("invalid mode"))
        .transpose()?
        .unwrap_or("full");
    if !matches!(mode, "full" | "error") {
        return Err("invalid mode".into());
    }
    let root = tempfile::tempdir()?;
    for name in [
        "upper",
        "work",
        "control",
        "empty",
        "extra-upper",
        "extra-work",
    ] {
        std::fs::create_dir(root.path().join(name))?;
    }
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--runner")
        .arg(root.path())
        .args(&args[..3])
        .arg(mode)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut timer_ready = None;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if root.path().join("upper/probe/timer-ready").exists() {
            let started = *timer_ready.get_or_insert_with(Instant::now);
            if std::fs::read(root.path().join("upper/probe/timer"))
                .ok()
                .as_deref()
                == Some(b"timer")
            {
                std::fs::write(root.path().join("control/timer-ack"), b"ok")?;
            } else if started.elapsed() > Duration::from_secs(2) {
                child.kill()?;
                child.wait()?;
                return Err("timer did not write an idle, closed file back to the host".into());
            }
        }
        if Instant::now() > deadline {
            child.kill()?;
            child.wait()?;
            return Err("guest writeback timed out".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    if mode == "error" {
        assert!(
            root.path().join("upper/probe/error-armed").is_dir(),
            "guest did not acknowledge the buffered write before failure"
        );
        assert_eq!(
            std::fs::metadata(root.path().join("extra-upper/failure"))?.len(),
            65536
        );
        if status.code() != Some(125) {
            return Err(format!("writeback error was not reported: {status}").into());
        }
        println!("GUEST_WRITEBACK_ERROR_PASS");
        return Ok(());
    }
    if !status.success() {
        return Err(format!("guest failed: {status}").into());
    }
    if timer_ready.is_none() {
        return Err("timer phase did not run".into());
    }
    assert_eq!(
        std::fs::read(root.path().join("extra-upper/closed-before-unmount"))?,
        b"drained"
    );
    assert_eq!(
        std::fs::read(root.path().join("upper/probe/closed-victim"))?,
        b"replacement"
    );
    for index in 0..8 {
        assert!(!root
            .path()
            .join(format!("upper/probe/closed-unlinked-{index}"))
            .exists());
    }
    for index in 0..256 {
        assert_eq!(
            std::fs::read(root.path().join(format!("upper/probe/file-{index:04}")))?,
            vec![index as u8; 65536]
        );
    }
    assert_eq!(
        std::fs::read(root.path().join("upper/probe/pressure"))?,
        vec![0x5a; 128 << 20]
    );
    assert_eq!(
        std::fs::read(root.path().join("upper/probe/final"))?,
        b"final"
    );
    assert_eq!(
        std::fs::read(root.path().join("upper/probe/renamed"))?,
        b"renamed"
    );
    assert!(!root.path().join("upper/probe/deleted").exists());
    assert_eq!(
        std::fs::read(root.path().join("upper/probe/overwritten"))?,
        vec![255; 4096]
    );
    assert_eq!(
        std::fs::read(root.path().join("upper/probe/synchronous"))?,
        vec![0x73; 8192]
    );
    assert_eq!(
        std::fs::read(root.path().join("upper/probe/mapped"))?,
        vec![0x6b; 8 << 20]
    );
    assert_eq!(
        std::fs::read(root.path().join("upper/probe/multifd-redirty"))?,
        expected_multifd_redirty()
    );
    println!("GUEST_WRITEBACK_PASS");
    Ok(())
}
