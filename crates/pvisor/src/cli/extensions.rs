//! First-party companion commands installed alongside pvisor.
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub(crate) const BUILTINS: &[&str] = &[
    "run",
    "apply",
    "drop",
    "status",
    "kill",
    "fork",
    "inspect",
    "extensions",
    "help",
];

#[derive(Debug, Clone, serde::Serialize)]
pub struct Companion {
    pub name: &'static str,
    pub description: &'static str,
}

const COMMANDS: &[Companion] = &[
    Companion {
        name: "memory-pool",
        description: "Serve the experimental shared VM cold-page pool",
    },
    Companion {
        name: "cache",
        description: "Serve or query the shared OCI file cache",
    },
    Companion {
        name: "replay",
        description: "Replay an agent-native trajectory",
    },
    Companion {
        name: "tui",
        description: "Run a Job in an interactive terminal",
    },
];

fn check_trust(metadata: &std::fs::Metadata) -> anyhow::Result<()> {
    anyhow::ensure!(
        (metadata.uid() == unsafe { libc::geteuid() } || metadata.uid() == 0)
            && metadata.mode() & 0o022 == 0,
        "command installation must be owned by the current user or root and not group/world writable"
    );
    Ok(())
}

fn installation_directory() -> anyhow::Result<PathBuf> {
    let directory = std::env::current_exe()?.parent().unwrap().to_path_buf();
    check_trust(&std::fs::symlink_metadata(&directory)?)?;
    Ok(directory)
}

fn check_executable(path: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    check_trust(&metadata)?;
    anyhow::ensure!(
        metadata.is_file() && metadata.mode() & 0o111 != 0,
        "command must be an executable regular file"
    );
    Ok(())
}

pub fn find(name: &str) -> anyhow::Result<Option<(PathBuf, Companion)>> {
    let Some(command) = COMMANDS.iter().find(|command| command.name == name) else {
        return Ok(None);
    };
    let path = installation_directory()?.join(format!("pvisor-{name}"));
    match check_executable(&path) {
        Ok(()) => Ok(Some((path, command.clone()))),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub fn discover() -> anyhow::Result<Vec<(PathBuf, Companion)>> {
    COMMANDS
        .iter()
        .filter_map(|command| find(command.name).transpose())
        .collect()
}

pub fn core_executable() -> anyhow::Result<PathBuf> {
    let path = installation_directory()?.join("pvisor");
    check_executable(&path)?;
    Ok(path)
}

pub fn dispatch(name: &str, args: &[OsString]) -> anyhow::Result<()> {
    let (path, _) =
        find(name)?.ok_or_else(|| anyhow::anyhow!("pvisor-{name} extension is not installed"))?;
    execute(path, args)
}

pub(crate) fn execute(path: PathBuf, args: &[OsString]) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    anyhow::ensure!(
        path.parent() == Some(installation_directory()?.as_path()),
        "extension is outside the installation"
    );
    check_executable(&path)?;
    Err(std::process::Command::new(path).args(args).exec().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn companions_require_trusted_regular_executables() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("pvisor-tui");
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        check_executable(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(check_executable(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(check_executable(&path).is_err());
        let link = temporary.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(check_executable(&link).is_err());
        assert!(find("../escape").unwrap().is_none());
        assert!(find("status").unwrap().is_none());
    }
}
