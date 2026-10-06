//! Host-only ephemeral RAM mount placement. Never consult TMPDIR.
use std::{
    fs, io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message.into())
}

// Resolve existing aliases even when an environment root has not been created.
fn resolved(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
        match fs::canonicalize(&result) {
            Ok(path) => result = path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(result)
}

fn protected(extra: &[&Path]) -> io::Result<Vec<PathBuf>> {
    let mut roots = extra
        .iter()
        .map(|path| resolved(path))
        .collect::<io::Result<Vec<_>>>()?;
    roots.push(resolved(&std::env::current_dir()?)?);
    for name in [
        "HOME",
        "CODEX_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "PVISOR_RUN_HOME",
    ] {
        if let Some(path) = std::env::var_os(name).filter(|value| !value.is_empty()) {
            roots.push(resolved(Path::new(&path))?);
        }
    }
    Ok(roots)
}

fn validate(root: &Path, uid: u32, shared: bool, protected: &[PathBuf]) -> io::Result<()> {
    if !root.is_absolute()
        || root
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid(
            "RAM runtime root must be an absolute normalized path",
        ));
    }
    for ancestor in root.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        let mode = metadata.permissions().mode();
        if !metadata.is_dir()
            || (metadata.uid() != 0 && metadata.uid() != uid)
            || (mode & 0o022 != 0 && !(metadata.uid() == 0 && mode & 0o1000 != 0))
        {
            return Err(invalid(format!(
                "unsafe RAM runtime ancestor: {}",
                ancestor.display()
            )));
        }
    }
    let metadata = fs::symlink_metadata(root)?;
    let mode = metadata.permissions().mode() & 0o7777;
    let safe = if shared {
        metadata.uid() == 0 && mode == 0o1777
    } else {
        metadata.uid() == uid && mode == 0o700
    };
    if !safe || protected.iter().any(|path| root.starts_with(path)) {
        return Err(invalid(format!(
            "RAM runtime root is not private/host-only: {}",
            root.display()
        )));
    }
    Ok(())
}

pub(super) fn private(root: &Path, extra: &[&Path]) -> io::Result<()> {
    validate(root, unsafe { libc::geteuid() }, false, &protected(extra)?)
}

fn select(
    candidates: &[(PathBuf, bool)],
    uid: u32,
    roots: &[PathBuf],
) -> io::Result<tempfile::TempDir> {
    let mut failures = Vec::new();
    for (root, shared) in candidates {
        let attempt: io::Result<tempfile::TempDir> = (|| {
            validate(root, uid, *shared, roots)?;
            let directory = tempfile::Builder::new()
                .prefix("ram-mount-")
                .permissions(fs::Permissions::from_mode(0o700))
                .tempdir_in(root)?;
            validate(directory.path(), uid, false, roots)?;
            Ok(directory)
        })();
        match attempt {
            Ok(directory) => return Ok(directory),
            Err(error) => failures.push(format!("{}: {error}", root.display())),
        }
    }
    Err(invalid(format!(
        "no safe host-only snapshot RAM runtime location: {}",
        failures.join("; ")
    )))
}

pub(super) fn temporary(extra: &[&Path]) -> io::Result<tempfile::TempDir> {
    let uid = unsafe { libc::geteuid() };
    #[cfg(target_os = "linux")]
    let candidates = [
        (PathBuf::from(format!("/run/user/{uid}")), false),
        (PathBuf::from("/tmp"), true),
    ];
    // /tmp is a symlink on macOS; use its real, system-owned sticky directory.
    #[cfg(target_os = "macos")]
    let candidates = [(PathBuf::from("/private/tmp"), true)];
    select(&candidates, uid, &protected(extra)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[cfg(target_os = "linux")]
    #[test]
    fn literal_tmp_fallback_is_private_ephemeral_and_fail_closed() {
        let runtime = PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() }));
        let directory = temporary(&[&runtime]).unwrap();
        let path = directory.path().to_path_buf();
        assert_eq!(path.parent(), Some(Path::new("/tmp")));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            0o700
        );
        drop(directory);
        assert!(fs::symlink_metadata(path).is_err());
        assert!(temporary(&[Path::new("/")]).is_err());
    }

    #[test]
    fn selection_rejects_protected_alias_symlink_permissions_and_owner() {
        let uid = unsafe { libc::geteuid() };
        let base = temporary(&[]).unwrap();
        let root = base.path().join("private");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let link = base.path().join("alias");
        symlink(&root, &link).unwrap();
        assert!(validate(&link, uid, false, &[]).is_err());
        assert!(validate(&root, uid.wrapping_add(1), false, &[]).is_err());
        assert!(validate(&root, uid, false, &[resolved(&link).unwrap()]).is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validate(&root, uid, false, &[]).is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let mount = select(&[(link, false), (root.clone(), false)], uid, &[]).unwrap();
        assert!(mount.path().starts_with(&root));
        assert_eq!(
            fs::metadata(mount.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(select(&[(root.clone(), false)], uid, &[root]).is_err());
        assert!(validate(Path::new("relative"), uid, false, &[]).is_err());
        assert!(select(&[(base.path().join("missing"), false)], uid, &[]).is_err());
    }
}
