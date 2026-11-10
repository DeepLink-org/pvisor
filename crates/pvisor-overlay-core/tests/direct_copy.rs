//! A producer's direct-copy failure must never publish a partial upper.
use pvisor_overlay_core::{
    OverlayCore,
    backend::{BackendAttachment, FileAttr, FileType, ReadOnlyBackend},
};
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

struct FailingSource {
    root: PathBuf,
    short_success: bool,
    healthy: AtomicBool,
    copies: AtomicUsize,
}
impl ReadOnlyBackend for FailingSource {
    fn prepare_metadata(&self, path: &Path) -> io::Result<()> {
        fs::symlink_metadata(self.root.join(path)).map(|_| ())
    }
    fn prepare_directory(&self, path: &Path) -> io::Result<()> {
        self.prepare_metadata(path)
    }
    fn attributes(&self, path: &Path) -> io::Result<FileAttr> {
        let m = fs::symlink_metadata(self.root.join(path))?;
        Ok(FileAttr {
            ino: m.ino(),
            size: m.len(),
            blocks: m.blocks(),
            atime: m.accessed()?,
            mtime: m.modified()?,
            ctime: m.modified()?,
            crtime: m.modified()?,
            kind: if m.is_dir() {
                FileType::Directory
            } else {
                FileType::RegularFile
            },
            perm: (m.mode() & 0o7777) as u16,
            uid: m.uid(),
            gid: m.gid(),
            nlink: m.nlink() as u32,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        })
    }
    fn read_at(&self, path: &Path, _: u64, _: u32) -> io::Result<Vec<u8>> {
        fs::read(self.root.join(path))
    }
    fn supports_direct_copy(&self) -> bool {
        true
    }
    fn copy_file_direct(&self, path: &Path, mut destination: &fs::File) -> io::Result<u64> {
        self.copies.fetch_add(1, Ordering::SeqCst);
        if self.healthy.load(Ordering::SeqCst) {
            let bytes = fs::read(self.root.join(path))?;
            destination.write_all(&bytes)?;
            return Ok(bytes.len() as u64);
        }
        destination.write_all(b"part")?;
        if self.short_success {
            Ok(4)
        } else {
            Err(io::Error::from_raw_os_error(libc::EIO))
        }
    }
    fn materialize_file(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
    fn materialize_tree(&self, _: &Path) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn partial_direct_copy_errors_leave_original_visible_without_upper_publication() {
    for prepared in [false, true] {
        for short_success in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let lower = temp.path().join("lower");
            let upper = temp.path().join("upper");
            fs::create_dir(&lower).unwrap();
            fs::write(lower.join("file"), b"original").unwrap();
            let _owner = BackendAttachment::new_stable_namespace(
                &lower,
                Arc::new(FailingSource {
                    root: lower.clone(),
                    short_success,
                    healthy: AtomicBool::new(false),
                    copies: AtomicUsize::new(0),
                }),
            )
            .unwrap();
            let core = OverlayCore::new(vec![lower.clone()], upper.clone(), None).unwrap();
            let error = if prepared {
                core.prepare_copy_ups(&["file".into()], false).unwrap_err()
            } else {
                core.copy_up(Path::new("file")).unwrap_err()
            };
            assert_eq!(error.raw_os_error(), Some(libc::EIO));
            assert!(!upper.join("file").exists());
            assert_eq!(fs::read(lower.join("file")).unwrap(), b"original");
            assert_eq!(core.metadata(Path::new("file")).unwrap().len(), 8);
        }
    }
}

#[test]
fn direct_copy_failure_retries_cleanly_then_reopens_without_recopying() {
    for prepared in [false, true] {
        for short_success in [false, true] {
            for separate_work in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let lower = temp.path().join("lower");
                let upper = temp.path().join("upper");
                let work = separate_work.then(|| temp.path().join("work"));
                fs::create_dir(&lower).unwrap();
                fs::write(lower.join("file"), b"original").unwrap();
                let source = Arc::new(FailingSource {
                    root: lower.clone(),
                    short_success,
                    healthy: AtomicBool::new(false),
                    copies: AtomicUsize::new(0),
                });
                let _attachment =
                    BackendAttachment::new_stable_namespace(&lower, source.clone()).unwrap();
                let core =
                    OverlayCore::new(vec![lower.clone()], upper.clone(), work.clone()).unwrap();
                let path = Path::new("file");
                for attempt in 1..=2 {
                    let error = if prepared {
                        core.prepare_copy_ups(&[path.into()], false).unwrap_err()
                    } else {
                        core.copy_up(path).unwrap_err()
                    };
                    assert_eq!(error.raw_os_error(), Some(libc::EIO));
                    assert_eq!(source.copies.load(Ordering::SeqCst), attempt);
                    assert_eq!(fs::read_dir(&upper).unwrap().count(), 0);
                    if let Some(work) = &work {
                        assert_eq!(fs::read_dir(work).unwrap().count(), 0);
                    }
                    assert_eq!(fs::read(lower.join(path)).unwrap(), b"original");
                    assert_eq!(core.metadata(path).unwrap().len(), 8);
                }

                // Only the injected transport behavior recovers; the stable
                // source's bytes and logical identity have never changed.
                source.healthy.store(true, Ordering::SeqCst);
                let guard = prepared.then(|| {
                    let copies = core.prepare_copy_ups(&[path.into()], false).unwrap();
                    core.use_prepared_copy_ups(copies).unwrap()
                });
                let published = core.copy_up(path).unwrap();
                assert_eq!(fs::read(&published).unwrap(), b"original");
                assert_eq!(source.copies.load(Ordering::SeqCst), 3);
                fs::write(&published, b"staged").unwrap();
                let staged_inode = fs::metadata(&published).unwrap().ino();
                assert_eq!(core.copy_up(path).unwrap(), published);
                assert_eq!(fs::read(&published).unwrap(), b"staged");
                assert_eq!(source.copies.load(Ordering::SeqCst), 3);
                drop(guard);
                assert_eq!(fs::read_dir(&upper).unwrap().count(), 1);
                if let Some(work) = &work {
                    assert_eq!(fs::read_dir(work).unwrap().count(), 0);
                }
                drop(core);

                let reopened = OverlayCore::open_existing(
                    vec![lower.clone()],
                    upper.clone(),
                    work.clone(),
                    vec![],
                    None,
                )
                .unwrap();
                assert_eq!(reopened.copy_up(path).unwrap(), published);
                assert_eq!(fs::read(&published).unwrap(), b"staged");
                assert_eq!(fs::metadata(&published).unwrap().ino(), staged_inode);
                assert_eq!(source.copies.load(Ordering::SeqCst), 3);
                assert_eq!(fs::read(lower.join(path)).unwrap(), b"original");
            }
        }
    }
}
