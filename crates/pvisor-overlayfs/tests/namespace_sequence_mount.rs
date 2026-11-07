//! Bounded namespace failure/retry trace against a native filesystem oracle.
//! Explicit mount runs must fail, not skip, when Linux FUSE is unavailable.
#[cfg(target_os = "linux")]
mod linux {
    use pvisor_overlayfs::api::{
        OverlayConfiguration, OverlayFs, OverlayMountConfig, OverlayMounting, OverlaySessionControl,
    };
    use std::collections::BTreeMap;
    use std::fs;
    use std::io;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};

    #[derive(Debug, PartialEq, Eq)]
    enum Entry {
        Directory,
        // The first sorted path identifies each hardlink group without comparing
        // native inode numbers between the two independent filesystems.
        File(Vec<u8>, PathBuf),
    }

    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Entry> {
        fn walk(
            root: &Path,
            dir: &Path,
            entries: &mut BTreeMap<PathBuf, Entry>,
            aliases: &mut BTreeMap<(u64, u64), PathBuf>,
        ) {
            let mut children: Vec<_> = fs::read_dir(root.join(dir))
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            children.sort();
            for name in children {
                let rel = dir.join(name);
                let path = root.join(&rel);
                let metadata = fs::symlink_metadata(&path).unwrap();
                let entry = if metadata.is_dir() {
                    walk(root, &rel, entries, aliases);
                    Entry::Directory
                } else {
                    assert!(metadata.is_file(), "unexpected type: {rel:?}");
                    let first = aliases
                        .entry((metadata.dev(), metadata.ino()))
                        .or_insert_with(|| rel.clone())
                        .clone();
                    Entry::File(fs::read(path).unwrap(), first)
                };
                entries.insert(rel, entry);
            }
        }
        let mut entries = BTreeMap::new();
        walk(root, Path::new(""), &mut entries, &mut BTreeMap::new());
        entries
    }

    #[derive(Clone, Copy, Debug)]
    enum Op {
        Rename(&'static str, &'static str),
        Link(&'static str, &'static str),
        Unlink(&'static str),
        Mkdir(&'static str),
        Rmdir(&'static str),
        Write(&'static str, &'static [u8]),
    }

    impl Op {
        fn run(self, root: &Path) -> io::Result<()> {
            match self {
                Self::Rename(from, to) => fs::rename(root.join(from), root.join(to)),
                Self::Link(from, to) => fs::hard_link(root.join(from), root.join(to)),
                Self::Unlink(path) => fs::remove_file(root.join(path)),
                Self::Mkdir(path) => fs::create_dir(root.join(path)),
                Self::Rmdir(path) => fs::remove_dir(root.join(path)),
                Self::Write(path, bytes) => fs::write(root.join(path), bytes),
            }
        }
    }

    fn populate(root: &Path) {
        fs::create_dir(root).unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/item"), b"original").unwrap();
        fs::create_dir(root.join("dst")).unwrap();
        fs::write(root.join("dst/block"), b"blocker").unwrap();
    }

    #[test]
    #[ignore = "requires a working Linux /dev/fuse and fusermount3"]
    fn namespace_failure_retry_matches_native_trace_007() {
        use Op::*;
        // Fixed trace rather than random fuzzing: trace ID is the reproducer.
        // All failures are ordinary syscall failures, not backing corruption.
        let trace = [
            (Rename("src", "dst"), Some(libc::ENOTEMPTY)),
            (Rename("src", "dst"), Some(libc::ENOTEMPTY)),
            (Link("src/item", "dst/block"), Some(libc::EEXIST)),
            (Link("src/item", "dst/block"), Some(libc::EEXIST)),
            (Rename("src/item", "dst"), Some(libc::EISDIR)),
            (Rename("src", "dst/block"), Some(libc::ENOTDIR)),
            (Link("src/item", "alias"), None),
            (Write("alias", b"alias edit"), None),
            (Rename("src", "dst"), Some(libc::ENOTEMPTY)),
            (Unlink("dst/block"), None),
            (Rename("src", "dst"), None),
            (Write("dst/item", b"renamed edit"), None),
            (Unlink("alias"), None),
            (Mkdir("src"), None),
            (Link("dst/item", "src/link"), None),
            (Unlink("dst/item"), None),
            (Write("src/link", b"surviving alias"), None),
            (Unlink("src/link"), None),
            (Rmdir("dst"), None),
            (Write("dst", b"directory became file"), None),
            (Rename("src", "dst"), Some(libc::ENOTDIR)),
            (Unlink("dst"), None),
            (Rename("src", "dst"), None),
            (Rmdir("dst"), None),
        ];
        assert!(trace.len() <= 64);
        let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let lower = root.path().join("lower");
        let native = root.path().join("native");
        populate(&lower);
        populate(&native);
        let initial_lower = snapshot(&lower);
        let config = OverlayMountConfig::new(
            vec![lower.clone()],
            root.path().join("upper"),
            Some(root.path().join("work")),
            root.path().join("merged"),
        );
        // Default cache policy: no noatime/owned-cache capability assumptions.
        let session = OverlayFs::mount(config).expect("trace-007 real Linux FUSE mount");
        let mount = session.mountpoint();
        assert_eq!(snapshot(mount), snapshot(&native));
        for (step, (op, expected_errno)) in trace.into_iter().enumerate() {
            eprintln!("trace=007 step={step} op={op:?}");
            let native_errno = op
                .run(&native)
                .err()
                .map(|error| error.raw_os_error().expect("native syscall errno"));
            assert_eq!(native_errno, expected_errno, "native step={step} {op:?}");
            let overlay_errno = op
                .run(mount)
                .err()
                .map(|error| error.raw_os_error().expect("FUSE syscall errno"));
            assert_eq!(overlay_errno, native_errno, "FUSE step={step} {op:?}");
            assert_eq!(
                snapshot(mount),
                snapshot(&native),
                "namespace/content/hardlinks step={step} {op:?}"
            );
            assert_eq!(snapshot(&lower), initial_lower, "lower changed step={step}");
        }
        session.unmount().expect("trace-007 clean unmount");
        assert!(!OverlayFs::is_mountpoint(&root.path().join("merged")));
    }
}
