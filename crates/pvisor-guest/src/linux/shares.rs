//! Guest filesystem setup shared by Sessions and controlled workloads.
use super::{check, mount};
use pvisor_guest::GuestConfig;
use std::{
    fs, io,
    os::unix::fs::{MetadataExt, PermissionsExt},
};

fn copy_volume(source: &std::path::Path, target: &std::path::Path, depth: usize) -> io::Result<()> {
    if depth > 256 {
        return Err(io::Error::other(
            "volume initialization exceeds directory depth limit",
        ));
    }
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_volume(&entry.path(), &target.join(entry.file_name()), depth + 1)?;
        }
    } else if metadata.is_file() {
        fs::copy(source, target)?;
    } else if metadata.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(source)?, target)?;
    } else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "volume initialization of special files is unsupported",
        ));
    }
    let path = std::ffi::CString::new(target.as_os_str().as_encoded_bytes())?;
    check(unsafe { libc::lchown(path.as_ptr(), metadata.uid(), metadata.gid()) })?;
    if !metadata.file_type().is_symlink() {
        fs::set_permissions(target, fs::Permissions::from_mode(metadata.mode()))?;
    }
    Ok(())
}

fn prepare_file_mountpoint(path: &std::path::Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            // Cold launches retain the container's writable upper, including
            // the empty directory beneath the previous virtio-fs mount.
            if !fs::symlink_metadata(path)?.is_dir()
                || fs::read_dir(path)?.next().transpose()?.is_some()
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "file share mountpoint must be an empty nonsymlink directory",
                ));
            }
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

pub(super) fn install(config: &GuestConfig) -> io::Result<()> {
    for (index, share) in config.mounts.iter().enumerate() {
        if share.source_file {
            let temporary = std::path::PathBuf::from(format!("/run/pvisor-file-{index}"));
            prepare_file_mountpoint(&temporary)?;
            mount(
                &share.tag,
                &temporary,
                "virtiofs",
                if share.read_only { libc::MS_RDONLY } else { 0 },
            )?;
            let source = temporary.join("file");
            if !fs::symlink_metadata(&source)?.is_file() {
                return Err(io::Error::other("file share entry is not an ordinary file"));
            }
            fs::create_dir_all(
                share
                    .target
                    .parent()
                    .ok_or_else(|| io::Error::other("file target has no parent"))?,
            )?;
            match fs::symlink_metadata(&share.target) {
                Ok(metadata) if metadata.is_file() => {}
                Ok(_) => return Err(io::Error::other("file bind target is not an ordinary file")),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&share.target)?;
                }
                Err(error) => return Err(error),
            }
            mount(
                source
                    .to_str()
                    .ok_or_else(|| io::Error::other("invalid file share path"))?,
                &share.target,
                "none",
                libc::MS_BIND,
            )?;
            if share.read_only {
                mount(
                    "",
                    &share.target,
                    "none",
                    libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                )?;
            }
            continue;
        }
        fs::create_dir_all(&share.target)?;
        if share.initialize {
            let temporary = std::path::PathBuf::from(format!("/run/pvisor-volume-{index}"));
            fs::create_dir_all(&temporary)?;
            mount(&share.tag, &temporary, "virtiofs", 0)?;
            if fs::read_dir(&temporary)?.next().is_none() {
                copy_volume(&share.target, &temporary, 0)?;
            }
            let path = std::ffi::CString::new(temporary.as_os_str().as_encoded_bytes())?;
            check(unsafe { libc::umount2(path.as_ptr(), 0) })?;
            fs::remove_dir(temporary)?;
        }
        mount(
            &share.tag,
            &share.target,
            "virtiofs",
            if share.read_only { libc::MS_RDONLY } else { 0 },
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_mountpoint_reuses_only_an_empty_nonsymlink_directory() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("mountpoint");
        prepare_file_mountpoint(&path).unwrap();
        let original = fs::symlink_metadata(&path).unwrap();
        prepare_file_mountpoint(&path).unwrap();
        assert_eq!(fs::symlink_metadata(&path).unwrap().ino(), original.ino());
        fs::write(path.join("keep"), b"image data").unwrap();
        assert!(prepare_file_mountpoint(&path).is_err());
        assert_eq!(fs::read(path.join("keep")).unwrap(), b"image data");
        let file = root.path().join("file");
        fs::write(&file, b"ordinary file").unwrap();
        assert!(prepare_file_mountpoint(&file).is_err());
        assert_eq!(fs::read(&file).unwrap(), b"ordinary file");
        for target in [root.path().join("absent"), root.path().join("empty")] {
            if target.ends_with("empty") {
                fs::create_dir(&target).unwrap();
            }
            let link = root.path().join("link");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(prepare_file_mountpoint(&link).is_err());
            assert!(
                fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            fs::remove_file(link).unwrap();
        }
    }
}
