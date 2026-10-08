use super::*;
use crate::image::cache::MAX_READ;
use crate::image::cache::backend::tests::fixture;
#[cfg(target_os = "macos")]
use pvisor_overlayfs::api::{OverlayFs, OverlayMounting};
#[cfg(target_os = "macos")]
use std::fs::OpenOptions;
#[cfg(target_os = "macos")]
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::Ordering;
#[test]
#[ignore = "requires a working host FUSE installation"]
fn native_mount_reads_lazily_and_unmounts() {
    use std::io::Read;
    let (temp, server, client, digest) = fixture();
    let filesystem = RemoteFs::new(client, digest, temp.path().join("client"), None).unwrap();
    let mount = mount(filesystem, temp.path()).unwrap();
    assert_eq!(
        fs::metadata(mount.path.join("large")).unwrap().len(),
        3 * MAX_READ as u64
    );
    assert_eq!(server.reads.load(Ordering::Relaxed), 0);
    assert_eq!(
        fs::read_link(mount.path.join("alias")).unwrap(),
        Path::new("large")
    );
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        let link = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_SYMLINK)
            .open(mount.path.join("alias"))
            .unwrap();
        let mut path = [0u8; libc::PATH_MAX as usize];
        assert_eq!(
            unsafe { libc::fcntl(link.as_raw_fd(), libc::F_GETPATH, path.as_mut_ptr()) },
            0
        );
    }
    let mut file = std::fs::File::open(mount.path.join("large")).unwrap();
    let mut bytes = [0u8; 16];
    file.read_exact(&mut bytes).unwrap();
    assert_eq!(bytes, [42; 16]);
    assert!(server.reads.load(Ordering::Relaxed) < 3);
    drop(file);
    let path = mount.path.clone();
    drop(mount);
    #[cfg(target_os = "macos")]
    assert!(!OverlayFs::is_mountpoint(&path));
    #[cfg(target_os = "linux")]
    assert!(!path.exists());
}
