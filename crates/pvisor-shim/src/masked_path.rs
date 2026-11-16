//! Masking fallback shared by the Linux syscalls and fault regressions.
use anyhow::Result;

pub(crate) fn mask_path(
    bind_null: impl FnOnce() -> Result<()>,
    mount_empty: impl FnOnce() -> Result<()>,
) -> Result<()> {
    match bind_null() {
        Err(error) => match error
            .downcast_ref::<std::io::Error>()
            .and_then(|io| io.raw_os_error())
        {
            Some(libc::ENOENT) => Ok(()),
            Some(libc::ENOTDIR) => mount_empty(),
            _ => Err(error),
        },
        Ok(()) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn failure(errno: i32) -> anyhow::Error {
        anyhow::Error::new(std::io::Error::from_raw_os_error(errno)).context("mount syscall")
    }

    #[test]
    fn directory_mask_requires_empty_mount_and_propagates_its_failure() {
        for errno in [None, Some(libc::EPERM), Some(libc::ENOSPC)] {
            let called = Cell::new(false);
            let result = mask_path(
                || Err(failure(libc::ENOTDIR)),
                || {
                    called.set(true);
                    errno.map_or(Ok(()), |errno| Err(failure(errno)))
                },
            );
            assert!(called.get(), "directory masking was silently skipped");
            assert_eq!(
                result.err().and_then(|e| e
                    .downcast_ref::<std::io::Error>()
                    .and_then(|e| e.raw_os_error())),
                errno
            );
        }
    }

    #[test]
    fn file_absent_and_failed_masks_have_no_directory_fallback() {
        for errno in [
            None,
            Some(libc::ENOENT),
            Some(libc::EPERM),
            Some(libc::EIO),
            Some(libc::EACCES),
        ] {
            let result = mask_path(
                || errno.map_or(Ok(()), |errno| Err(failure(errno))),
                || panic!("unexpected directory fallback"),
            );
            let expected = errno.filter(|errno| *errno != libc::ENOENT);
            assert_eq!(
                result.err().and_then(|e| e
                    .downcast_ref::<std::io::Error>()
                    .and_then(|e| e.raw_os_error())),
                expected
            );
        }
    }
}
