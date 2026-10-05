//! Confined OCI source inspection used only while publishing immutable images.
#[cfg(test)]
use super::MAX_READ;
use super::Response;
#[cfg(test)]
use super::protocol::MAX_FRAME;
use super::protocol::hash;
use crate::image::oci::ImageStore;
use anyhow::ensure;
use std::ffi::{CStr, CString, OsStr};
use std::fs::{self, File, OpenOptions};
#[cfg(test)]
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path};
use std::sync::{Arc, Mutex};

mod metadata;
#[cfg(test)]
mod tests;

// Every component is opened relative to its parent fd, without following links.
// This remains confined even if a directory is renamed during a request.
pub(super) fn open_child(parent: &File, name: &OsStr, directory: bool) -> anyhow::Result<File> {
    let name = CString::new(name.as_bytes())?;
    let flags = libc::O_RDONLY
        | libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if directory { libc::O_DIRECTORY } else { 0 };
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(super) fn parent(
    store: &ImageStore,
    digest: &str,
    path: &[u8],
) -> anyhow::Result<(File, Vec<u8>)> {
    let digest = crate::image::oci::digest_hex(digest)?;
    ensure!(!path.contains(&0), "NUL in cache path");
    let path = Path::new(OsStr::from_bytes(path));
    let components: Vec<_> = path.components().collect();
    ensure!(
        components.iter().all(|c| matches!(c, Component::Normal(_))),
        "cache path must be relative without dot or parent components"
    );
    let roots = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(store.root.join("rootfs-v3/sha256"))?;
    let mut directory = open_child(&roots, OsStr::new(digest), true)?;
    for component in components.iter().take(components.len().saturating_sub(1)) {
        directory = open_child(&directory, component.as_os_str(), true)?;
    }
    let name = components
        .last()
        .map_or_else(|| b".".to_vec(), |c| c.as_os_str().as_bytes().to_vec());
    Ok((directory, name))
}

fn directory_names(directory: File) -> anyhow::Result<Vec<Vec<u8>>> {
    let raw = unsafe { libc::fdopendir(directory.as_raw_fd()) };
    if raw.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let _ = directory.into_raw_fd(); // fdopendir owns the descriptor on success.
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let directory = Directory(raw);
    let mut names = Vec::new();
    loop {
        #[cfg(target_os = "macos")]
        unsafe {
            *libc::__error() = 0;
        }
        #[cfg(target_os = "linux")]
        unsafe {
            *libc::__errno_location() = 0;
        }
        let entry = unsafe { libc::readdir(directory.0) };
        if entry.is_null() {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(0) {
                return Err(error.into());
            }
            break;
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(name.to_vec());
        }
    }
    Ok(names)
}

#[allow(clippy::unnecessary_cast)] // libc stat field widths differ by platform.
fn metadata_at(directory: &File, name: &[u8]) -> anyhow::Result<Response> {
    let name = CString::new(name)?;
    let mut m: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            &mut m,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let kind = match m.st_mode & libc::S_IFMT {
        libc::S_IFREG => "file",
        libc::S_IFDIR => "directory",
        libc::S_IFLNK => "symlink",
        _ => "special",
    };
    let target = if kind == "symlink" {
        let mut bytes = vec![0u8; 4096];
        let size = unsafe {
            libc::readlinkat(
                directory.as_raw_fd(),
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if size < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        ensure!(
            (size as usize) < bytes.len(),
            "symlink target exceeds protocol limit"
        );
        bytes.truncate(size as usize);
        Some(bytes)
    } else {
        None
    };
    #[cfg(target_os = "macos")]
    {
        // O_SYMLINK opens the link itself on macOS. Combining it with
        // O_NOFOLLOW instead rejects symlinks with ELOOP, breaking directory
        // metadata listings. The parent remains confined by directory fds.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_SYMLINK | libc::O_CLOEXEC | libc::O_EVTONLY,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let mut bytes = [0u8; 128];
        let count = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                c"user.containers.override_stat".as_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                0,
                0,
            )
        };
        if count >= 0 {
            let text = std::str::from_utf8(&bytes[..count as usize])?;
            let fields: Vec<_> = text.split(':').collect();
            ensure!(fields.len() == 3, "invalid image override_stat");
            m.st_uid = fields[0].parse()?;
            m.st_gid = fields[1].parse()?;
            m.st_mode = u16::from_str_radix(fields[2], 8)?;
        } else {
            let error = std::io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(libc::ENOATTR | libc::ENOTSUP)) {
                return Err(error.into());
            }
        }
    }
    Ok(Response::Metadata {
        kind: kind.into(),
        size: m.st_size as u64,
        mode: m.st_mode as u32,
        uid: m.st_uid,
        gid: m.st_gid,
        inode: m.st_ino as u64,
        nlink: m.st_nlink as u64,
        mtime: m.st_mtime as i64,
        mtime_nsec: m.st_mtime_nsec as i64,
        target,
    })
}

pub(super) fn stat(store: &ImageStore, digest: &str, path: &[u8]) -> anyhow::Result<Response> {
    metadata::stat(store, digest, path)
}

pub(super) fn directory(
    store: &ImageStore,
    digest: &str,
    path: &[u8],
) -> anyhow::Result<Arc<Vec<Vec<u8>>>> {
    metadata::directory(store, digest, path)
}

// Test-only source requests exercise confinement without exposing a daemon API.
#[cfg(test)]
pub(super) enum Request {
    List {
        digest: String,
        path: Vec<u8>,
        offset: usize,
    },
    Stat {
        digest: String,
        path: Vec<u8>,
    },
    #[cfg(test)]
    Read {
        digest: String,
        path: Vec<u8>,
        offset: u64,
        length: u32,
    },
}

#[cfg(test)]
pub(super) fn handle(store: &ImageStore, request: Request) -> anyhow::Result<(Response, Vec<u8>)> {
    let response = match request {
        Request::List {
            digest,
            path,
            offset,
        } => {
            let names = metadata::directory(store, &digest, &path)?;
            ensure!(offset <= names.len(), "directory offset out of range");
            let mut page = Vec::new();
            let mut attributes = Vec::new();
            // Leave room for JSON field names and pagination. Symlink targets and
            // non-UTF-8 names can expand substantially in JSON byte arrays.
            let mut frame_bytes = 128;
            for name in names.iter().skip(offset).take(256) {
                let mut child = path.clone();
                if !child.is_empty() {
                    child.push(b'/');
                }
                child.extend_from_slice(name);
                let attr = metadata::stat(store, &digest, &child)?;
                let bytes = serde_json::to_vec(name)?.len() + serde_json::to_vec(&attr)?.len() + 2;
                if frame_bytes + bytes > MAX_FRAME {
                    ensure!(!page.is_empty(), "directory entry exceeds protocol limit");
                    break;
                }
                frame_bytes += bytes;
                page.push(name.clone());
                attributes.push(attr);
            }
            let end = offset + page.len();
            Response::Entries {
                names: page,
                metadata: attributes,
                next_offset: (end < names.len()).then_some(end),
            }
        }
        Request::Stat { digest, path } => metadata::stat(store, &digest, &path)?,
        #[cfg(test)]
        Request::Read {
            digest,
            path,
            offset,
            length,
        } => {
            ensure!(
                length > 0 && length <= MAX_READ,
                "read length must be 1..={MAX_READ}"
            );
            let (directory, name) = parent(store, &digest, &path)?;
            let mut file = open_child(&directory, OsStr::from_bytes(&name), false)?;
            ensure!(file.metadata()?.is_file(), "only regular files can be read");
            file.seek(SeekFrom::Start(offset))?;
            let mut body = Vec::new();
            file.take(length as u64).read_to_end(&mut body)?;
            return Ok((
                Response::Data {
                    length: body.len() as u32,
                    sha256: hash(&body),
                },
                body,
            ));
        }
    };
    Ok((response, Vec::new()))
}
