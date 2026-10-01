//! Executable extensions with an embedded, inert JSON manifest.
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

const START: &[u8] = b"\0PVISOR_COMMAND_MANIFEST_V1\n";
const END: &[u8] = b"\nPVISOR_COMMAND_MANIFEST_END\0";
const MAX_MANIFEST: usize = 4096;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandManifest {
    pub schema_version: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    pub session_protocol: u32,
}

/// Reference the returned string from main so release stripping retains it.
#[macro_export]
macro_rules! command_manifest {
    ($name:literal, $description:literal) => {
        concat!(
            "\0PVISOR_COMMAND_MANIFEST_V1\n",
            "{\"schema_version\":1,\"name\":\"",
            $name,
            "\",\"version\":\"",
            env!("CARGO_PKG_VERSION"),
            "\",\"description\":\"",
            $description,
            "\",\"session_protocol\":1}",
            "\nPVISOR_COMMAND_MANIFEST_END\0"
        )
    };
}

pub fn embedded_manifest(bytes: &[u8]) -> anyhow::Result<CommandManifest> {
    let mut found = None;
    for offset in memchr::memmem::find_iter(bytes, START) {
        let start = offset + START.len();
        let tail = &bytes[start..bytes.len().min(start + MAX_MANIFEST + END.len())];
        let Some(end) = tail.windows(END.len()).position(|value| value == END) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<CommandManifest>(&tail[..end]) else {
            continue;
        };
        anyhow::ensure!(
            manifest.schema_version == 1
                && manifest.session_protocol == persisting_control::SESSION_PROTOCOL_VERSION,
            "unsupported command manifest protocol"
        );
        anyhow::ensure!(
            !manifest.name.is_empty()
                && manifest.name.len() <= 64
                && !BUILTINS.contains(&manifest.name.as_str())
                && manifest
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
            "invalid command name"
        );
        anyhow::ensure!(
            manifest.description.len() <= 512
                && !manifest.description.chars().any(char::is_control)
                && manifest.version.len() <= 64
                && !manifest.version.chars().any(char::is_control),
            "invalid manifest text"
        );
        anyhow::ensure!(found.is_none(), "ambiguous command manifests");
        found = Some(manifest);
    }
    found.ok_or_else(|| anyhow::anyhow!("embedded command manifest not found"))
}

pub fn manifest_requested(manifest: &str) -> anyhow::Result<bool> {
    if std::env::args_os().nth(1).as_deref() != Some(OsStr::new("--pvisor-manifest")) {
        return Ok(false);
    }
    anyhow::ensure!(
        std::env::args_os().count() == 2,
        "--pvisor-manifest takes no arguments"
    );
    println!(
        "{}",
        serde_json::to_string(&embedded_manifest(manifest.as_bytes())?)?
    );
    Ok(true)
}

fn directories() -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = vec![std::env::current_exe()?.parent().unwrap().to_path_buf()];
    paths.extend(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .filter(|path| !path.as_os_str().is_empty()),
    );
    Ok(paths)
}

fn inspect(path: &Path, name: &str) -> anyhow::Result<CommandManifest> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.mode() & 0o111 != 0 && metadata.len() <= 256 * 1024 * 1024,
        "extension must be an executable regular file of at most 256 MiB"
    );
    let mut bytes = Vec::new();
    file.take(256 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 256 * 1024 * 1024, "extension is too large");
    let manifest = embedded_manifest(&bytes)?;
    anyhow::ensure!(
        manifest.name == name,
        "manifest name does not match executable name"
    );
    Ok(manifest)
}

pub fn find(name: &str) -> anyhow::Result<Option<(PathBuf, CommandManifest)>> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Ok(None);
    }
    for directory in directories()? {
        let path = directory.join(format!("pvisor-{name}"));
        if path.exists() {
            let manifest = inspect(&path, name)?;
            return Ok(Some((path.canonicalize()?, manifest)));
        }
    }
    Ok(None)
}

pub fn discover() -> anyhow::Result<Vec<(PathBuf, CommandManifest)>> {
    let mut commands = std::collections::BTreeMap::new();
    let mut seen = std::collections::HashSet::new();
    for directory in directories()? {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let filename = entry.file_name();
            let Some(name) = filename
                .to_str()
                .and_then(|name| name.strip_prefix("pvisor-"))
            else {
                continue;
            };
            if !seen.insert(name.to_owned()) {
                continue;
            }
            if let Ok(manifest) = inspect(&entry.path(), name) {
                commands.insert(name.to_owned(), (entry.path(), manifest));
            }
        }
    }
    Ok(commands.into_values().collect())
}

pub fn core_executable() -> anyhow::Result<PathBuf> {
    for directory in directories()? {
        let path = directory.join("pvisor");
        if path.is_file() {
            return Ok(path.canonicalize()?);
        }
    }
    anyhow::bail!("pvisor core executable is missing; install it alongside the extension")
}

pub fn dispatch(name: &str, args: &[OsString]) -> anyhow::Result<()> {
    let (path, _) =
        find(name)?.ok_or_else(|| anyhow::anyhow!("pvisor-{name} extension is not installed"))?;
    execute(path, args)
}

pub(crate) fn execute(path: PathBuf, args: &[OsString]) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    Err(std::process::Command::new(path).args(args).exec().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manifests_are_bounded_versioned_and_unambiguous() {
        let manifest = crate::command_manifest!("example", "Example command");
        assert_eq!(
            embedded_manifest(manifest.as_bytes()).unwrap().name,
            "example"
        );
        assert!(embedded_manifest(b"not a manifest").is_err());
        assert!(embedded_manifest(manifest.replace("example", "../escape").as_bytes()).is_err());
        assert!(
            embedded_manifest(
                manifest
                    .replace("\"session_protocol\":1", "\"session_protocol\":99")
                    .as_bytes()
            )
            .is_err()
        );
        assert!(embedded_manifest(format!("{manifest}{manifest}").as_bytes()).is_err());
    }
}
