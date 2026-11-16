//! Real syscalls establish whether the host VFS forwards a same-inode rename.
#![cfg(unix)]

use pvisor_overlayfs::api::{
    FilesystemMetrics, FsMetrics, OverlayConfiguration, OverlayFs, OverlayMountConfig,
    OverlayMounting, OverlaySessionControl,
};
use std::{fs, os::unix::fs::MetadataExt, path::PathBuf};

#[test]
#[ignore = "requires a working host FUSE mount (macFUSE FSKit or Linux /dev/fuse)"]
fn same_inode_rename_then_unlink_preserves_the_surviving_cached_inode() {
    for copied in [false, true] {
        let root = tempfile::Builder::new()
            .prefix("pvisor-h03-")
            .tempdir()
            .unwrap();
        let lower = root.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("one"), b"original").unwrap();
        fs::hard_link(lower.join("one"), lower.join("two")).unwrap();
        let native = root.path().join("native");
        fs::create_dir(&native).unwrap();
        fs::write(
            native.join("one"),
            if copied { b"changed!" } else { b"original" },
        )
        .unwrap();
        fs::hard_link(native.join("one"), native.join("two")).unwrap();
        let expected_inode = fs::metadata(native.join("one")).unwrap().ino();
        fs::rename(native.join("one"), native.join("two")).unwrap();
        fs::remove_file(native.join("two")).unwrap();
        assert_eq!(
            fs::metadata(native.join("one")).unwrap().ino(),
            expected_inode
        );
        let expected_bytes = fs::read(native.join("one")).unwrap();
        let mount = if cfg!(target_os = "macos") {
            PathBuf::from("/Volumes").join(root.path().file_name().unwrap())
        } else {
            root.path().join("merged")
        };
        let metrics = FsMetrics::default();
        let mut config =
            OverlayMountConfig::new(vec![lower.clone()], root.path().join("upper"), None, mount);
        config.observation = Some(metrics.clone());
        let session = OverlayFs::mount(config).expect("real host FUSE mount");
        let mount = session.mountpoint();
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> std::io::Result<()> {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while !OverlayFs::is_mountpoint(mount) {
                    if std::time::Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "mount never became active",
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                let original = fs::metadata(mount.join("one"))?.ino();
                assert_eq!(fs::metadata(mount.join("two"))?.ino(), original);
                if copied {
                    fs::write(mount.join("one"), b"changed!")?;
                }
                fs::rename(mount.join("one"), mount.join("two"))?;
                // Re-looking up the source here would repair the lost adapter alias.
                fs::remove_file(mount.join("two"))?;
                assert_eq!(fs::read(mount.join("one"))?, expected_bytes);
                assert_eq!(fs::metadata(mount.join("one"))?.ino(), original);
                Ok(())
            }));
        let observed = metrics.snapshot();
        let rename_hits = observed
            .paths
            .get("one")
            .and_then(|operations| operations.get("rename_from"))
            .map_or(0, |counter| counter.hits);
        eprintln!("copied={copied} rename_callback_hits={rename_hits} result={result:?}");
        session.unmount().expect("explicit unmount after the trace");
        if cfg!(target_os = "macos") {
            assert_eq!(rename_hits, 1, "FSKit must exercise the rename callback");
        }
        assert_eq!(fs::read(lower.join("one")).unwrap(), b"original");
        assert_eq!(fs::read(lower.join("two")).unwrap(), b"original");
        result.unwrap().unwrap();
    }
}
