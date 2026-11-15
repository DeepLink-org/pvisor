//! A single-file share without exposing its host parent or copying file data.
use super::*;

pub(super) struct Share {
    directory: tempfile::TempDir,
}
impl Share {
    pub(super) fn new(private_root: &Path, source: &Path) -> anyhow::Result<Self> {
        let before = fs::symlink_metadata(source)?;
        ensure!(
            before.is_file(),
            "file bind source is not an ordinary nonsymlink file"
        );
        let directory = tempfile::Builder::new()
            .prefix("file-bind-")
            .tempdir_in(private_root)?;
        let path = directory.path().join("file");
        fs::hard_link(source, &path).context(
            "file binds require hard-link permission and the same filesystem as the private container store"
        )?;
        let pinned = fs::symlink_metadata(&path)?;
        ensure!(
            pinned.is_file() && pinned.dev() == before.dev() && pinned.ino() == before.ino(),
            "file bind source changed while pinning"
        );
        Ok(Self { directory })
    }
    pub(super) fn path(&self) -> &Path {
        self.directory.path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_share_pins_only_selected_inode_and_cleans_its_link() {
        let directory = tempfile::tempdir().unwrap();
        let private = tempfile::tempdir_in(directory.path()).unwrap();
        let source = directory.path().join("config");
        fs::write(&source, b"first").unwrap();
        fs::write(directory.path().join("secret-sibling"), b"not shared").unwrap();
        let share = Share::new(private.path(), &source).unwrap();
        assert_eq!(fs::read_dir(share.path()).unwrap().count(), 1);
        let pin = share.path().join("file");
        fs::write(&source, b"second").unwrap();
        assert_eq!(fs::read(&pin).unwrap(), b"second");
        fs::rename(&source, directory.path().join("old")).unwrap();
        fs::write(&source, b"replacement").unwrap();
        assert_eq!(
            fs::read(&pin).unwrap(),
            b"second",
            "binds retain the admitted inode"
        );
        fs::write(&pin, b"guest write").unwrap();
        assert_eq!(
            fs::read(directory.path().join("old")).unwrap(),
            b"guest write"
        );
        drop(share);
        assert!(!pin.exists());
        assert_eq!(fs::read(&source).unwrap(), b"replacement");
    }
}
