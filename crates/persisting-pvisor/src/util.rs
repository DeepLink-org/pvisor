//! Shared helpers for pVisor.

use anyhow::Context;
pub use persisting_control::unix_now_ms;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub(crate) use persisting_journal::{create_dir_all_durable, sync_directory};

/// Atomically replace a file after syncing both its contents and parent directory.
pub(crate) fn atomic_write(path: &Path, contents: &[u8], mode: u32) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", path.display()))?;
    create_dir_all_durable(parent)?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("persisting");
    let temporary = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .with_context(|| format!("create temporary file {}", temporary.display()))?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.write_all(contents)?;
        file.sync_all()
            .with_context(|| format!("sync temporary file {}", temporary.display()))?;
        fs::rename(&temporary, path)
            .with_context(|| format!("replace {} with {}", path.display(), temporary.display()))?;
        sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Publish owner-only JSON using the same durable replacement as Run records.
pub(crate) fn write_private_json(path: &Path, value: &impl serde::Serialize) -> anyhow::Result<()> {
    atomic_write(path, &serde_json::to_vec_pretty(value)?, 0o600)
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn atomic_write_replaces_private_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("record.json");
        atomic_write(&path, b"first", 0o600).unwrap();
        atomic_write(&path, b"second", 0o600).unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn private_json_preserves_previous_contents_on_failure() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nested/result.json");
        write_private_json(&path, &serde_json::json!({"state": "completed"})).unwrap();
        let original = fs::read(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let invalid = std::collections::BTreeMap::from([(vec![1, 2], "invalid JSON key")]);
        assert!(write_private_json(&path, &invalid).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);

        let directory = temp.path().join("existing-directory");
        fs::create_dir(&directory).unwrap();
        assert!(write_private_json(&directory, &true).is_err());
        assert!(directory.is_dir());
        assert_eq!(
            fs::read_dir(temp.path()).unwrap().count(),
            2,
            "temporary file leaked"
        );
    }

    #[test]
    fn durable_directory_creation_handles_nested_paths() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("one/two/three");

        create_dir_all_durable(&path).unwrap();
        assert!(path.is_dir());
    }
}
