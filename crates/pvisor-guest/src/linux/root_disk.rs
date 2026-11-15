//! OCI filesystem construction stays inside Linux, never on NTFS.
use super::{check, mount_with_options};
use pvisor_guest::{NetworkConfig, RootDisk};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(super) fn enter(root: &RootDisk, network: Option<&NetworkConfig>) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !Path::new(&root.device).exists() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "root block device did not appear",
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    fs::create_dir_all("/.pvisor-layers")?;
    if !root.layers.is_empty() {
        mount_with_options(
            "pvisor-layers",
            Path::new("/.pvisor-layers"),
            "virtiofs",
            libc::MS_RDONLY | libc::MS_NODEV | libc::MS_NOSUID | libc::MS_NOEXEC,
            None,
        )?;
    }
    let mut layers = Vec::new();
    for layer in &root.layers {
        let mut file = File::open(format!("/.pvisor-layers/{}", layer.sha256))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0; 65536];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        let actual = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        if actual != layer.sha256 {
            return Err(invalid("OCI layer digest changed before guest extraction"));
        }
        file.rewind()?;
        layers.push((file, layer.media_type.clone()));
    }
    fs::create_dir_all("/.pvisor-root")?;
    mount_with_options(
        &root.device,
        Path::new("/.pvisor-root"),
        "ext4",
        libc::MS_RELATIME,
        None,
    )?;
    check(unsafe { libc::chroot(c"/.pvisor-root".as_ptr()) })?;
    check(unsafe { libc::chdir(c"/".as_ptr()) })?;
    // Do not expose proc's magic fd links to an archive while PID 1 still owns
    // bootstrap descriptors. Pseudo-filesystems are mounted only after unpacking.
    for (input, media) in layers {
        apply_layer(input, &media)?;
    }
    if let Some(network) = network {
        // OCI images need not ship a resolver. The native disk profile owns
        // its guest root, just as the Unix host profile owns its resolver
        // layer. Refresh this after extraction and on retained-disk starts.
        install_resolver(Path::new("/etc/resolv.conf"), network.gateway)?;
    }
    Ok(())
}

fn install_resolver(path: &Path, gateway: [u8; 4]) -> io::Result<()> {
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| invalid("resolver has no parent"))?,
    )?;
    // Replace an image's resolver symlink without following it into /run or
    // another image-owned file. Malformed directory destinations fail closed.
    match fs::remove_file(path) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    fs::write(
        path,
        format!("nameserver {}\n", std::net::Ipv4Addr::from(gateway)),
    )?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))
}

fn clean(path: &Path) -> io::Result<PathBuf> {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Normal(name) => result.push(name),
            Component::CurDir => (),
            _ => return Err(invalid("OCI archive path escapes its Linux root")),
        }
    }
    Ok(result)
}

fn pseudo(path: &Path) -> bool {
    path.components().next().is_some_and(|p| {
        [
            Component::Normal("dev".as_ref()),
            Component::Normal("proc".as_ref()),
            Component::Normal("sys".as_ref()),
        ]
        .contains(&p)
    })
}

fn remove(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn no_symlink_parents(path: &Path) -> io::Result<()> {
    let mut current = PathBuf::from("/");
    for part in path.parent().unwrap_or(Path::new("")).components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => (),
            Ok(_) => return Err(invalid("OCI whiteout traverses a non-directory")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn apply_layer(input: File, media: &str) -> io::Result<()> {
    // Anonymous ext4 scratch: an image cannot rename or replace the archive.
    let name = CString::new("/")?;
    let fd = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
            0o600,
        )
    };
    check(fd)?;
    use std::os::fd::FromRawFd;
    let mut tar = unsafe { File::from_raw_fd(fd) };
    let input: Box<dyn Read> = if media.ends_with("+gzip") || media.ends_with(".gzip") {
        Box::new(flate2::read::GzDecoder::new(input))
    } else {
        Box::new(input)
    };
    let count = io::copy(&mut input.take((2u64 << 30) + 1), &mut tar)?;
    if count > (2u64 << 30) {
        return Err(invalid("uncompressed OCI layer exceeds 2 GiB"));
    }
    tar.rewind()?;
    let mut archive = tar::Archive::new(tar.try_clone()?);
    for entry in archive.entries()? {
        let entry = entry?;
        let path = clean(&entry.path()?)?;
        if pseudo(&path) {
            continue;
        }
        if let Some(name) = path
            .file_name()
            .and_then(|p| p.to_str())
            .and_then(|s| s.strip_prefix(".wh."))
        {
            if name.is_empty() || name == "." || name == ".." {
                return Err(invalid("invalid OCI whiteout"));
            }
            let parent = path.parent().unwrap_or(Path::new(""));
            if name == ".wh..opq" {
                no_symlink_parents(&parent.join("entry"))?;
                let directory = Path::new("/").join(parent);
                if directory.is_dir() {
                    for child in fs::read_dir(directory)? {
                        remove(&child?.path())?;
                    }
                }
            } else {
                let target = parent.join(name);
                no_symlink_parents(&target)?;
                remove(&Path::new("/").join(target))?;
            }
        }
    }
    tar.seek(SeekFrom::Start(0))?;
    let mut archive = tar::Archive::new(tar);
    archive.set_preserve_permissions(true);
    archive.set_preserve_ownerships(true);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = clean(&entry.path()?)?;
        if path
            .file_name()
            .and_then(|p| p.to_str())
            .is_some_and(|s| s.starts_with(".wh."))
        {
            continue;
        }
        let kind = entry.header().entry_type();
        if pseudo(&path) {
            if kind.is_dir() {
                continue;
            }
            return Err(invalid("OCI image replaces a reserved pseudo-filesystem"));
        }
        if !(kind.is_file() || kind.is_dir() || kind.is_symlink() || kind.is_hard_link()) {
            return Err(invalid("OCI layer contains an unsupported special file"));
        }
        if kind.is_hard_link() {
            clean(
                &entry
                    .link_name()?
                    .ok_or_else(|| invalid("missing hardlink target"))?,
            )?;
        }
        if !entry.unpack_in("/")? {
            return Err(invalid("OCI archive entry escaped its root"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod resolver_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn resolver_is_created_refreshed_and_replaces_image_links_without_following_them() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("etc/resolv.conf");
        install_resolver(&path, [172, 17, 0, 1]).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"nameserver 172.17.0.1\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        install_resolver(&path, [192, 0, 2, 1]).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"nameserver 192.0.2.1\n");
        let target = root.path().join("image-resolver");
        fs::write(&target, b"image file must stay unchanged").unwrap();
        fs::remove_file(&path).unwrap();
        symlink(&target, &path).unwrap();
        install_resolver(&path, [172, 17, 0, 1]).unwrap();
        assert!(
            !fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(target).unwrap(), b"image file must stay unchanged");
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(install_resolver(&path, [172, 17, 0, 1]).is_err());
        assert!(path.is_dir());
    }
}
