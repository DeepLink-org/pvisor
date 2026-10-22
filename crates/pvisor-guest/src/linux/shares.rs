//! Guest filesystem setup shared by Jobs and controlled workloads.
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

pub(super) fn install(config: &GuestConfig) -> io::Result<()> {
    for (index, share) in config.mounts.iter().enumerate() {
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
