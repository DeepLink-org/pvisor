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
    sync::Arc,
};

struct FailingSource {
    root: PathBuf,
    short_success: bool,
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
    fn copy_file_direct(&self, _: &Path, mut destination: &fs::File) -> io::Result<u64> {
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
