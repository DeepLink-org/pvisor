use crate::api::{DurableFiles, Persistence};
use anyhow::Context as _;
use std::{fs::OpenOptions, io::Write, os::unix::fs::PermissionsExt, path::Path};

impl DurableFiles for Persistence {
    fn sync_directory(path: &Path) -> anyhow::Result<()> {
        std::fs::File::open(path)?
            .sync_all()
            .with_context(|| format!("sync directory {}", path.display()))
    }

    /// Create a directory tree and sync every newly created directory entry.
    fn create_dir_all_durable(path: &Path) -> anyhow::Result<()> {
        create_dir_all_durable_using(path, Self::sync_directory)
    }

    /// Atomically replace a file after syncing both its contents and parent directory.
    fn atomic_write(path: &Path, contents: &[u8], mode: u32) -> anyhow::Result<()> {
        Self::atomic_write_observed(path, contents, mode, |_, _, _| {})
    }

    /// Report completed persistence steps without depending on a logging frontend.
    /// Durations exclude observer work; observers run after the write/cleanup finishes.
    /// A failed step is reported too, and the original I/O error is preserved.
    fn atomic_write_observed(
        path: &Path,
        contents: &[u8],
        mode: u32,
        mut observe: impl FnMut(&'static str, std::time::Duration, bool),
    ) -> anyhow::Result<()> {
        use std::fs;
        use std::time::Instant;
        let mut timings = Vec::with_capacity(6);
        macro_rules! step {
            ($phase:literal, $operation:expr) => {{
                let start = Instant::now();
                let result = $operation;
                timings.push(($phase, start.elapsed(), result.is_ok()));
                result?
            }};
        }
        let result = (|| -> anyhow::Result<()> {
            let parent = path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", path.display()))?;
            let start = Instant::now();
            let mut directory_sync = std::time::Duration::ZERO;
            let prepared = create_dir_all_durable_using(parent, |directory| {
                let start = Instant::now();
                let result = Self::sync_directory(directory);
                directory_sync += start.elapsed();
                result
            });
            timings.push((
                "directory_prepare",
                start.elapsed().saturating_sub(directory_sync),
                prepared.is_ok(),
            ));
            timings.push(("directory_prepare_sync", directory_sync, prepared.is_ok()));
            prepared?;
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("pvisor");
            let temporary = parent.join(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
            let result = (|| -> anyhow::Result<()> {
                let file = step!(
                    "file_write",
                    (|| -> anyhow::Result<_> {
                        let mut file = OpenOptions::new()
                            .create_new(true)
                            .write(true)
                            .open(&temporary)
                            .with_context(|| {
                                format!("create temporary file {}", temporary.display())
                            })?;
                        file.set_permissions(fs::Permissions::from_mode(mode))?;
                        file.write_all(contents)?;
                        Ok(file)
                    })()
                );
                step!(
                    "file_sync",
                    file.sync_all()
                        .with_context(|| format!("sync temporary file {}", temporary.display()))
                );
                step!(
                    "rename",
                    fs::rename(&temporary, path).with_context(|| format!(
                        "replace {} with {}",
                        path.display(),
                        temporary.display()
                    ))
                );
                step!("directory_sync", Self::sync_directory(parent));
                Ok(())
            })();
            if result.is_err() {
                let _ = fs::remove_file(&temporary);
            }
            result
        })();
        for (phase, elapsed, success) in timings {
            observe(phase, elapsed, success);
        }
        result
    }
}

fn create_dir_all_durable_using(
    path: &Path,
    mut sync: impl FnMut(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut missing = Vec::new();
    let mut cursor = path;
    while !cursor.exists() {
        missing.push(cursor.to_path_buf());
        let Some(parent) = cursor.parent() else {
            break;
        };
        if parent.as_os_str().is_empty() {
            break;
        }
        cursor = parent;
    }
    std::fs::create_dir_all(path)
        .with_context(|| format!("create directory tree {}", path.display()))?;
    if let Some(first) = missing.last() {
        let parent = first
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        sync(parent)?;
    }
    // All entries exist before syncing. Each intermediate directory sync also
    // commits its child entry, so repeating it as the next parent is redundant.
    for directory in missing.iter().rev() {
        sync(directory)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observed_atomic_write_reports_steps_and_failed_rename_without_leaking_tempfiles() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("new/record.json");
        let mut steps = Vec::new();
        Persistence::atomic_write_observed(&path, b"record", 0o600, |phase, _, ok| {
            steps.push((phase, ok))
        })
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"record");
        assert_eq!(
            steps,
            vec![
                ("directory_prepare", true),
                ("directory_prepare_sync", true),
                ("file_write", true),
                ("file_sync", true),
                ("rename", true),
                ("directory_sync", true)
            ]
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let directory = root.path().join("destination");
        std::fs::create_dir(&directory).unwrap();
        steps.clear();
        assert!(
            Persistence::atomic_write_observed(&directory, b"record", 0o600, |phase, _, ok| steps
                .push((phase, ok)))
            .is_err()
        );
        assert_eq!(steps.last(), Some(&("rename", false)));
        assert!(std::fs::read_dir(root.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn directory_barriers_cover_each_entry_once_and_propagate_failure() {
        let root = tempfile::tempdir().unwrap();
        let leaf = root.path().join("a/b/c");
        let mut synced = Vec::new();
        create_dir_all_durable_using(&leaf, |path| {
            synced.push(path.to_path_buf());
            Ok(())
        })
        .unwrap();
        assert!(leaf.is_dir());
        assert_eq!(
            synced,
            vec![
                root.path().to_path_buf(),
                root.path().join("a"),
                root.path().join("a/b"),
                leaf.clone()
            ]
        );
        synced.clear();
        create_dir_all_durable_using(&leaf, |path| {
            synced.push(path.to_path_buf());
            Ok(())
        })
        .unwrap();
        assert!(synced.is_empty());
        let mut calls = 0;
        assert!(
            create_dir_all_durable_using(&root.path().join("other/child"), |_| {
                calls += 1;
                anyhow::bail!("injected directory sync failure")
            })
            .is_err()
        );
        assert_eq!(calls, 1);
    }
}
