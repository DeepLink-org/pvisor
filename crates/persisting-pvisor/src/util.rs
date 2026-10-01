//! Shared helpers for pVisor.

pub use persisting_control::unix_now_ms;
#[cfg(test)]
use std::fs;
use std::path::Path;

pub(crate) use persisting_journal::{create_dir_all_durable, sync_directory};

pub(crate) use persisting_journal::atomic_write;

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
