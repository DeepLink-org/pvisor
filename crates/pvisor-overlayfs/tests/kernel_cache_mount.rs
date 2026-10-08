//! Real Linux HOST FUSE checks. Explicitly ignored by portable test runs;
//! run with nextest --run-ignored ignored-only and a usable /dev/fuse.
#[cfg(target_os = "linux")]
mod linux {
    use pvisor_overlay_core::LayerMutability;
    use pvisor_overlayfs::api::{
        KernelCacheConfig, KernelCachePolicy, OverlayConfiguration, OverlayFs, OverlayMountConfig,
        OverlayMounting, OverlaySessionControl, OwnedViewContract, ReadObservationSemantics,
    };
    use std::ffi::CString;
    use std::fs::{self, File, FileTimes, OpenOptions};
    use std::io::Read;

    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    fn config(root: &Path, policy: KernelCachePolicy) -> OverlayMountConfig {
        let mut config = OverlayMountConfig::new(
            vec![root.join("lower")],
            root.join("upper"),
            Some(root.join("work")),
            root.join("merged"),
        );
        config.read_only = true;
        config.lower_mutability = vec![LayerMutability::Immutable];
        config.kernel_cache = KernelCacheConfig {
            policy,
            ttl: Duration::from_secs(60),
            owned_view: Some(OwnedViewContract {
                exclusive_upper_and_work: true,
                fixed_metadata_and_aliases: true,
            }),
            read_observation: ReadObservationSemantics::StableView,
        };
        config
    }

    // Arrange stable atime for this short test on relatime backing filesystems.
    // Record and verify it too: a strictatime backing mount must fail this test,
    // not silently provide invalid evidence for the ownership contract.
    fn stabilize_atime(path: &Path) {
        let future = SystemTime::now() + Duration::from_secs(7 * 24 * 60 * 60);
        File::open(path)
            .unwrap()
            .set_times(FileTimes::new().set_accessed(future))
            .unwrap();
    }

    fn enter_noatime_fixture(test: &str) -> bool {
        use std::process::Command;
        // libtest is multithreaded, so establish the user/mount namespace before
        // starting a child test binary, not via unshare inside its test thread.
        // No environment variable enables cache policy or bypasses admission.
        let mapping = fs::read_to_string("/proc/self/uid_map").unwrap();
        let fields: Vec<_> = mapping.split_whitespace().collect();
        let private_user = unsafe { libc::geteuid() } == 0
            && fields.len() == 3
            && fields[0] == "0"
            && fields[2] == "1";
        if !private_user {
            let status = Command::new("unshare")
                .args([
                    "--user",
                    "--map-root-user",
                    "--mount",
                    "--propagation",
                    "private",
                ])
                .arg(std::env::current_exe().unwrap())
                .args(["--exact", test, "--ignored", "--nocapture"])
                .status()
                .expect("unshare user/mount namespace for stable backing");
            assert!(status.success(), "noatime namespace test failed: {status}");
            return false;
        }
        true
    }

    struct WritableFixture(tempfile::TempDir);
    impl WritableFixture {
        fn path(&self) -> &Path {
            self.0.path()
        }
    }
    impl Drop for WritableFixture {
        fn drop(&mut self) {
            let path = CString::new(self.path().as_os_str().as_bytes()).unwrap();
            if unsafe { libc::umount2(path.as_ptr(), libc::MNT_DETACH) } != 0 {
                eprintln!(
                    "fixture tmpfs unmount failed: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
    }
    fn writable_fixture() -> WritableFixture {
        let root = WritableFixture(tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap());
        // Inherited mounts may have locked atime flags in a user namespace.
        // A fresh tmpfs in our private namespace establishes true noatime for
        // all newly created upper files without touching any host mount options.
        assert!(
            std::process::Command::new("mount")
                .args([
                    "-t",
                    "tmpfs",
                    "-o",
                    "noatime,nosuid,nodev,mode=0700,size=16m",
                    "tmpfs"
                ])
                .arg(root.path())
                .status()
                .unwrap()
                .success()
        );
        for name in ["lower", "upper", "work"] {
            fs::create_dir(root.path().join(name)).unwrap();
        }
        let lower = root.path().join("lower");
        fs::write(lower.join("a"), b"original lower content").unwrap();
        fs::hard_link(lower.join("a"), lower.join("alias")).unwrap();
        fs::create_dir(lower.join("dir")).unwrap();
        fs::write(lower.join("dir/child"), b"child").unwrap();
        fs::hard_link(lower.join("dir/child"), lower.join("outside-alias")).unwrap();
        fs::write(lower.join("trunc"), b"truncate pages").unwrap();
        fs::hard_link(lower.join("trunc"), lower.join("trunc-alias")).unwrap();
        fs::write(lower.join("victim"), b"replacement victim").unwrap();
        fs::write(lower.join("late-a"), b"abcd").unwrap();
        fs::hard_link(lower.join("late-a"), lower.join("late-z")).unwrap();
        fs::write(lower.join("append-flags"), b"abcd").unwrap();
        fs::create_dir(lower.join("empty")).unwrap();
        for (directory, bytes) in [("swap-one", b"one"), ("swap-two", b"two")] {
            fs::create_dir(lower.join(directory)).unwrap();
            fs::write(lower.join(directory).join("child"), bytes).unwrap();
        }

        root
    }

    #[test]
    #[ignore = "requires Linux FUSE, writable fusectl abort, unshare and private noatime tmpfs"]
    fn host_kernel_cache_writable_warm_negatives_copyup_aliases_rename_and_metadata() {
        if !enter_noatime_fixture(
            "linux::host_kernel_cache_writable_warm_negatives_copyup_aliases_rename_and_metadata",
        ) {
            return;
        }
        for policy in [KernelCachePolicy::Uncached, KernelCachePolicy::Metadata] {
            let root = writable_fixture();
            let mut cfg = config(root.path(), policy);
            cfg.read_only = false;
            let session = OverlayFs::mount(cfg.clone())
                .expect("writable HOST FUSE admission, including abort fd");
            let mount = session.mountpoint();
            let original = fs::read(root.path().join("lower/a")).unwrap();
            let lower_atime = fs::metadata(root.path().join("lower/a"))
                .unwrap()
                .accessed()
                .unwrap();
            // late-z is intentionally never looked up, opened or enumerated
            // before copy-up and growth of late-a.
            {
                let appender = OpenOptions::new()
                    .append(true)
                    .open(mount.join("late-a"))
                    .unwrap();
                appender.write_at(b"efgh", 0).unwrap();
                drop(appender); // late alias must not rely on a live source handle
                assert_eq!(fs::metadata(mount.join("late-z")).unwrap().len(), 8);
                assert_eq!(fs::read(mount.join("late-z")).unwrap(), b"abcdefgh");
                assert_eq!(
                    fs::metadata(mount.join("late-a")).unwrap().ino(),
                    fs::metadata(mount.join("late-z")).unwrap().ino()
                );
                assert_eq!(fs::read(root.path().join("lower/late-z")).unwrap(), b"abcd");
            }
            {
                let file = OpenOptions::new()
                    .append(true)
                    .open(mount.join("append-flags"))
                    .unwrap();
                let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
                assert!(flags >= 0);
                assert_eq!(
                    unsafe {
                        libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags & !libc::O_APPEND)
                    },
                    0
                );
                file.write_at(b"X", 0).unwrap();
                assert_eq!(fs::read(mount.join("append-flags")).unwrap(), b"Xbcd");
                assert_eq!(
                    unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_APPEND) },
                    0
                );
                file.write_at(b"Y", 0).unwrap();
                assert_eq!(fs::read(mount.join("append-flags")).unwrap(), b"XbcdY");
            }
            {
                fs::write(mount.join("append-copy-source"), b"XYZ").unwrap();
                fs::write(mount.join("append-copy-target"), b"abcdefgh").unwrap();
                fs::hard_link(
                    mount.join("append-copy-target"),
                    mount.join("append-copy-alias"),
                )
                .unwrap();
                let output = OpenOptions::new()
                    .append(true)
                    .open(mount.join("append-copy-target"))
                    .unwrap();
                let input = File::open(mount.join("append-copy-source")).unwrap();
                let cached = File::open(mount.join("append-copy-alias")).unwrap();
                let mut bytes = [0; 8];
                assert_eq!(cached.read_at(&mut bytes, 0).unwrap(), 8);
                assert_eq!(&bytes, b"abcdefgh");
                let flags = unsafe { libc::fcntl(output.as_raw_fd(), libc::F_GETFL) };
                assert!(flags >= 0);
                // Linux must still reject a genuinely append-mounted target.
                let mut src = 0;
                let mut dst = 2;
                assert_eq!(
                    unsafe {
                        libc::copy_file_range(
                            input.as_raw_fd(),
                            &mut src,
                            output.as_raw_fd(),
                            &mut dst,
                            3,
                            0,
                        )
                    },
                    -1
                );
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EBADF)
                );
                assert_eq!(
                    unsafe {
                        libc::fcntl(output.as_raw_fd(), libc::F_SETFL, flags & !libc::O_APPEND)
                    },
                    0
                );
                // No WRITE callback between F_SETFL and this positional copy.
                assert_eq!(
                    unsafe {
                        libc::copy_file_range(
                            input.as_raw_fd(),
                            &mut src,
                            output.as_raw_fd(),
                            &mut dst,
                            3,
                            0,
                        )
                    },
                    3
                );
                assert_eq!((src, dst), (3, 5));
                assert_eq!(cached.read_at(&mut bytes, 0).unwrap(), 8);
                assert_eq!(&bytes, b"abXYZfgh");
                assert_eq!(
                    fs::metadata(mount.join("append-copy-alias")).unwrap().len(),
                    8
                );
                assert_eq!(
                    fs::read(root.path().join("upper/append-copy-target")).unwrap(),
                    b"abXYZfgh"
                );
                assert_eq!(
                    unsafe {
                        libc::fcntl(output.as_raw_fd(), libc::F_SETFL, flags | libc::O_APPEND)
                    },
                    0
                );
                output.write_at(b"!", 0).unwrap();
                assert_eq!(
                    fs::read(mount.join("append-copy-alias")).unwrap(),
                    b"abXYZfgh!"
                );
            }
            // Warm positives and negatives before every cooperative mutation.
            for _ in 0..3 {
                assert_eq!(fs::read(mount.join("a")).unwrap(), original);
                assert_eq!(fs::read(mount.join("alias")).unwrap(), original);
                assert!(fs::metadata(mount.join("new")).is_err());
                assert!(fs::metadata(mount.join("linked")).is_err());
            }
            assert_eq!(fs::read(mount.join("trunc")).unwrap(), b"truncate pages");
            assert_eq!(
                fs::read(mount.join("trunc-alias")).unwrap(),
                b"truncate pages"
            );
            {
                let held_trunc = File::open(mount.join("trunc-alias")).unwrap();
                let truncated = OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(mount.join("trunc"))
                    .unwrap();
                assert_eq!(fs::metadata(mount.join("trunc-alias")).unwrap().len(), 0);
                assert_eq!(held_trunc.read_at(&mut [0; 16], 0).unwrap(), 0);
                drop(truncated);
                assert_eq!(
                    fs::read(root.path().join("lower/trunc")).unwrap(),
                    b"truncate pages"
                );
            }
            fs::write(mount.join("new"), b"created after negative").unwrap();
            assert_eq!(
                fs::read(mount.join("new")).unwrap(),
                b"created after negative"
            );
            let held = File::open(mount.join("a")).unwrap();
            let inode = fs::metadata(mount.join("a")).unwrap().ino();
            let alias_inode = fs::metadata(mount.join("alias")).unwrap().ino();
            assert_eq!(inode, alias_inode);
            let writer = OpenOptions::new()
                .write(true)
                .open(mount.join("a"))
                .unwrap();
            let mut warm = vec![0; original.len()];
            assert_eq!(held.read_at(&mut warm, 0).unwrap(), original.len());
            assert_eq!(warm, original); // warm pages after the writer's open
            writer.write_at(b"CHANGED", 0).unwrap();
            assert_eq!(fs::metadata(mount.join("alias")).unwrap().ino(), inode);
            let mut bytes = vec![0; original.len()];
            held.read_at(&mut bytes, 0).unwrap();
            assert_eq!(
                &bytes[..7],
                b"CHANGED",
                "existing lower fd follows original copy-up rebinding semantics"
            );
            writer.set_len(3).unwrap();
            assert_eq!(held.read_at(&mut bytes, 0).unwrap(), 3);
            assert_eq!(&bytes[..3], b"CHA");
            assert_eq!(fs::metadata(mount.join("alias")).unwrap().len(), 3);
            assert_eq!(fs::read(mount.join("alias")).unwrap(), b"CHA");
            fs::set_permissions(mount.join("a"), fs::Permissions::from_mode(0o640)).unwrap();
            assert_eq!(
                fs::metadata(mount.join("alias")).unwrap().mode() & 0o777,
                0o640
            );
            let atime = fs::metadata(root.path().join("upper/a"))
                .unwrap()
                .accessed()
                .unwrap();
            fs::read(mount.join("a")).unwrap();
            assert_eq!(
                fs::metadata(root.path().join("upper/a"))
                    .unwrap()
                    .accessed()
                    .unwrap(),
                atime,
                "upper reads must not mutate backing atime"
            );
            let file_path = CString::new(mount.join("alias").as_os_str().as_bytes()).unwrap();
            assert_eq!(
                unsafe {
                    libc::setxattr(
                        file_path.as_ptr(),
                        c"user.cache-test".as_ptr(),
                        c"value".as_ptr().cast(),
                        5,
                        0,
                    )
                },
                0
            );
            let mut value = [0_u8; 5];
            assert_eq!(
                unsafe {
                    libc::getxattr(
                        file_path.as_ptr(),
                        c"user.cache-test".as_ptr(),
                        value.as_mut_ptr().cast(),
                        5,
                    )
                },
                5
            );
            assert_eq!(&value, b"value");
            assert_eq!(
                unsafe { libc::removexattr(file_path.as_ptr(), c"user.cache-test".as_ptr()) },
                0
            );
            assert_eq!(
                unsafe {
                    libc::getxattr(
                        file_path.as_ptr(),
                        c"user.cache-test".as_ptr(),
                        value.as_mut_ptr().cast(),
                        5,
                    )
                },
                -1
            );
            let links = fs::metadata(mount.join("alias")).unwrap().nlink();
            fs::hard_link(mount.join("a"), mount.join("linked")).unwrap();
            assert_eq!(fs::metadata(mount.join("linked")).unwrap().ino(), inode);
            assert_eq!(
                fs::metadata(mount.join("alias")).unwrap().nlink(),
                links + 1
            );
            fs::remove_file(mount.join("linked")).unwrap();
            assert!(fs::metadata(mount.join("linked")).is_err());
            assert_eq!(fs::metadata(mount.join("alias")).unwrap().nlink(), links);
            let victim = File::open(mount.join("victim")).unwrap();
            assert_eq!(
                fs::read(mount.join("victim")).unwrap(),
                b"replacement victim"
            );
            fs::rename(mount.join("a"), mount.join("victim")).unwrap();
            assert_eq!(fs::read(mount.join("victim")).unwrap(), b"CHA");
            assert!(fs::metadata(mount.join("a")).is_err());
            assert_eq!(fs::read(mount.join("alias")).unwrap(), b"CHA");
            let mut old = [0_u8; 18];
            victim.read_at(&mut old, 0).unwrap();
            assert_eq!(&old, b"replacement victim");
            let child = fs::metadata(mount.join("dir/child")).unwrap().ino();
            assert_eq!(
                fs::metadata(mount.join("outside-alias")).unwrap().ino(),
                child
            );
            assert_eq!(fs::read(mount.join("outside-alias")).unwrap(), b"child");
            let held_child = File::open(mount.join("dir/child")).unwrap();
            assert!(fs::metadata(mount.join("moved/child")).is_err());
            fs::rename(mount.join("dir"), mount.join("moved")).unwrap();
            assert!(fs::metadata(mount.join("dir/child")).is_err());
            assert_eq!(
                fs::metadata(mount.join("moved/child")).unwrap().ino(),
                child
            );
            assert_eq!(fs::read(mount.join("moved/child")).unwrap(), b"child");
            let mut contents = [0; 5];
            held_child.read_at(&mut contents, 0).unwrap();
            assert_eq!(&contents, b"child");
            {
                let child_writer = OpenOptions::new()
                    .write(true)
                    .open(mount.join("moved/child"))
                    .unwrap();
                child_writer.write_at(b"!", 5).unwrap();
                assert_eq!(fs::metadata(mount.join("outside-alias")).unwrap().len(), 6);
                assert_eq!(fs::read(mount.join("outside-alias")).unwrap(), b"child!");
            }
            fs::create_dir(mount.join("dir")).unwrap();
            assert!(fs::read_dir(mount.join("dir")).unwrap().next().is_none());
            assert!(fs::metadata(mount.join("dir/child")).is_err());
            assert!(
                root.path().join("upper/dir/.wh..wh..opq").exists(),
                "recreated lower directory must be opaque"
            );
            // Removing/recreating a lower directory produces whiteout/opaque
            // backing changes. Only the newly created merged contents may appear.
            fs::remove_file(mount.join("moved/child")).unwrap();
            fs::remove_dir(mount.join("moved")).unwrap();
            fs::create_dir(mount.join("moved")).unwrap();
            assert!(fs::read_dir(mount.join("moved")).unwrap().next().is_none());
            fs::remove_dir(mount.join("empty")).unwrap();
            assert!(fs::metadata(mount.join("empty")).is_err());
            fs::create_dir(mount.join("empty")).unwrap();
            assert!(fs::read_dir(mount.join("empty")).unwrap().next().is_none());
            {
                let first = mount.join("swap-one");
                let second = mount.join("swap-two");
                let first_child = fs::metadata(first.join("child")).unwrap().ino();
                let second_child = fs::metadata(second.join("child")).unwrap().ino();
                let first_held = File::open(first.join("child")).unwrap();
                let second_held = File::open(second.join("child")).unwrap();
                assert_eq!(fs::read(first.join("child")).unwrap(), b"one");
                assert_eq!(fs::read(second.join("child")).unwrap(), b"two");
                let first_path = CString::new(first.as_os_str().as_bytes()).unwrap();
                let second_path = CString::new(second.as_os_str().as_bytes()).unwrap();
                assert_eq!(
                    unsafe {
                        libc::syscall(
                            libc::SYS_renameat2,
                            libc::AT_FDCWD,
                            first_path.as_ptr(),
                            libc::AT_FDCWD,
                            second_path.as_ptr(),
                            libc::RENAME_EXCHANGE,
                        )
                    },
                    0
                );
                assert_eq!(
                    fs::metadata(first.join("child")).unwrap().ino(),
                    second_child
                );
                assert_eq!(
                    fs::metadata(second.join("child")).unwrap().ino(),
                    first_child
                );
                assert_eq!(fs::read(first.join("child")).unwrap(), b"two");
                assert_eq!(fs::read(second.join("child")).unwrap(), b"one");
                let mut bytes = [0; 3];
                first_held.read_at(&mut bytes, 0).unwrap();
                assert_eq!(&bytes, b"one");
                second_held.read_at(&mut bytes, 0).unwrap();
                assert_eq!(&bytes, b"two");
            }
            {
                fs::write(mount.join("copy-in"), b"copied!").unwrap();
                fs::write(mount.join("copy-out"), b"prior").unwrap();
                fs::hard_link(mount.join("copy-out"), mount.join("copy-alias")).unwrap();
                let cached = File::open(mount.join("copy-alias")).unwrap();
                assert_eq!(fs::read(mount.join("copy-alias")).unwrap(), b"prior");
                let input = File::open(mount.join("copy-in")).unwrap();
                let output = OpenOptions::new()
                    .write(true)
                    .open(mount.join("copy-out"))
                    .unwrap();
                let mut warm = [0; 5];
                assert_eq!(cached.read_at(&mut warm, 0).unwrap(), 5);
                assert_eq!(&warm, b"prior");
                let mut src: libc::off64_t = 0;
                let mut dst: libc::off64_t = 0;
                assert_eq!(
                    unsafe {
                        libc::copy_file_range(
                            input.as_raw_fd(),
                            &mut src,
                            output.as_raw_fd(),
                            &mut dst,
                            7,
                            0,
                        )
                    },
                    7
                );
                assert_eq!(fs::metadata(mount.join("copy-alias")).unwrap().len(), 7);
                let mut copied = [0; 7];
                assert_eq!(cached.read_at(&mut copied, 0).unwrap(), 7);
                assert_eq!(&copied, b"copied!");
                assert_eq!(
                    unsafe { libc::posix_fallocate(output.as_raw_fd(), 0, 8192) },
                    0
                );
                assert_eq!(fs::metadata(mount.join("copy-alias")).unwrap().len(), 8192);
                assert_eq!(cached.read_at(&mut copied, 0).unwrap(), 7);
                assert_eq!(&copied, b"copied!");
            }
            // No sleeps or notification fences: all checks follow syscall reply.
            assert_eq!(fs::read(root.path().join("lower/a")).unwrap(), original);
            assert_eq!(
                fs::metadata(root.path().join("lower/a"))
                    .unwrap()
                    .accessed()
                    .unwrap(),
                lower_atime
            );
            assert!(!session.has_exited());
            if policy == KernelCachePolicy::Metadata {
                let mut second = cfg;
                second.mountpoint = root.path().join("second");
                assert!(
                    OverlayFs::mount(second)
                        .unwrap_err()
                        .to_string()
                        .contains("coordination lock unavailable")
                );
            }
            drop((held, writer, victim, held_child));
            session.unmount().unwrap();
        }
    }

    #[test]
    #[ignore = "requires Linux FUSE, writable fusectl abort, unshare and private noatime tmpfs"]
    fn host_kernel_cache_writable_parallel_stress_and_pending_teardown() {
        if !enter_noatime_fixture(
            "linux::host_kernel_cache_writable_parallel_stress_and_pending_teardown",
        ) {
            return;
        }
        let root = writable_fixture();
        let mut cfg = config(root.path(), KernelCachePolicy::Metadata);
        cfg.read_only = false;
        let session =
            OverlayFs::mount(cfg).expect("writable HOST FUSE admission, including abort fd");
        let mount = session.mountpoint().to_path_buf();
        std::thread::scope(|scope| {
            for worker in 0..4 {
                let directory = mount.join(format!("worker-{worker}"));
                fs::create_dir(&directory).unwrap();
                scope.spawn(move || {
                    for iteration in 0..64 {
                        let first = directory.join("first");
                        let alias = directory.join("alias");
                        let moved = directory.join("moved");
                        assert!(fs::metadata(&first).is_err());
                        assert!(fs::metadata(&alias).is_err());
                        assert!(fs::metadata(&moved).is_err());
                        fs::write(&first, iteration.to_string()).unwrap();
                        fs::hard_link(&first, &alias).unwrap();
                        let file = OpenOptions::new().write(true).open(&first).unwrap();
                        file.set_len(1).unwrap();
                        assert_eq!(fs::metadata(&alias).unwrap().len(), 1);
                        fs::set_permissions(&first, fs::Permissions::from_mode(0o600)).unwrap();
                        assert_eq!(fs::metadata(&alias).unwrap().mode() & 0o777, 0o600);
                        fs::rename(&first, &moved).unwrap();
                        assert!(fs::metadata(&first).is_err());
                        assert_eq!(
                            fs::metadata(&moved).unwrap().ino(),
                            fs::metadata(&alias).unwrap().ino()
                        );
                        fs::remove_file(&alias).unwrap();
                        fs::remove_file(&moved).unwrap();
                    }
                });
            }
        });
        assert!(!session.has_exited());
        // No delay/fence before teardown: pending entry notifications must not
        // deadlock unmount or survive as detached workers retaining the channel.
        session.unmount().unwrap();
    }

    #[test]
    #[ignore = "requires real Linux FUSE mount; run explicitly"]
    fn host_kernel_cache_stable_readonly_same_artifact_modes_and_coordination() {
        for policy in [
            KernelCachePolicy::Disabled,
            KernelCachePolicy::Uncached,
            KernelCachePolicy::Metadata,
            KernelCachePolicy::MetadataAndData,
        ] {
            // /tmp may be a strictatime tmpfs. Use the crate's backing mount
            // for a relatime fixture; the final assertion still fails if this
            // filesystem does not actually preserve the promised atime.
            let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
            let lower = root.path().join("lower");
            let upper = root.path().join("upper");
            let work = root.path().join("work");
            for path in [&lower, &upper, &work] {
                fs::create_dir(path).unwrap();
            }
            fs::write(lower.join("a"), b"stable lower pages").unwrap();
            fs::hard_link(lower.join("a"), lower.join("alias")).unwrap();
            fs::write(lower.join("hidden"), b"hidden").unwrap();
            fs::create_dir(lower.join("opaque")).unwrap();
            fs::write(lower.join("opaque/hidden"), b"hidden child").unwrap();
            fs::write(upper.join(".wh.hidden"), b"").unwrap();
            fs::create_dir(upper.join("opaque")).unwrap();
            fs::write(upper.join("opaque/.wh..wh..opq"), b"").unwrap();
            fs::write(upper.join("opaque/visible"), b"upper pages").unwrap();
            let stable_paths = [
                lower.clone(),
                lower.join("a"),
                lower.join("hidden"),
                lower.join("opaque"),
                lower.join("opaque/hidden"),
                upper.clone(),
                upper.join(".wh.hidden"),
                upper.join("opaque"),
                upper.join("opaque/.wh..wh..opq"),
                upper.join("opaque/visible"),
            ];
            for path in &stable_paths {
                stabilize_atime(path);
            }
            let before: Vec<_> = stable_paths
                .iter()
                .map(|p| fs::metadata(p).unwrap().accessed().unwrap())
                .collect();
            let cfg = config(root.path(), policy);
            let session = OverlayFs::mount(cfg.clone()).expect("real HOST FUSE mount");
            let mount = session.mountpoint();
            for _ in 0..3 {
                assert_eq!(fs::read(mount.join("a")).unwrap(), b"stable lower pages");
                assert_eq!(
                    fs::read(mount.join("alias")).unwrap(),
                    b"stable lower pages"
                );
                assert_eq!(
                    fs::metadata(mount.join("a")).unwrap().ino(),
                    fs::metadata(mount.join("alias")).unwrap().ino()
                );
                assert_eq!(
                    fs::read(mount.join("opaque/visible")).unwrap(),
                    b"upper pages"
                );
                for name in ["missing", "hidden", "opaque/hidden"] {
                    assert_eq!(
                        fs::metadata(mount.join(name)).unwrap_err().raw_os_error(),
                        Some(libc::ENOENT)
                    );
                }
                let names: Vec<_> = fs::read_dir(mount)
                    .unwrap()
                    .map(|e| e.unwrap().file_name())
                    .collect();
                assert!(
                    names
                        .iter()
                        .all(|name| !name.to_string_lossy().starts_with(".wh."))
                );
                assert!(!names.contains(&"hidden".into()));
            }
            let mut held = File::open(mount.join("a")).unwrap();
            let mut bytes = Vec::new();
            held.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"stable lower pages");
            for error in [
                OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(mount.join("a"))
                    .unwrap_err(),
                fs::write(mount.join("missing"), b"create").unwrap_err(),
                fs::remove_file(mount.join("a")).unwrap_err(),
                fs::rename(mount.join("a"), mount.join("replacement")).unwrap_err(),
                fs::hard_link(mount.join("a"), mount.join("new-alias")).unwrap_err(),
                fs::set_permissions(mount.join("a"), fs::Permissions::from_mode(0o600))
                    .unwrap_err(),
                fs::rename(mount.join("opaque"), mount.join("renamed")).unwrap_err(),
            ] {
                assert_eq!(error.raw_os_error(), Some(libc::EROFS));
            }
            let file_path = CString::new(mount.join("a").as_os_str().as_bytes()).unwrap();
            let result = unsafe {
                libc::setxattr(
                    file_path.as_ptr(),
                    c"user.cache-test".as_ptr(),
                    c"value".as_ptr().cast(),
                    5,
                    0,
                )
            };
            assert_eq!(result, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EROFS)
            );
            if matches!(
                policy,
                KernelCachePolicy::Metadata | KernelCachePolicy::MetadataAndData
            ) {
                let mut second = cfg.clone();
                second.mountpoint = root.path().join("second");
                let error = OverlayFs::mount(second).unwrap_err();
                assert!(
                    error.to_string().contains("coordination lock unavailable"),
                    "{error}"
                );
            }
            drop(held);
            session.unmount().unwrap();
            let after: Vec<_> = stable_paths
                .iter()
                .map(|p| fs::metadata(p).unwrap().accessed().unwrap())
                .collect();
            assert_eq!(before, after, "backing atime must actually stay stable");
            // Teardown releases the advisory lock so the same upper can be reused.
            let session = OverlayFs::mount(cfg).unwrap();
            assert_eq!(
                fs::read(session.mountpoint().join("a")).unwrap(),
                b"stable lower pages"
            );
            session.unmount().unwrap();
        }
    }
}
