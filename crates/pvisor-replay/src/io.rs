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
}
