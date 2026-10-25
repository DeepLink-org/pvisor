//! One local image namespace for container commands, Jobs and the cache service.
use anyhow::{Context, ensure};
use fs2::FileExt;
use pvisor_journal::api::{DurableFiles, Persistence};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub(crate) struct LocalStore {
    pub(crate) root: PathBuf,
}

impl LocalStore {
    pub(crate) fn discover(root: Option<PathBuf>) -> anyhow::Result<Self> {
        Self::open(resolve_root(
            root,
            std::env::var_os("PVISOR_IMAGE_STORE").map(PathBuf::from),
            std::env::var_os("PVISOR_DOCKER_HOME").map(PathBuf::from),
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

    pub(crate) fn pool(&self) -> PathBuf {
        self.root.join("pool-v2")
    }
    pub(crate) fn writable_pool(&self) -> anyhow::Result<PathBuf> {
        let path = self.pool();
        private_directory(&path)?;
        Ok(path)
    }
    pub(crate) fn projections(&self) -> PathBuf {
        self.root.join("projections")
    }
    pub(crate) fn staging(&self) -> anyhow::Result<tempfile::TempDir> {
        Ok(tempfile::Builder::new()
            .prefix(".import-")
            .tempdir_in(self.root.join("staging"))?)
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

fn private_directory(path: &Path) -> anyhow::Result<()> {
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
