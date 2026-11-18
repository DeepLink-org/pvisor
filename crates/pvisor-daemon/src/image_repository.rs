//! Prepared templates and S3/static publication.
use anyhow::{Context, ensure};
use clap::{Args, Subcommand};
use fs2::FileExt;
use pvisor_daemon::api::PreparedImage;
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub(super) const DEFAULT_IMAGES: &str = ".pvisor/images";

#[derive(Args)]
pub(super) struct Manage {
    #[arg(long)]
    images_dir: PathBuf,
    #[command(subcommand)]
    command: Operation,
}

#[derive(Subcommand)]
enum Operation {
    Push {
        name: String,
        #[arg(long)]
        to: String,
    },
    Pull {
        name: String,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        refresh: bool,
    },
    Ls,
    Inspect {
        name: String,
    },
}

impl Manage {
    pub(super) fn run(self) -> anyhow::Result<()> {
        let value = match self.command {
            Operation::Ls => {
                let entries = match fs::read_dir(&self.images_dir) {
                    Ok(entries) => Some(entries),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error.into()),
                };
                let mut images = Vec::new();
                for entry in entries.into_iter().flatten() {
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
                serde_json::to_value(images)?
            }
            Operation::Inspect { name } => {
                validate_name(&name)?;
                let image: PreparedImage =
                    read_json(&self.images_dir.join(format!("{name}.json")))?;
                let mut metadata = image.pooled_image.inspect()?;
                metadata["RepoTags"] = serde_json::json!([name]);
                metadata
            }
            Operation::Push { name, to } => {
                let (image, source) = push(&self.images_dir, &name, &to)?;
                serde_json::json!({
                    "source": source, "image_handle": image.cache.handle,
                    "digest": image.digest, "platform": image.platform,
                })
            }
            Operation::Pull {
                name,
                from,
                refresh,
            } => {
                let image = pull(&self.images_dir, &name, from.as_deref(), refresh)?;
                serde_json::json!({
                    "image": name, "source": image.cache.repository, "mirrors": image.cache.mirrors,
                })
            }
        };
        println!("{}", serde_json::to_string(&value)?);
        Ok(())
    }
}

pub(super) fn validate_name(name: &str) -> anyhow::Result<()> {
    pvisor::docker::Store::validate_image_key(name)
}

pub(super) fn private_directory(path: &Path) -> anyhow::Result<()> {
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

pub(super) fn lock(path: &Path) -> anyhow::Result<File> {
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
pub(super) fn image_lock(images: &Path, name: &str) -> anyhow::Result<File> {
    validate_name(name)?;
    private_directory(&images.join(".distribution"))?;
    let directory = images.join(".distribution/image-locks");
    private_directory(&directory)?;
    lock(&directory.join(format!("{name}.lock")))
}

pub(super) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
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

pub(super) fn write_json(path: &Path, value: &impl Serialize, replace: bool) -> anyhow::Result<()> {
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

fn push(
    images: &Path,
    name: &str,
    destination: &str,
) -> anyhow::Result<(pvisor::docker::Image, String)> {
    use pvisor::docker::Store;
    Store::validate_image_destination(destination)?;
    let _lock = image_lock(images, name)?;
    let image: PreparedImage = read_json(&images.join(format!("{name}.json")))?;
    let portable = serde_json::to_vec(&portable_template(&image))?;
    let source = Store::open(image.rootfs)?.publish_registry_image(
        &image.pooled_image,
        name,
        destination,
        &portable,
    )?;
    Ok((image.pooled_image, source))
}

fn pull(
    images: &Path,
    name: &str,
    source: Option<&str>,
    refresh: bool,
) -> anyhow::Result<pvisor::docker::Image> {
    use pvisor::docker::Store;
    if let Some(source) = source {
        ensure!(
            ["s3://", "http://", "https://"]
                .iter()
                .any(|scheme| source.starts_with(scheme)),
            "--from requires an S3 or HTTP(S) publication"
        );
    }
    let _lock = image_lock(images, name)?;
    let images = images.canonicalize()?;
    let manifest = images.join(format!("{name}.json"));
    ensure!(
        refresh || !manifest.try_exists()?,
        "image already exists; use --refresh"
    );
    let (bytes, mut repositories) = if let Some(source) = source {
        let repository = source.trim_end_matches('/').to_owned();
        (Store::read_image_repository(&repository)?, vec![repository])
    } else {
        let registries = crate::registries::load(&images)?;
        ensure!(
            !registries.is_empty(),
            "image absent locally; configure registries or use --from"
        );
        let resolved = Store::resolve_image_repository(name, &registries)?;
        (resolved.template, resolved.repositories)
    };
    let mut image: PreparedImage = serde_json::from_slice(&bytes)?;
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
}
