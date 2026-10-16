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
        return Err("expected ROOTFS FIRMWARE GUEST_BINARY [MODE]".into());
    }
    let mode = args
        .get(3)
        .map(|s| s.to_str().ok_or("mode must be UTF-8"))
        .transpose()?
        .unwrap_or("full");
    if !matches!(
        mode,
        "full"
            | "baseline"
            | "warm"
            | "bench"
            | "atime"
            | "review"
            | "review-time"
            | "review-setxattr"
            | "review-removexattr"
            | "review-link"
            | "review-poll"
            | "review-pages"
            | "review-attrs"
            | "review-flush"
            | "review-data"
            | "review-negative"
    ) {
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
    for kind in ["time", "setxattr", "removexattr", "link"] {
        std::fs::write(fixture.join(format!("lifetime-{kind}")), b"original")?;
    }
    if matches!(mode, "review" | "review-removexattr") {
        let path = std::ffi::CString::new(
            fixture
                .join("lifetime-removexattr")
                .as_os_str()
                .as_encoded_bytes(),
        )?;
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::setxattr(
                path.as_ptr(),
                c"user.lifetime".as_ptr(),
                b"v".as_ptr().cast(),
                1,
                0,
            )
        };
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::setxattr(
                path.as_ptr(),
                c"user.lifetime".as_ptr(),
                b"v".as_ptr().cast(),
                1,
                0,
                0,
            )
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err("xattr lifetime probe requires Linux or macOS".into());
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    for i in 0..300 {
        let directory = stable.join(format!("dir-{i:04}"));
        std::fs::create_dir(&directory)?;
        std::fs::write(directory.join("file"), b"x")?;
    }
    for name in ["wide-lookup", "dense-lookup"] {
        let directory = stable.join(name);
        std::fs::create_dir(&directory)?;
        for i in 0u64..256 {
            std::fs::write(directory.join(format!("f{i:04}")), i.to_le_bytes())?;
        }
    }
    if mode == "atime" {
        for mask in ["size-mtime", "atime", "basic"] {
            for (directory, prefix) in [
                (&fixture, "atime-lower"),
                (&stable, "atime"),
                (&mutable, "atime"),
            ] {
                std::fs::write(directory.join(format!("{prefix}-{mask}")), b"original")?;
            }
        }
    }
    if mode == "review-poll" {
        std::fs::write(mutable.join("poll-target"), b"original")?;
    }
    if mode == "review-pages" {
        let bytes: Vec<u8> = (0..16384).map(|i| (i % 251) as u8).collect();
        for bound in [false, true] {
            for size in [4096, 8192] {
                std::fs::write(fixture.join(format!("pages-{bound}-{size}")), &bytes)?;
            }
        }
    }
    if mode == "review-flush" {
        for directory in [&fixture, &stable, &mutable] {
            std::fs::write(directory.join("flush-read"), b"original")?;
        }
        std::fs::write(fixture.join("flush-copyup"), vec![b'a'; 16384])?;
    }
    if mode == "review-data" {
        let pattern = |size: usize, seed: usize| -> Vec<u8> {
            (0..size)
                .map(|index| ((index * 17 + index / 4096 * 29 + seed) % 251) as u8)
                .collect()
        };
        for bound in [false, true] {
            for mutation in ["full", "partial", "zero", "shrink", "extend"] {
                std::fs::write(
                    fixture.join(format!("data-{mutation}-{bound}")),
                    pattern(16384, 3),
                )?;
            }
        }
        for name in [
            "reopen",
            "shared",
            "private",
            "rename-victim",
            "unlink",
            "transfer",
            "hardlink",
            "link-transition",
            "copy-range",
            "direct",
            "map-files",
        ] {
            std::fs::write(stable.join(format!("data-{name}")), pattern(16384, 3))?;
        }
        for name in ["mmap-shrink", "mmap-extend"] {
            std::fs::write(stable.join(format!("data-{name}")), pattern(8192 + 17, 3))?;
        }
        std::fs::write(stable.join("data-rename-source"), pattern(16384, 97))?;
        std::fs::hard_link(
            stable.join("data-hardlink"),
            stable.join("data-hardlink-alias"),
        )?;
        std::fs::write(mutable.join("data-fallback-mutable"), pattern(16384, 3))?;
    }
    if mode == "review-attrs" {
        use std::os::unix::fs::PermissionsExt;
        for kind in [
            "chmod",
            "time",
            "truncate",
            "write",
            "copyup",
            "link",
            "link-opath",
            "rename-overwrite",
            "unlink-recreate",
            "alias",
        ] {
            let path = fixture.join(format!("attrs-{kind}"));
            std::fs::write(&path, b"original")?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
        }
        std::fs::hard_link(
            fixture.join("attrs-alias"),
            fixture.join("attrs-alias-other"),
        )?;
        std::fs::write(fixture.join("attrs-rename-source"), b"replacement")?;
        std::fs::set_permissions(
            fixture.join("attrs-rename-source"),
            std::fs::Permissions::from_mode(0o600),
        )?;
    }
    if mode == "review-negative" {
        for lower in [&fixture, &stable, &mutable] {
            for name in [
                "repeat",
                "expiry",
                "create",
                "mkdir",
                "symlink",
                "link",
                "rename",
                "exchange",
                "failed",
                "parent",
                "permissions",
                "concurrent",
                "external",
            ] {
                let directory = lower.join(format!("negative-{name}"));
                std::fs::create_dir(&directory)?;
                std::fs::write(directory.join("existing"), b"original")?;
            }
        }
        std::fs::create_dir(stable.join("negative-hidden"))?;
        std::fs::write(stable.join("negative-hidden/existing"), b"host-only")?;
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
            excluded_paths: if mode == "review-negative" && name == "stable" {
                vec![
                    PathBuf::from("negative-hidden"),
                    PathBuf::from("negative-excluded-ancestor/secret"),
                ]
            } else {
                vec![]
            },
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
    if mode == "review-flush" {
        std::fs::write(root_overlay.upper_dir.join("flush-upper"), b"upper")?;
    }
    if mode == "review-data" {
        let bytes: Vec<u8> = (0..16384)
            .map(|index| ((index * 17 + index / 4096 * 29 + 3) % 251) as u8)
            .collect();
        std::fs::write(root_overlay.upper_dir.join("data-fallback-upper"), bytes)?;
    }
    if mode == "atime" {
        for mask in ["size-mtime", "atime", "basic"] {
            std::fs::write(
                root_overlay.upper_dir.join(format!("atime-upper-{mask}")),
                b"original",
            )?;
        }
    }
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
    let poll_failure = mode == "review-poll";
    let negative_external = mode == "review-negative";
    let watcher = (mode == "full" || poll_failure || negative_external).then(|| {
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
            if poll_failure {
                std::fs::remove_file(mutable.join("poll-target"))?;
                return std::fs::write(mutable.join("poll-removed"), b"removed");
            }
            if negative_external {
                let temporary = mutable.join("negative-external/pending");
                std::fs::write(&temporary, b"external-visible")?;
                return std::fs::rename(temporary, mutable.join("negative-external/arrived"));
            }
            // Publish complete bytes atomically; creation alone exposes an empty file.
            let temporary = mutable.join("pending-create");
            std::fs::write(&temporary, b"visible")?;
            std::fs::rename(temporary, mutable.join("created-after-import"))
        })
    });
    vm.run(|_| Ok(()))?;
    if let Some(watcher) = watcher {
        watcher.join().map_err(|_| "watcher panicked")??;
    }
    Ok(())
}
