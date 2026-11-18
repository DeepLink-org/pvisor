//! Prepared templates and S3/static publication.
use crate::api::{PreparedCatalogService, PreparedImage, RegistryUpdate};
use anyhow::{Context, ensure};
use fs2::FileExt;
use pvisor::docker::{Image, ImageRegistry, Store};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const REGISTRY_CONFIG: &str = ".distribution/config/registries.json";

/// A directory of prepared sandbox templates and its registry configuration.
pub struct PreparedCatalog {
    images: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryConfig {
    version: u32,
    registries: Vec<ImageRegistry>,
}

impl PreparedCatalogService for PreparedCatalog {
    fn new(images: PathBuf) -> Self {
        Self { images }
    }

    fn list(&self) -> anyhow::Result<Vec<(String, Image)>> {
        let entries = match fs::read_dir(&self.images) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => return Err(error.into()),
        };
        let mut images = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let name = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .context("invalid image name")?;
                validate_name(name)?;
                let image: PreparedImage = read_json(&path)?;
                images.push((name.to_owned(), image.pooled_image));
            }
        }
        images.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(images)
    }

    fn inspect(&self, name: &str) -> anyhow::Result<serde_json::Value> {
        validate_name(name)?;
        let image: PreparedImage = read_json(&self.images.join(format!("{name}.json")))?;
        let mut metadata = image.pooled_image.inspect()?;
        metadata["RepoTags"] = serde_json::json!([name]);
        Ok(metadata)
    }

    fn push(&self, name: &str, destination: &str) -> anyhow::Result<serde_json::Value> {
        Store::validate_image_destination(destination)?;
        let _lock = image_lock(&self.images, name)?;
        let image: PreparedImage = read_json(&self.images.join(format!("{name}.json")))?;
        let portable = serde_json::to_vec(&portable_template(&image))?;
        let source = Store::open(image.rootfs)?.publish_registry_image(
            &image.pooled_image,
            name,
            destination,
            &portable,
        )?;
        Ok(serde_json::json!({
            "source": source, "image_handle": image.pooled_image.cache.handle,
            "digest": image.pooled_image.digest, "platform": image.pooled_image.platform,
        }))
    }

    fn pull(&self, name: &str, source: Option<&str>, refresh: bool) -> anyhow::Result<Image> {
        if let Some(source) = source {
            ensure!(
                ["s3://", "http://", "https://"]
                    .iter()
                    .any(|scheme| source.starts_with(scheme)),
                "--from requires an S3 or HTTP(S) publication"
            );
        }
        let _lock = image_lock(&self.images, name)?;
        let images = self.images.canonicalize()?;
        let manifest = images.join(format!("{name}.json"));
        ensure!(
            refresh || !manifest.try_exists()?,
            "image already exists; use --refresh"
        );
        let (bytes, mut repositories) = if let Some(source) = source {
            let repository = source.trim_end_matches('/').to_owned();
            (Store::read_image_repository(&repository)?, vec![repository])
        } else {
            let registries = PreparedCatalog::new(images.clone()).registries()?;
            ensure!(
                !registries.is_empty(),
                "image absent locally; configure registries or use --from"
            );
            let resolved = Store::resolve_image_repository(name, &registries)?;
            (resolved.template, resolved.repositories)
        };
        let mut image: PreparedImage = serde_json::from_slice(&bytes)?;
        crate::runtime::validate_image_profile(&image)?;
        image.rootfs = images.join("store");
        let store = Store::open(image.rootfs.clone())?;
        image.pooled_image.cache.location = image.rootfs.join("images/pool-v2");
        image.pooled_image.cache.repository = Some(repositories.remove(0));
        image.pooled_image.cache.mirrors = repositories;
        image.library_dir = None;
        store.import_image_config(&image.pooled_image)?;
        drop(store.open_image(&image.pooled_image)?);
        write_json(&manifest, &image, refresh)?;
        Ok(image.pooled_image)
    }

    fn prepare(
        &self,
        name: &str,
        build: impl FnOnce(PathBuf) -> anyhow::Result<PreparedImage>,
    ) -> anyhow::Result<()> {
        validate_name(name)?;
        fs::create_dir_all(&self.images)?;
        let images = self.images.canonicalize()?;
        let _lock = image_lock(&images, name)?;
        let root = images.join("store");
        let manifest = build(root.clone())?;
        ensure!(
            manifest.rootfs == root,
            "prepared template belongs to a different store"
        );
        write_json(&images.join(format!("{name}.json")), &manifest, true)
    }

    fn registries(&self) -> anyhow::Result<Vec<ImageRegistry>> {
        let config: RegistryConfig = match read_json(&self.images.join(REGISTRY_CONFIG)) {
            Ok(config) => config,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(vec![]);
            }
            Err(error) => return Err(error),
        };
        ensure!(
            config.version == 1,
            "unsupported registry configuration version"
        );
        Store::validate_image_registries(&config.registries)?;
        Ok(config.registries)
    }

    fn update_registries(&self, update: RegistryUpdate) -> anyhow::Result<Vec<ImageRegistry>> {
        private_directory(&self.images.join(".distribution"))?;
        private_directory(&self.images.join(".distribution/config"))?;
        let _lock = lock(&self.images.join(".distribution/registries.lock"))?;
        let mut registries = self.registries()?;
        match update {
            RegistryUpdate::Add(mut registry) => {
                registry.location = registry.location.trim_end_matches('/').into();
                registries.push(registry);
            }
            RegistryUpdate::Remove(name) => {
                registries.remove(registry_index(&registries, &name)?);
            }
            RegistryUpdate::Move { name, position } => {
                ensure!(
                    position > 0 && position <= registries.len(),
                    "priority position must be in 1..=registry count"
                );
                let registry = registries.remove(registry_index(&registries, &name)?);
                registries.insert(position - 1, registry);
            }
        }
        Store::validate_image_registries(&registries)?;
        let config = RegistryConfig {
            version: 1,
            registries,
        };
        write_json(&self.images.join(REGISTRY_CONFIG), &config, true)?;
        Ok(config.registries)
    }
}

fn registry_index(registries: &[ImageRegistry], name: &str) -> anyhow::Result<usize> {
    registries
        .iter()
        .position(|registry| registry.name == name)
        .context("unknown registry")
}

fn validate_name(name: &str) -> anyhow::Result<()> {
    pvisor::docker::Store::validate_image_key(name)
}

fn private_directory(path: &Path) -> anyhow::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "image distribution state must be an owned private directory: {}",
        path.display()
    );
    Ok(())
}

fn lock(path: &Path) -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "unsafe image lock"
    );
    file.lock_exclusive()?;
    Ok(file)
}

/// Preparation, publication and import serialize on the same image key.
fn image_lock(images: &Path, name: &str) -> anyhow::Result<File> {
    validate_name(name)?;
    private_directory(&images.join(".distribution"))?;
    let directory = images.join(".distribution/image-locks");
    private_directory(&directory)?;
    lock(&directory.join(format!("{name}.lock")))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= 1024 * 1024,
        "invalid image record"
    );
    Ok(serde_json::from_reader(file)?)
}

fn write_json(path: &Path, value: &impl Serialize, replace: bool) -> anyhow::Result<()> {
    let parent = path.parent().context("record requires parent")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(file.as_file_mut(), value)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    if replace {
        file.persist(path)?;
    } else {
        file.persist_noclobber(path)?;
    }
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn portable_template(image: &PreparedImage) -> PreparedImage {
    let mut portable = image.clone();
    portable.rootfs = "/pvisor/store".into();
    portable.pooled_image.cache.location = "/pvisor/store/images/pool-v2".into();
    portable.pooled_image.cache.repository = None;
    portable.pooled_image.cache.mirrors.clear();
    portable.library_dir = None;
    portable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_keys_cannot_collide_with_global_lock_namespaces() {
        let temp = tempfile::tempdir().unwrap();
        let name = "registries";
        let _image = image_lock(temp.path(), name).unwrap();
        let global = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(
                temp.path()
                    .join(".distribution")
                    .join(format!("{name}.lock")),
            )
            .unwrap();
        global
            .try_lock_exclusive()
            .expect("per-image and global locks must be different files");
    }

    #[test]
    fn image_keys_are_confined() {
        for name in ["../escape", ".hidden", "", "a/b"] {
            assert!(validate_name(name).is_err());
        }
        validate_name("python-3.12").unwrap();
    }

    #[test]
    fn records_are_atomic_and_new_imports_cannot_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("image.json");
        write_json(&path, &"original", false).unwrap();
        assert!(write_json(&path, &"updated", false).is_err());
        assert_eq!(read_json::<String>(&path).unwrap(), "original");
        write_json(&path, &"updated", true).unwrap();
        assert_eq!(read_json::<String>(&path).unwrap(), "updated");
    }

    #[test]
    fn configuration_never_shadows_a_valid_image_named_registries() {
        let temp = tempfile::tempdir().unwrap();
        let catalog = PreparedCatalog::new(temp.path().into());
        let image = temp.path().join("registries.json");
        std::fs::write(&image, b"local prepared image").unwrap();
        private_directory(&temp.path().join(".distribution")).unwrap();
        let policy = temp.path().join(".distribution/registries.json");
        std::fs::write(&policy, b"image publication policy").unwrap();
        assert!(catalog.registries().unwrap().is_empty());
        catalog
            .update_registries(RegistryUpdate::Add(ImageRegistry {
                name: "origin".into(),
                location: "https://origin.example/pvisor".into(),
                resolve: true,
            }))
            .unwrap();
        assert_eq!(catalog.registries().unwrap().len(), 1);
        assert_eq!(std::fs::read(image).unwrap(), b"local prepared image");
        assert_eq!(std::fs::read(policy).unwrap(), b"image publication policy");
    }

    #[test]
    fn configuration_is_versioned_bounded_and_strict_without_network_io() {
        let temp = tempfile::tempdir().unwrap();
        let catalog = PreparedCatalog::new(temp.path().into());
        private_directory(&temp.path().join(".distribution/config")).unwrap();
        write_json(
            &temp.path().join(REGISTRY_CONFIG),
            &serde_json::json!({
                "version": 99, "registries": []
            }),
            false,
        )
        .unwrap();
        assert!(catalog.registries().is_err());
        write_json(&temp.path().join(REGISTRY_CONFIG), &serde_json::json!({
            "version": 1, "registries": [
                {"name": "origin", "location": "https://origin.example/pvisor", "password": "not-a-real-secret"}
            ]
        }), true).unwrap();
        assert!(
            catalog.registries().is_err(),
            "unknown credential fields cannot be silently persisted"
        );
        let registry: ImageRegistry = serde_json::from_value(serde_json::json!({
            "name": "origin", "location": "https://origin.example/pvisor"
        }))
        .unwrap();
        assert!(
            registry.resolve,
            "legacy minimal entries resolve by default"
        );
        let mut entries: Vec<_> = (0..9)
            .map(|i| ImageRegistry {
                name: format!("r-{i}"),
                location: format!("https://r-{i}.example"),
                resolve: true,
            })
            .collect();
        assert!(Store::validate_image_registries(&entries).is_err());
        entries.truncate(1);
        entries[0].resolve = false;
        assert!(
            Store::resolve_image_repository("tiny", &entries).is_err(),
            "download-only sources must not be queried for names"
        );
    }
}
