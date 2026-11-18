//! Command-line adapter for prepared-catalog registry configuration.
use crate::DEFAULT_IMAGES;
use clap::{Args, Subcommand};
use pvisor::docker::ImageRegistry;
use pvisor_daemon::api::{PreparedCatalog, PreparedCatalogService, RegistryUpdate};
use std::path::PathBuf;

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

impl Registry {
    pub(super) fn run(self) -> anyhow::Result<()> {
        let catalog = PreparedCatalog::new(self.images_dir);
        let registries = match self.action.unwrap_or(Action::List) {
            Action::List => catalog.registries()?,
            Action::Add {
                name,
                location,
                download_only,
            } => catalog.update_registries(RegistryUpdate::Add(ImageRegistry {
                name,
                location,
                resolve: !download_only,
            }))?,
            Action::Remove { name } => catalog.update_registries(RegistryUpdate::Remove(name))?,
            Action::Move { name, position } => {
                catalog.update_registries(RegistryUpdate::Move { name, position })?
            }
        };
        println!(
            "{}",
            serde_json::json!({"version": 1, "registries": registries})
        );
        Ok(())
    }
}
