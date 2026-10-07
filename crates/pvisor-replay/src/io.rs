use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{ReplayError, ReplayErrorKind, ResultExt};

pub fn read_regular_file(path: &Path) -> Result<Vec<u8>, ReplayError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Check the opened inode, without following a final symlink or blocking on a FIFO.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    if fs::symlink_metadata(path)
        .replay_context(ReplayErrorKind::Configuration, "inspect input")?
        .is_symlink()
    {
        return Err(ReplayError::configuration("input must not be a symlink"));
    }
    let file = options.open(path).replay_context(
        ReplayErrorKind::Configuration,
        format!("open {}", path.display()),
    )?;
    let metadata = file.metadata().replay_context(
        ReplayErrorKind::Configuration,
        format!("inspect {}", path.display()),
    )?;
    if !metadata.is_file() {
        return Err(ReplayError::configuration(format!(
            "input must be a regular file: {}",
            path.display()
        )));
    }
    const MAX_BYTES: u64 = 256 * 1024 * 1024;
    if metadata.len() > MAX_BYTES {
        return Err(ReplayError::configuration(format!(
            "input exceeds {MAX_BYTES} bytes: {}",
            path.display()
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len().min(8192) as usize);
    file.take(metadata.len() + 1)
        .read_to_end(&mut bytes)
        .replay_context(
            ReplayErrorKind::Configuration,
            format!("read {}", path.display()),
        )?;
    if bytes.len() as u64 != metadata.len() {
        return Err(ReplayError::configuration(format!(
            "input changed while being read: {}",
            path.display()
        )));
    }
    Ok(bytes)
}

pub(crate) struct BoundedFileRead {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

/// Read a resolved workspace target without following replacement symlinks.
/// The opened inode, rather than a prior path inspection, must be a regular file.
pub(crate) fn read_confined_regular_file(
    workspace: &Path,
    resolved_path: &Path,
    max_bytes: usize,
) -> Result<BoundedFileRead, ReplayError> {
    let workspace = canonicalize(workspace, ReplayErrorKind::Workspace, "workspace")?;
    let relative = resolved_path
        .strip_prefix(&workspace)
        .map_err(|_| ReplayError::new(ReplayErrorKind::Executor, "tool path escapes workspace"))?;
    let file = open_confined_file(&workspace, relative).replay_context(
        ReplayErrorKind::Executor,
        format!("open {}", resolved_path.display()),
    )?;
    if !file
        .metadata()
        .replay_context(ReplayErrorKind::Executor, "inspect Read target")?
        .is_file()
    {
        return Err(ReplayError::new(
            ReplayErrorKind::Executor,
            "Read target must be a regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(max_bytes.min(8192));
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .replay_context(ReplayErrorKind::Executor, "read tool target")?;
    let truncated = bytes.len() > max_bytes;
    bytes.truncate(max_bytes);
    Ok(BoundedFileRead { bytes, truncated })
}

#[cfg(unix)]
fn open_confined_file(workspace: &Path, relative: &Path) -> std::io::Result<File> {
    use std::ffi::{CString, OsStr};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::Component;

    fn open_component(directory: &File, name: &OsStr, is_directory: bool) -> std::io::Result<File> {
        let name = CString::new(name.as_bytes())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | if is_directory { libc::O_DIRECTORY } else { 0 };
        // Each basename is opened relative to a retained directory FD. O_NOFOLLOW
        // blocks symlink substitution; O_NONBLOCK prevents a FIFO open from waiting.
        // SAFETY: the directory FD is owned and the basename is a live NUL-terminated string.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat returned a new, exclusively owned file descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open("/")?;
    // Also anchor the canonical workspace without following ancestor substitutions.
    for component in workspace.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => directory = open_component(&directory, name, true)?,
            _ => return Err(std::io::ErrorKind::InvalidInput.into()),
        }
    }
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(std::io::ErrorKind::InvalidInput.into());
        };
        directory = open_component(&directory, name, components.peek().is_some())?;
    }
    Ok(directory)
}

#[cfg(not(unix))]
fn open_confined_file(_workspace: &Path, _relative: &Path) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure workspace Read requires Unix directory-relative file opens",
    ))
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ReplayError> {
    let parent = path
        .parent()
        .ok_or_else(|| ReplayError::configuration("output path has no parent"))?;
    fs::create_dir_all(parent).replay_context(
        ReplayErrorKind::Executor,
        format!("create {}", parent.display()),
    )?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("output"),
        uuid::Uuid::new_v4().simple()
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).replay_context(
        ReplayErrorKind::Executor,
        format!("create {}", temporary.display()),
    )?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .replay_context(
            ReplayErrorKind::Executor,
            format!("write {}", temporary.display()),
        )?;
    fs::rename(&temporary, path).replay_context(
        ReplayErrorKind::Executor,
        format!("replace {}", path.display()),
    )?;
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

pub fn atomic_write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), ReplayError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .replay_context(ReplayErrorKind::Internal, "serialize replay artifact")?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

pub fn canonicalize(
    path: &Path,
    kind: ReplayErrorKind,
    label: &str,
) -> Result<PathBuf, ReplayError> {
    fs::canonicalize(path).replay_context(kind, format!("resolve {label} {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_input_accepts_only_bounded_regular_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("trajectory");
        fs::write(&path, b"trajectory").unwrap();
        assert_eq!(read_regular_file(&path).unwrap(), b"trajectory");
        assert!(read_regular_file(temp.path()).is_err());
        #[cfg(unix)]
        {
            let link = temp.path().join("link");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(read_regular_file(&link).is_err());
            let fifo = temp.path().join("fifo");
            use std::os::unix::ffi::OsStrExt;
            let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            assert!(read_regular_file(&fifo).is_err());
        }
        File::create(&path)
            .unwrap()
            .set_len(256 * 1024 * 1024 + 1)
            .unwrap();
        assert!(read_regular_file(&path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn confined_read_rejects_symlink_substitution_after_path_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let outside = temp.path().join("outside");
        fs::create_dir_all(workspace.join("nested")).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(workspace.join("nested/source"), b"inside").unwrap();
        fs::write(outside.join("source"), b"outside secret").unwrap();
        let workspace = fs::canonicalize(workspace).unwrap();
        let resolved = fs::canonicalize(workspace.join("nested/source")).unwrap();

        fs::rename(workspace.join("nested"), workspace.join("retained")).unwrap();
        std::os::unix::fs::symlink(&outside, workspace.join("nested")).unwrap();
        assert!(read_confined_regular_file(&workspace, &resolved, 32).is_err());

        fs::remove_file(workspace.join("nested")).unwrap();
        fs::rename(workspace.join("retained"), workspace.join("nested")).unwrap();
        fs::remove_file(&resolved).unwrap();
        std::os::unix::fs::symlink(outside.join("source"), &resolved).unwrap();
        assert!(read_confined_regular_file(&workspace, &resolved, 32).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn confined_read_bounds_actual_bytes_of_a_large_sparse_file() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("large");
        let mut file = File::create(&path).unwrap();
        file.write_all(b"prefix").unwrap();
        file.set_len(1024 * 1024 * 1024).unwrap();
        let resolved = fs::canonicalize(path).unwrap();
        let read = read_confined_regular_file(workspace.path(), &resolved, 32).unwrap();
        assert_eq!(read.bytes.len(), 32);
        assert_eq!(&read.bytes[..6], b"prefix");
        assert!(read.truncated);
    }
}
