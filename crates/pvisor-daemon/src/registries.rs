//! Atomic, credential-free ordered registry configuration in the image directory.
use super::image_repository::{DEFAULT_IMAGES, lock, private_directory, read_json, write_json};
use anyhow::{Context, ensure};
use clap::{Args, Subcommand};
use pvisor::docker::{ImageRegistry, Store};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const CONFIG_FILE: &str = ".distribution/config/registries.json";

#[derive(Args)]
pub(super) struct Registry {
    /// Image directory owning the registry configuration.
    #[arg(long, global = true, default_value = DEFAULT_IMAGES)]
    images_dir: PathBuf,
    #[command(subcommand)]
    action: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// Append a trusted registry. Earlier entries have higher priority.
    Add {
        name: String,
        location: String,
        /// Supply immutable bytes only; never resolve mutable names through this CDN.
        #[arg(long)]
        download_only: bool,
    },
    /// Print ordered configuration without starting any service.
    List,
    /// Remove a registry from future resolutions; existing images remain pinned.
    Remove { name: String },
    /// Move a registry to a one-based priority position.
    Move { name: String, position: usize },
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u32,
    registries: Vec<ImageRegistry>,
}

pub(super) fn load(images: &Path) -> anyhow::Result<Vec<ImageRegistry>> {
    let config: Config = match read_json(&images.join(CONFIG_FILE)) {
        Ok(config) => config,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
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

pub(super) fn destination(images: &Path, explicit: Option<&str>) -> anyhow::Result<Option<String>> {
    if let Some(location) = explicit.filter(|s| s.contains("://")) {
        return Ok(Some(location.into()));
    }
    let registries = load(images)?;
    if let Some(name) = explicit {
        let registry = registries
            .iter()
            .find(|r| r.name == name)
            .context("unknown destination registry")?;
        ensure!(
            registry.location.starts_with("s3://"),
            "named HTTP registries are read-only"
        );
        return Ok(Some(registry.location.clone()));
    }
    if registries.is_empty() {
        return Ok(None);
    } // Preserve explicit legacy IPFS workflow.
    Ok(Some(
        registries
            .iter()
            .find(|r| r.resolve && r.location.starts_with("s3://"))
            .context("no writable registry; use --to s3:// or file:///")?
            .location
            .clone(),
    ))
}

impl Registry {
    pub(super) fn run(self) -> anyhow::Result<()> {
        let action = self.action.unwrap_or(Action::List);
        if matches!(action, Action::List) {
            println!(
                "{}",
                serde_json::to_string(&Config {
                    version: 1,
                    registries: load(&self.images_dir)?
                })?
            );
            return Ok(());
        }
        private_directory(&self.images_dir.join(".distribution"))?;
        private_directory(&self.images_dir.join(".distribution/config"))?;
        let _lock = lock(&self.images_dir.join(".distribution/registries.lock"))?;
        let mut registries = load(&self.images_dir)?;
        match action {
            Action::Add {
                name,
                location,
                download_only,
            } => registries.push(ImageRegistry {
                name,
                location: location.trim_end_matches('/').into(),
                resolve: !download_only,
            }),
            Action::Remove { name } => {
                let index = registries
                    .iter()
                    .position(|r| r.name == name)
                    .context("unknown registry")?;
                registries.remove(index);
            }
            Action::Move { name, position } => {
                ensure!(
                    position > 0 && position <= registries.len(),
                    "priority position must be in 1..=registry count"
                );
                let index = registries
                    .iter()
                    .position(|r| r.name == name)
                    .context("unknown registry")?;
                let registry = registries.remove(index);
                registries.insert(position - 1, registry);
            }
            Action::List => unreachable!(),
        }
        Store::validate_image_registries(&registries)?;
        let config = Config {
            version: 1,
            registries,
        };
        write_json(&self.images_dir.join(CONFIG_FILE), &config, true)?;
        println!("{}", serde_json::to_string(&config)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configuration_never_shadows_a_valid_image_named_registries() {
        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("registries.json");
        std::fs::write(&image, b"local prepared image").unwrap();
        private_directory(&temp.path().join(".distribution")).unwrap();
        let policy = temp.path().join(".distribution/registries.json");
        std::fs::write(&policy, b"image publication policy").unwrap();
        assert!(load(temp.path()).unwrap().is_empty());
        Registry {
            images_dir: temp.path().into(),
            action: Some(Action::Add {
                name: "origin".into(),
                location: "https://origin.example/pvisor".into(),
                download_only: false,
            }),
        }
        .run()
        .unwrap();
        assert_eq!(load(temp.path()).unwrap().len(), 1);
        assert_eq!(std::fs::read(image).unwrap(), b"local prepared image");
        assert_eq!(std::fs::read(policy).unwrap(), b"image publication policy");
    }

    #[test]
    fn configuration_is_versioned_bounded_and_strict_without_network_io() {
        let temp = tempfile::tempdir().unwrap();
        private_directory(&temp.path().join(".distribution/config")).unwrap();
        write_json(
            &temp.path().join(CONFIG_FILE),
            &serde_json::json!({
                "version": 99, "registries": []
            }),
            false,
        )
        .unwrap();
        assert!(load(temp.path()).is_err());
        write_json(&temp.path().join(CONFIG_FILE), &serde_json::json!({
            "version": 1, "registries": [
                {"name": "origin", "location": "https://origin.example/pvisor", "password": "not-a-real-secret"}
            ]
        }), true).unwrap();
        assert!(
            load(temp.path()).is_err(),
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
