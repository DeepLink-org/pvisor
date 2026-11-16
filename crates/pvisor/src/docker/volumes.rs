//! Public managed-volume discovery and removal; no access through private DTOs.
use super::*;

/// An owned local managed volume. Its name, not its backing path, is the public
/// selector. Both anonymous and named volumes are discoverable.
#[derive(Clone, Debug, Serialize)]
pub struct Volume {
    /// Safe volume name, without the internal anonymous binding prefix.
    pub name: String,
    /// Local driver; nonlocal driver options are unsupported.
    pub driver: String,
}

impl Store {
    /// Create a named local volume or return an existing owned directory.
    /// Omission generates a unique name. This does not initialize image content;
    /// first writable container use does. Symlinks/foreign owners are refused.
    pub fn create_volume(&self, name: Option<&str>) -> anyhow::Result<Volume> {
        let _guard = self.lock()?;
        let name = name.map(str::to_owned).unwrap_or_else(uuid);
        self.volume(&name)?;
        Ok(Volume {
            name,
            driver: "local".into(),
        })
    }

    /// List valid owned volumes without creating them. Corrupt/foreign entries
    /// fail rather than being hidden or adopted.
    pub fn volumes(&self) -> anyhow::Result<Vec<Volume>> {
        let _guard = self.lock()?;
        let mut volumes = vec![];
        for entry in fs::read_dir(self.root.join("volumes"))? {
            let name = entry?
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("invalid volume name"))?;
            volumes.push(self.volume_record(&name)?);
        }
        volumes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(volumes)
    }

    fn volume_record(&self, name: &str) -> anyhow::Result<Volume> {
        ensure!(valid_name(name), "invalid volume name");
        let metadata = fs::symlink_metadata(self.root.join("volumes").join(name))
            .with_context(|| format!("No such volume: {name}"))?;
        ensure!(
            metadata.is_dir() && metadata.uid() == unsafe { libc::geteuid() },
            "volume must be an owned directory: {name}"
        );
        Ok(Volume {
            name: name.into(),
            driver: "local".into(),
        })
    }

    /// Inspect an existing local volume. Never creates it or exposes private
    /// backing/control files. Existence is not permission for Compose adoption.
    pub fn inspect_volume(&self, name: &str) -> anyhow::Result<Volume> {
        let _guard = self.lock()?;
        self.volume_record(name)
    }

    /// Remove an unused volume under the catalog lock, including its data.
    /// References from stopped containers also block deletion. Compose ownership
    /// is reconciled only after identity validation and successful deletion;
    /// directory substitution/symlinks are refused. No force bypass is provided.
    pub fn remove_volume(&self, name: &str) -> anyhow::Result<()> {
        let _guard = self.lock()?;
        self.volume_record(name)?;
        let path = self.root.join("volumes").join(name);
        ensure!(
            !self.all()?.iter().any(|instance| instance
                .config
                .mounts
                .iter()
                .any(|mount| mount.source == path)),
            "volume is still referenced by a container: {name}"
        );
        compose::volumes::remove_unused(self, name)
    }
}
