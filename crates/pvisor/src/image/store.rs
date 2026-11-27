//! One local image namespace for container commands, Sessions and the cache service.
use anyhow::{Context, ensure};
use fs2::FileExt;
use pvisor_journal::api::{DurableFiles, Persistence};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs, io,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub(crate) struct LocalStore {
    pub(crate) root: PathBuf,
}

const MANAGED_IMPORT_PREFIX: &str = ".leased-import-v1-";
const STAGING_ADMISSION_LOCK: &str = "import-staging-v1.lock";

/// The locked lease survives until cleanup finishes. An interrupted owner
/// leaves its work and lease available for a later recovery pass.
pub(crate) struct ImportDirectory {
    root: PathBuf,
    work: PathBuf,
    _lease: File,
}

impl ImportDirectory {
    pub(crate) fn path(&self) -> &Path {
        &self.work
    }
}

impl Drop for ImportDirectory {
    fn drop(&mut self) {
        if let Err(error) = remove_work(&self.root) {
            tracing::warn!(%error, "cannot clean image import staging");
        }
    }
}

fn remove_work(root: &Path) -> io::Result<()> {
    let work = root.join("work");
    match fs::symlink_metadata(&work) {
        Ok(metadata) if metadata.is_dir() => remove_import(&work)?,
        Ok(_) => fs::remove_file(work)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    // Remove the lease only after deleting the data, so recovery is retryable.
    fs::remove_file(root.join("lease"))?;
    fs::remove_dir(root)
}

pub(crate) fn remove_import(root: &Path) -> std::io::Result<()> {
    restore_import_permissions(root)?;
    fs::remove_dir_all(root)
}

fn restore_import_permissions(root: &Path) -> std::io::Result<()> {
    let mut pending = vec![root.to_owned()];
    while let Some(path) = pending.pop() {
        if fs::symlink_metadata(&path)?.is_dir() {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
            for entry in fs::read_dir(&path)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    pending.push(entry.path());
                }
            }
        }
    }
    Ok(())
}

// The opened handle pins the stale inode even if another process renames it.
fn still_named(path: &Path, opened: &File) -> io::Result<bool> {
    match path.symlink_metadata() {
        Ok(visible) => {
            let expected = opened.metadata()?;
            Ok((visible.dev(), visible.ino()) == (expected.dev(), expected.ino()))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

enum StagingClaim {
    Skip,
    RemovedEmpty,
    Abandoned(AbandonedImport),
}

struct AbandonedImport {
    root: PathBuf,
    directory: File,
    lease: File,
}

impl AbandonedImport {
    fn still_named(&self) -> io::Result<bool> {
        Ok(still_named(&self.root, &self.directory)?
            && still_named(&self.root.join("lease"), &self.lease)?)
    }

    fn remove(&self) -> io::Result<bool> {
        if !self.still_named()? {
            return Ok(false);
        }
        // The wrapper is reserved for the lease and its own work tree.
        for entry in fs::read_dir(&self.root)? {
            if !matches!(entry?.file_name().as_encoded_bytes(), b"lease" | b"work") {
                return Ok(false);
            }
        }
        if !self.still_named()? {
            return Ok(false);
        }
        remove_work(&self.root)?;
        Ok(true)
    }
}

impl LocalStore {
    pub(crate) fn discover(root: Option<PathBuf>) -> anyhow::Result<Self> {
        Self::open(resolve_root(
            root,
            crate::env::os(crate::env::cache::IMAGE_STORE_ENV).map(PathBuf::from),
            crate::env::os(crate::env::cache::DOCKER_HOME_ENV).map(PathBuf::from),
            dirs::data_local_dir(),
        )?)
    }

    /// Explicit roots stay isolated from process-global defaults.
    pub(crate) fn open(root: PathBuf) -> anyhow::Result<Self> {
        private_directory(&root)?;
        let root = root.canonicalize()?;
        for name in ["configs", "staging", "locks", "projections"] {
            private_directory(&root.join(name))?;
        }
        Ok(Self { root })
    }

    pub(crate) const POOL_DIR: &str = "image-pack-v1";

    pub(crate) fn pool(&self) -> PathBuf {
        self.root.join(Self::POOL_DIR)
    }
    pub(crate) fn writable_pool(&self) -> anyhow::Result<PathBuf> {
        let path = self.pool();
        private_directory(&path)?;
        Ok(path)
    }
    pub(crate) fn projections(&self) -> PathBuf {
        self.root.join("projections")
    }
    pub(crate) fn staging(&self) -> anyhow::Result<ImportDirectory> {
        let _admission = self.lock_file(STAGING_ADMISSION_LOCK)?;
        let temporary = tempfile::Builder::new()
            .prefix(MANAGED_IMPORT_PREFIX)
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(self.root.join("staging"))?;
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(temporary.path().join("lease"))?;
        FileExt::lock_exclusive(&lease)?;
        let work = temporary.path().join("work");
        private_directory(&work)?;
        Ok(ImportDirectory {
            root: temporary.keep(),
            work,
            _lease: lease,
        })
    }

    pub(crate) fn prune_staging(&self) -> anyhow::Result<usize> {
        // Snapshot names so deletion does not change an in-progress directory walk.
        // Only one stage's handles are held during reclamation.
        let mut roots = Vec::new();
        for entry in fs::read_dir(self.root.join("staging"))? {
            let entry = entry?;
            if entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(MANAGED_IMPORT_PREFIX.as_bytes())
            {
                roots.push(entry.path());
            }
        }
        let mut removed = 0;
        let mut failed = 0;
        let mut first_error = None;
        for root in roots {
            let claim = {
                // Keep admission only until the lease has been acquired.
                let _admission = self.lock_file(STAGING_ADMISSION_LOCK)?;
                self.claim_abandoned_stage(&root)
            };
            let result = match claim {
                Ok(StagingClaim::Skip) => Ok(false),
                Ok(StagingClaim::RemovedEmpty) => Ok(true),
                Ok(StagingClaim::Abandoned(stage)) => match stage.remove() {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                    result => result.map_err(anyhow::Error::from),
                },
                Err(error) => Err(error),
            };
            match result {
                Ok(reclaimed) => removed += usize::from(reclaimed),
                Err(error) => {
                    failed += 1;
                    first_error.get_or_insert_with(|| {
                        error.context(format!("inspect or clean image import {}", root.display()))
                    });
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error).with_context(|| {
                format!("removed {removed} interrupted image import directories; cleanup failed for {failed} directories")
            });
        }
        Ok(removed)
    }

    // The caller holds admission, which the allocator also takes before creating a lease.
    fn claim_abandoned_stage(&self, root: &Path) -> anyhow::Result<StagingClaim> {
        let directory = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(root)
        {
            Ok(directory) => directory,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) || error.raw_os_error() == Some(libc::ELOOP) =>
            {
                return Ok(StagingClaim::Skip);
            }
            Err(error) => return Err(error.into()),
        };
        let metadata = directory.metadata()?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Ok(StagingClaim::Skip);
        }
        let lease = match OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join("lease"))
        {
            Ok(lease) => lease,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // An interrupted allocation or teardown can leave an empty wrapper.
                if !still_named(root, &directory)? {
                    return Ok(StagingClaim::Skip);
                }
                return match fs::remove_dir(root) {
                    Ok(()) => Ok(StagingClaim::RemovedEmpty),
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
                        ) =>
                    {
                        Ok(StagingClaim::Skip)
                    }
                    Err(error) => Err(error.into()),
                };
            }
            Err(error) => return Err(error.into()),
        };
        let lease_metadata = lease.metadata()?;
        ensure!(
            lease_metadata.is_file()
                && lease_metadata.uid() == metadata.uid()
                && lease_metadata.mode() & 0o077 == 0,
            "import lease must be an owned private regular file"
        );
        match FileExt::try_lock_exclusive(&lease) {
            Ok(()) => Ok(StagingClaim::Abandoned(AbandonedImport {
                root: root.to_owned(),
                directory,
                lease,
            })),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(StagingClaim::Skip),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn retain_config(&self, digest: &str, bytes: &[u8]) -> anyhow::Result<()> {
        let hex = super::oci::digest_hex(digest)?;
        ensure!(
            crate::util::encode_hex(&Sha256::digest(bytes)) == hex,
            "image config digest mismatch"
        );
        Persistence::atomic_write(&self.config(digest)?, bytes, 0o600)?;
        Ok(())
    }
    pub(crate) fn config(&self, digest: &str) -> anyhow::Result<PathBuf> {
        Ok(self
            .root
            .join("configs")
            .join(format!("{}.json", super::oci::digest_hex(digest)?)))
    }
    pub(crate) fn pull_lock(&self, reference: &str, architecture: &str) -> anyhow::Result<File> {
        let (reference, _) = super::oci::cache_reference(reference)?;
        let key = crate::util::encode_hex(&Sha256::digest(serde_json::to_vec(&(
            reference,
            architecture,
        ))?));
        self.lock_file(&key)
    }

    pub(super) fn lock_file(&self, key: &str) -> anyhow::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.root.join("locks").join(key))?;
        file.lock_exclusive()?;
        Ok(file)
    }

    pub(crate) fn retain_prepared_config(
        &self,
        source: &super::oci::ImageStore,
        image: &super::oci::PreparedImage,
    ) -> anyhow::Result<()> {
        let manifest: serde_json::Value = serde_json::from_slice(&fs::read(
            source
                .root
                .join("metadata/manifests-v1")
                .join(format!("{}.json", super::oci::digest_hex(&image.digest)?)),
        )?)?;
        let digest = manifest["config"]["digest"]
            .as_str()
            .context("OCI manifest is missing its config digest")?;
        let bytes = fs::read(
            source
                .root
                .join("blobs/sha256")
                .join(super::oci::digest_hex(digest)?),
        )?;
        self.retain_config(digest, &bytes)
    }
}

fn resolve_root(
    explicit: Option<PathBuf>,
    image_env: Option<PathBuf>,
    docker_env: Option<PathBuf>,
    data: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    explicit
        .or(image_env)
        .or_else(|| docker_env.map(|root| root.join("images")))
        .or_else(|| data.map(|root| root.join("pvisor/docker/images")))
        .context("cannot find user image data directory")
}

pub(super) fn private_directory(path: &Path) -> anyhow::Result<()> {
    match fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
    {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no arguments or memory preconditions.
    let uid = unsafe { libc::geteuid() };
    ensure!(
        metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o077 == 0,
        "image storage must be an owned private directory: {}",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn client_first_pool_initialization_keeps_private_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let store = LocalStore::open(temp.path().join("images")).unwrap();
        let _client = super::super::cache::CacheClient::local(&store).unwrap();
        assert_eq!(fs::metadata(store.pool()).unwrap().mode() & 0o777, 0o700);
        assert_eq!(store.writable_pool().unwrap(), store.pool());
    }
    #[test]
    fn staging_reclamation_preserves_replaced_owners_leases_and_unexpected_entries() {
        let temp = tempfile::tempdir().unwrap();
        let store = LocalStore::open(temp.path().join("images")).unwrap();
        let staging = store.root.join("staging");
        let seed = |name: &str| {
            let root = staging.join(format!("{MANAGED_IMPORT_PREFIX}{name}"));
            private_directory(&root).unwrap();
            fs::create_dir(root.join("work")).unwrap();
            fs::write(root.join("work/input"), name.as_bytes()).unwrap();
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(root.join("lease"))
                .unwrap();
            root
        };
        let replaced_owner = seed("owner");
        let replaced_lease = seed("lease");
        let unknown_entry = seed("unknown");
        let stages = [&replaced_owner, &replaced_lease, &unknown_entry].map(|root| {
            let StagingClaim::Abandoned(stage) = store.claim_abandoned_stage(root).unwrap() else {
                panic!("fixture stage was not acquired");
            };
            stage
        });

        let previous = temp.path().join("previous");
        fs::rename(&replaced_owner, &previous).unwrap();
        assert_eq!(seed("owner"), replaced_owner);
        fs::write(replaced_owner.join("work/input"), b"replacement").unwrap();
        fs::rename(replaced_lease.join("lease"), temp.path().join("old-lease")).unwrap();
        File::create(replaced_lease.join("lease")).unwrap();
        fs::write(unknown_entry.join("keep"), b"unmanaged").unwrap();

        for stage in stages {
            assert!(!stage.remove().unwrap());
        }
        assert_eq!(fs::read(previous.join("work/input")).unwrap(), b"owner");
        assert_eq!(
            fs::read(replaced_owner.join("work/input")).unwrap(),
            b"replacement"
        );
        assert_eq!(
            fs::read(replaced_lease.join("work/input")).unwrap(),
            b"lease"
        );
        assert!(replaced_lease.join("lease").is_file());
        assert_eq!(
            fs::read(unknown_entry.join("work/input")).unwrap(),
            b"unknown"
        );
        assert_eq!(fs::read(unknown_entry.join("keep")).unwrap(), b"unmanaged");
        assert!(unknown_entry.join("lease").is_file());
    }

    #[test]
    fn staging_prune_reports_an_invalid_lease_after_reclaiming_other_imports() {
        let temporary = tempfile::tempdir().unwrap();
        let store = LocalStore::open(temporary.path().join("images")).unwrap();
        let staging = store.root.join("staging");
        let invalid = staging.join(format!("{MANAGED_IMPORT_PREFIX}invalid"));
        private_directory(&invalid).unwrap();
        fs::create_dir(invalid.join("lease")).unwrap();
        fs::write(invalid.join("recovery-data"), b"keep").unwrap();

        let valid = staging.join(format!("{MANAGED_IMPORT_PREFIX}abandoned"));
        private_directory(&valid).unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(valid.join("lease"))
            .unwrap();
        fs::create_dir(valid.join("work")).unwrap();

        let error = store.prune_staging().unwrap_err();
        assert!(format!("{error:#}").contains(&invalid.display().to_string()));
        assert!(
            error
                .to_string()
                .contains("removed 1 interrupted image import")
        );
        assert!(!valid.exists());
        assert_eq!(fs::read(invalid.join("recovery-data")).unwrap(), b"keep");
    }

    #[test]
    fn staging_prune_preserves_live_and_legacy_imports() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temporary = tempfile::tempdir().unwrap();
        let store = LocalStore::open(temporary.path().join("images")).unwrap();
        let staging = store.root.join("staging");
        let live = store.staging().unwrap();
        assert_eq!(live.root.metadata().unwrap().mode() & 0o077, 0);
        fs::write(live.path().join("live"), b"active transaction").unwrap();

        let abandoned = staging.join(format!("{MANAGED_IMPORT_PREFIX}abandoned"));
        let work = abandoned.join("work");
        private_directory(&abandoned).unwrap();
        fs::create_dir_all(work.join("readonly")).unwrap();
        let legacy = staging.join(".import-legacy");
        fs::create_dir(&legacy).unwrap();
        let preserved = legacy.join("partial");
        fs::write(&preserved, b"legacy version has no lease").unwrap();
        symlink(&preserved, work.join("outside")).unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(abandoned.join("lease"))
            .unwrap();
        fs::set_permissions(work.join("readonly"), fs::Permissions::from_mode(0o000)).unwrap();
        private_directory(&staging.join(format!("{MANAGED_IMPORT_PREFIX}unadmitted"))).unwrap();
        let unrelated = staging.join(format!("{MANAGED_IMPORT_PREFIX}nonprivate"));
        fs::create_dir(&unrelated).unwrap();
        fs::set_permissions(&unrelated, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(store.prune_staging().unwrap(), 2);
        assert_eq!(store.prune_staging().unwrap(), 0);
        assert!(unrelated.is_dir());
        assert_eq!(
            fs::read(&preserved).unwrap(),
            b"legacy version has no lease"
        );
        assert_eq!(
            fs::read(live.path().join("live")).unwrap(),
            b"active transaction"
        );
        assert!(!abandoned.exists());
        drop(live);
        let mut remaining: Vec<_> = fs::read_dir(staging)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        remaining.sort();
        assert_eq!(remaining, [legacy, unrelated]);
    }

    #[test]
    fn imports_are_temporary_and_configuration_identity_is_checked() {
        let temp = tempfile::tempdir().unwrap();
        let store = LocalStore::open(temp.path().join("images")).unwrap();
        let staging = store.staging().unwrap();
        let path = staging.path().to_owned();
        fs::write(path.join("partial"), b"unfinished").unwrap();
        drop(staging);
        assert!(!path.exists());
        let bytes = b"{\"config\":{}}";
        let digest = format!("sha256:{}", crate::util::encode_hex(&Sha256::digest(bytes)));
        store.retain_config(&digest, bytes).unwrap();
        assert!(store.retain_config(&digest, b"changed").is_err());
        assert_eq!(fs::read(store.config(&digest).unwrap()).unwrap(), bytes);
    }
    #[test]
    fn import_cleanup_handles_read_only_directories_without_following_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let external = temp.path().join("external");
        fs::create_dir(&external).unwrap();
        fs::write(external.join("keep"), b"external").unwrap();
        fs::set_permissions(&external, fs::Permissions::from_mode(0o555)).unwrap();
        let store = LocalStore::open(temp.path().join("images")).unwrap();
        let staging = store.staging().unwrap();
        let path = staging.path().to_owned();
        fs::create_dir(path.join("readonly")).unwrap();
        fs::write(path.join("readonly/file"), b"source").unwrap();
        symlink(&external, path.join("readonly/link")).unwrap();
        fs::set_permissions(path.join("readonly"), fs::Permissions::from_mode(0o555)).unwrap();
        drop(staging);
        assert!(!path.exists());
        assert_eq!(fs::read(external.join("keep")).unwrap(), b"external");
        assert_eq!(fs::metadata(&external).unwrap().mode() & 0o777, 0o555);
        fs::set_permissions(external, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn path_precedence_preserves_explicit_and_docker_isolation() {
        let p = |s: &str| Some(PathBuf::from(s));
        assert_eq!(
            resolve_root(p("explicit"), p("env"), p("docker"), p("data")).unwrap(),
            Path::new("explicit")
        );
        assert_eq!(
            resolve_root(None, p("env"), p("docker"), p("data")).unwrap(),
            Path::new("env")
        );
        assert_eq!(
            resolve_root(None, None, p("docker"), p("data")).unwrap(),
            Path::new("docker/images")
        );
        assert_eq!(
            resolve_root(None, None, None, p("data")).unwrap(),
            Path::new("data/pvisor/docker/images")
        );
        assert!(resolve_root(None, None, None, None).is_err());
    }
    #[test]
    fn rejects_symlink_and_shared_roots_without_changing_them() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let shared = temp.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(LocalStore::open(shared.clone()).is_err());
        let link = temp.path().join("link");
        symlink(&shared, &link).unwrap();
        assert!(LocalStore::open(link).is_err());
        assert_eq!(fs::metadata(shared).unwrap().mode() & 0o777, 0o755);
    }
}
