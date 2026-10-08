//! Local firmware discovery only; acquisition belongs to packaging.
use std::{
    io,
    path::{Path, PathBuf},
};

pub(super) fn firmware_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "libkrunfw.5.dylib"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "libkrunfw.so.5"
    }
}

pub(super) fn bundled_directory() -> Option<PathBuf> {
    let directory = std::env::current_exe().ok()?.parent()?.to_path_buf();
    directory
        .join(firmware_name())
        .is_file()
        .then_some(directory)
}

pub(super) fn resolve(directory: Option<&Path>) -> io::Result<PathBuf> {
    let executable;
    let directory = match directory {
        Some(directory) => directory,
        None => {
            executable = std::env::current_exe()?;
            executable.parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "current executable has no firmware directory",
                )
            })?
        }
    };
    canonical_file(&directory.join(firmware_name()))
}

pub(super) fn canonical_file(path: &Path) -> io::Result<PathBuf> {
    let path = path.canonicalize().map_err(|error| io::Error::new(
        error.kind(),
        format!("local firmware {} is unavailable: {error}; install packaged firmware or select a local firmware directory", path.display()),
    ))?;
    if !path.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("local firmware {} must be a regular file", path.display()),
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_directory_resolves_only_its_regular_file() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join(firmware_name());
        let error = resolve(Some(directory.path())).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(error.to_string().contains(&file.display().to_string()));
        std::fs::create_dir(&file).unwrap();
        assert_eq!(
            resolve(Some(directory.path())).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        std::fs::remove_dir(&file).unwrap();
        std::fs::write(&file, b"local firmware").unwrap();
        let resolved = resolve(Some(directory.path())).unwrap();
        assert!(resolved.is_absolute());
        assert_eq!(resolved, file.canonicalize().unwrap());
    }

    #[test]
    fn executable_adjacent_discovery_never_uses_another_directory() {
        let executable = std::env::current_exe().unwrap();
        let expected = canonical_file(&executable.parent().unwrap().join(firmware_name()));
        match (resolve(None), expected) {
            (Ok(actual), Ok(expected)) => assert_eq!(actual, expected),
            (Err(actual), Err(expected)) => {
                assert_eq!(actual.kind(), expected.kind());
                assert_eq!(actual.to_string(), expected.to_string());
            }
            result => panic!("inconsistent executable-adjacent discovery: {result:?}"),
        }
    }

    #[test]
    fn firmware_symlink_is_canonicalized() {
        let directory = tempfile::tempdir().unwrap();
        let payload = directory.path().join("payload");
        std::fs::write(&payload, b"firmware").unwrap();
        std::os::unix::fs::symlink(&payload, directory.path().join(firmware_name())).unwrap();
        assert_eq!(
            resolve(Some(directory.path())).unwrap(),
            payload.canonicalize().unwrap()
        );
    }
}
