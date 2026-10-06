use super::models::Registry;
use anyhow::{Context, ensure};
use fs2::FileExt;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub(super) struct Store {
    directory: PathBuf,
    _lock: File,
}

impl Store {
    pub fn open(directory: &Path) -> anyhow::Result<(Self, Registry)> {
        fs::create_dir_all(directory)?;
        ensure!(
            !fs::symlink_metadata(directory)?.file_type().is_symlink(),
            "state directory cannot be a symlink"
        );
        #[cfg(unix)]
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        let directory = directory.canonicalize()?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        let lock = options.open(directory.join("daemon.lock"))?;
        lock.try_lock_exclusive()
            .context("another daemon owns this state directory")?;
        let store = Self {
            directory,
            _lock: lock,
        };
        let path = store.directory.join("sandboxes.json");
        let registry = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file() && metadata.len() <= 16 * 1024 * 1024,
                    "invalid or oversized registry"
                );
                let mut options = OpenOptions::new();
                options.read(true);
                #[cfg(unix)]
                options.custom_flags(libc::O_NOFOLLOW);
                serde_json::from_reader::<_, Registry>(options.open(path)?)
                    .context("invalid registry; refusing to forget native ownership")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let registry = Registry {
                    version: 1,
                    owner: uuid::Uuid::new_v4().to_string(),
                    sandboxes: BTreeMap::new(),
                };
                store.save(&registry)?;
                registry
            }
            Err(error) => return Err(error.into()),
        };
        ensure!(registry.version == 1, "unsupported registry version");
        uuid::Uuid::parse_str(&registry.owner).context("invalid node owner")?;
        for (id, record) in &registry.sandboxes {
            ensure!(
                id == &record.sandbox.id && id.starts_with("sb-"),
                "invalid sandbox identity"
            );
            let uuid = uuid::Uuid::parse_str(&id[3..]).context("invalid sandbox identity")?;
            ensure!(
                id == &format!("sb-{uuid}")
                    && record.cpu_millis > 0
                    && record.memory_bytes > 0
                    && record.endpoint_token.len() >= 32,
                "invalid sandbox reservation or credential"
            );
        }
        Ok((store, registry))
    }

    pub fn save(&self, registry: &Registry) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(registry)?;
        ensure!(
            bytes.len() <= 16 * 1024 * 1024,
            "registry size limit exceeded"
        );
        let temporary = self
            .directory
            .join(format!(".registry-{}", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, self.directory.join("sandboxes.json"))?;
            File::open(&self.directory)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exclusive_owner_and_corruption_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let (store, first) = Store::open(directory.path()).unwrap();
        assert!(Store::open(directory.path()).is_err());
        drop(store);
        let (store, second) = Store::open(directory.path()).unwrap();
        assert_eq!(first.owner, second.owner);
        drop(store);
        fs::write(directory.path().join("sandboxes.json"), b"broken").unwrap();
        assert!(Store::open(directory.path()).is_err());
    }
}
