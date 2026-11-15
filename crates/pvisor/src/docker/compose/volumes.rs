//! Compose volume ownership and removal, serialized by the Docker catalog lock.
use super::*;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition {
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    external: bool,
    #[serde(default)]
    driver: Option<String>,
}

impl Definition {
    pub(super) fn resolve(&mut self, project: &str, key: &str) -> anyhow::Result<()> {
        ensure!(valid_name(key), "invalid volume key: {key}");
        ensure!(
            self.driver
                .as_deref()
                .is_none_or(|driver| driver == "local"),
            "only local volumes are supported"
        );
        ensure!(
            !self.external || self.driver.is_none(),
            "external volumes cannot specify a driver"
        );
        let name = self.name.get_or_insert_with(|| {
            if self.external {
                key.into()
            } else {
                format!("{project}_{key}")
            }
        });
        ensure!(valid_name(name), "invalid volume name: {name}");
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    schema: u32,
    volumes: BTreeMap<String, Owner>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    project: String,
    key: String,
    device: u64,
    inode: u64,
}

fn catalog(store: &Store) -> anyhow::Result<Catalog> {
    let path = store.root.join("compose-volumes.json");
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Catalog {
                schema: 1,
                volumes: BTreeMap::new(),
            });
        }
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0
            && metadata.nlink() == 1
            && metadata.len() <= 1024 * 1024,
        "unsafe Compose volume catalog"
    );
    let catalog: Catalog = serde_json::from_reader(file.take(1024 * 1024 + 1))?;
    ensure!(catalog.schema == 1, "unknown Compose volume catalog");
    for (name, owner) in &catalog.volumes {
        ensure!(
            valid_name(name) && project_name(&owner.project) && valid_name(&owner.key),
            "invalid Compose volume ownership"
        );
    }
    Ok(catalog)
}

fn save(store: &Store, catalog: &Catalog) -> anyhow::Result<()> {
    crate::util::write_private_json(&store.root.join("compose-volumes.json"), catalog)
}

fn existing(store: &Store, name: &str) -> anyhow::Result<PathBuf> {
    let path = store.root.join("volumes").join(name);
    let metadata =
        fs::symlink_metadata(&path).with_context(|| format!("volume not found: {name}"))?;
    ensure!(
        metadata.is_dir() && metadata.uid() == unsafe { libc::geteuid() },
        "volume must be an owned directory: {name}"
    );
    Ok(path)
}

fn present(path: &Path) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn owned(store: &Store, name: &str, owner: &Owner) -> anyhow::Result<PathBuf> {
    let path = existing(store, name)?;
    let metadata = fs::symlink_metadata(&path)?;
    ensure!(
        metadata.dev() == owner.device && metadata.ino() == owner.inode,
        "Compose volume directory identity changed: {name}"
    );
    Ok(path)
}

impl Project {
    pub(super) fn volume_name(&self, key: &str) -> &str {
        self.document.volumes[key]
            .as_ref()
            .unwrap()
            .name
            .as_deref()
            .unwrap()
    }

    pub(super) fn ensure_volumes(&self, store: &Store, services: &[String]) -> anyhow::Result<()> {
        let keys: BTreeSet<_> = services
            .iter()
            .flat_map(|service| self.document.services[service].volumes.iter())
            .filter_map(|mount| {
                let source = mount.split(':').next().unwrap();
                (!source.starts_with('/')).then_some(source)
            })
            .collect();
        let _guard = store.lock()?;
        let mut catalog = catalog(store)?;
        // Check every existing authority before creating any new volume.
        for key in &keys {
            let definition = self.document.volumes[*key].as_ref().unwrap();
            let name = self.volume_name(key);
            if definition.external {
                existing(store, name)?;
            } else if let Some(owner) = catalog.volumes.get(name) {
                ensure!(
                    owner.project == self.name && owner.key == *key,
                    "volume belongs to another owner: {name}"
                );
                owned(store, name, owner)?;
            } else {
                ensure!(
                    !present(&store.root.join("volumes").join(name))?,
                    "refusing to adopt an unowned volume: {name}"
                );
            }
        }
        for key in keys {
            let definition = self.document.volumes[key].as_ref().unwrap();
            let name = self.volume_name(key);
            if !definition.external && !catalog.volumes.contains_key(name) {
                let path = store.volume(name)?;
                let metadata = fs::symlink_metadata(path)?;
                catalog.volumes.insert(
                    name.into(),
                    Owner {
                        project: self.name.clone(),
                        key: key.into(),
                        device: metadata.dev(),
                        inode: metadata.ino(),
                    },
                );
                // A failed publication retains the directory but grants no ownership.
                save(store, &catalog)?;
            }
        }
        Ok(())
    }

    pub(super) fn remove_volumes(&self, store: &Store) -> anyhow::Result<()> {
        let _guard = store.lock()?;
        let mut catalog = catalog(store)?;
        let mut names = Vec::new();
        // The current file supplies deletion scope; the catalog only proves
        // ownership. Historical project membership is not deletion authority.
        for (key, definition) in &self.document.volumes {
            let definition = definition.as_ref().unwrap();
            if definition.external {
                continue;
            }
            let name = self.volume_name(key);
            let Some(owner) = catalog.volumes.get(name) else {
                ensure!(
                    !present(&store.root.join("volumes").join(name))?,
                    "refusing to remove an unowned volume: {name}"
                );
                continue;
            };
            ensure!(
                owner.project == self.name && owner.key == *key,
                "volume belongs to another owner: {name}"
            );
            names.push(name.to_owned());
        }
        let instances = store.all()?;
        for name in &names {
            let path = store.root.join("volumes").join(name);
            ensure!(
                !instances.iter().any(|instance| instance
                    .config
                    .mounts
                    .iter()
                    .any(|mount| mount.source == path)),
                "volume is still referenced by a container: {name}"
            );
            if present(&path)? {
                owned(store, name, &catalog.volumes[name])?;
            }
        }
        for name in names {
            let path = store.root.join("volumes").join(&name);
            if present(&path)? {
                fs::remove_dir_all(path)?;
            }
            catalog.volumes.remove(&name);
            save(store, &catalog)?;
        }
        Ok(())
    }
}
