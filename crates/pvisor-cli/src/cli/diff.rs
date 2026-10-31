//! Bounded, no-follow inputs shared by review and apply's text diff presentation.

use anyhow::{Context, ensure};
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

// Walk from / so symlinks in the root itself cannot redirect a preview either.
// Open the final entry nonblocking before checking its type, to avoid FIFO waits.
pub(super) fn read_regular(root: &Path, relative: &Path, limit: u64) -> anyhow::Result<Vec<u8>> {
    ensure!(root.is_absolute(), "diff root must be absolute");
    ensure!(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))),
        "unsafe diff path"
    );
    let full = root.join(relative);
    let mut directory = File::open("/")?;
    let mut components = full
        .components()
        .filter(|c| *c != Component::RootDir)
        .peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            anyhow::bail!("unsafe diff path");
        };
        let name = std::ffi::CString::new(name.as_bytes())?;
        let last = components.peek().is_none();
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if last {
                libc::O_NONBLOCK
            } else {
                libc::O_DIRECTORY
            };
        // SAFETY: directory owns a live descriptor and name is NUL-terminated.
        let raw = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: openat returned a new descriptor, owned exclusively here.
        directory = unsafe { File::from_raw_fd(raw) };
        if last {
            let metadata = directory.metadata()?;
            ensure!(
                metadata.is_file() && metadata.len() <= limit,
                "non-regular or large file"
            );
            let mut bytes = Vec::new();
            directory
                .take(limit.saturating_add(1))
                .read_to_end(&mut bytes)?;
            ensure!(bytes.len() as u64 <= limit, "large file");
            return Ok(bytes);
        }
    }
    anyhow::bail!("not a regular file")
}

pub(super) fn is_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

// The renderer only opens private copies of the bytes already checked above.
// Replacing a source path during rendering cannot change its input.
pub(super) fn unified(old: &[u8], new: &[u8], label: &str) -> anyhow::Result<Vec<u8>> {
    let temporary = tempfile::tempdir()?;
    let old_path = temporary.path().join("old");
    let new_path = temporary.path().join("new");
    std::fs::write(&old_path, old)?;
    std::fs::write(&new_path, new)?;
    let output = std::process::Command::new("diff")
        .args(["-u", "--label"])
        .arg(format!("a/{label}"))
        .arg("--label")
        .arg(format!("b/{label}"))
        .arg("--")
        .arg(old_path)
        .arg(new_path)
        .output()
        .with_context(|| format!("render diff for {label:?}"))?;
    ensure!(
        matches!(output.status.code(), Some(0 | 1)),
        "diff failed for {label:?}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn bounded_reads_reject_symlinks_special_files_and_escaping_paths() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("files")).unwrap();
        std::fs::write(root.join("files/text"), b"hello").unwrap();
        symlink("files", root.join("alias")).unwrap();
        symlink("files/text", root.join("link")).unwrap();
        let fifo = std::ffi::CString::new(root.join("fifo").as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo is a NUL-terminated path within this test's temporary directory.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(
            read_regular(&root, Path::new("files/text"), 5).unwrap(),
            b"hello"
        );
        assert!(read_regular(&root, Path::new("files/text"), 4).is_err());
        for path in [
            "alias/text",
            "link",
            "fifo",
            "files",
            "../escape",
            "/etc/passwd",
            "",
        ] {
            assert!(
                read_regular(&root, Path::new(path), 1024).is_err(),
                "{path}"
            );
        }
        assert!(read_regular(&root.join("alias"), Path::new("text"), 1024).is_err());
    }

    #[test]
    fn text_detection_checks_all_bytes_and_rejects_invalid_utf8() {
        assert!(is_text(b""));
        assert!(is_text("hello 世界\n".as_bytes()));
        assert!(!is_text(b"text\xff"));
        let mut bytes = vec![b'x'; 8192];
        bytes.push(0);
        assert!(!is_text(&bytes));
    }
}
