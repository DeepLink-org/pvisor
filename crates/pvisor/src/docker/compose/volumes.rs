//! Compose volume ownership and removal, serialized by the Docker catalog lock.
use super::*;
use std::{ffi::CString, os::unix::fs::DirBuilderExt};

// Outside the managed volume namespace, but on the same filesystem.
const STAGING_PREFIX: &str = ".compose-create-";

pub(in crate::docker) fn is_staged_name(name: &str) -> bool {
    name.strip_prefix(STAGING_PREFIX).is_some_and(valid_name)
}

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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    removing: bool,
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

fn volume_path(store: &Store, name: &str) -> PathBuf {
    store.root.join("volumes").join(name)
}

fn stage_path(store: &Store, name: &str) -> PathBuf {
    volume_path(store, &format!("{STAGING_PREFIX}{name}"))
}

fn directory(path: &Path, name: &str) -> anyhow::Result<Option<fs::Metadata>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_dir() && metadata.uid() == unsafe { libc::geteuid() },
        "volume must be an owned directory: {name}"
    );
    Ok(Some(metadata))
}

fn present(path: &Path) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

enum Location {
    Installed,
    Staged,
    Missing,
}

fn locate(store: &Store, name: &str, owner: &Owner) -> anyhow::Result<Location> {
    for (path, location) in [
        (volume_path(store, name), Location::Installed),
        (stage_path(store, name), Location::Staged),
    ] {
        if let Some(metadata) = directory(&path, name)? {
            ensure!(
                metadata.dev() == owner.device && metadata.ino() == owner.inode,
                "Compose volume directory identity changed: {name}"
            );
            return Ok(location);
        }
    }
    Ok(Location::Missing)
}

fn stage(store: &Store, name: &str, project: &str, key: &str) -> anyhow::Result<Owner> {
    let path = stage_path(store, name);
    // An unrecorded stage was never mountable; only an empty slot is reusable.
    if directory(&path, name)?.is_some() {
        fs::remove_dir(&path)
            .with_context(|| format!("discard empty Compose volume stage: {name}"))?;
    }
    fs::DirBuilder::new().mode(0o700).create(&path)?;
    let metadata = fs::symlink_metadata(&path)?;
    File::open(&path)?.sync_all()?;
    sync_volumes(store)?;
    Ok(Owner {
        project: project.into(),
        key: key.into(),
        device: metadata.dev(),
        inode: metadata.ino(),
        removing: false,
    })
}

fn install(store: &Store, name: &str) -> anyhow::Result<()> {
    let source = CString::new(stage_path(store, name).as_os_str().as_encoded_bytes())?;
    let destination = CString::new(volume_path(store, name).as_os_str().as_encoded_bytes())?;
    #[cfg(target_os = "linux")]
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let rc = unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    if rc != 0 {
        return Err(io::Error::last_os_error())
            .with_context(|| format!("install Compose volume: {name}"));
    }
    Ok(())
}

fn sync_volumes(store: &Store) -> anyhow::Result<()> {
    File::open(store.root.join("volumes"))?.sync_all()?;
    Ok(())
}

fn remove_owned(
    store: &Store,
    catalog: &mut Catalog,
    name: &str,
    location: Location,
) -> anyhow::Result<()> {
    if !matches!(location, Location::Missing) {
        let owner = catalog.volumes.get_mut(name).unwrap();
        if !owner.removing {
            // A partial recursive deletion must not look mountable on retry.
            owner.removing = true;
            save(store, catalog)?;
        } else {
            // Confirm a visible but unacknowledged intent before deleting data.
            File::open(&store.root)?.sync_all()?;
        }
        match locate(store, name, &catalog.volumes[name])? {
            Location::Installed => fs::remove_dir_all(volume_path(store, name))?,
            Location::Staged => fs::remove_dir_all(stage_path(store, name))?,
            Location::Missing => {}
        }
    }
    sync_volumes(store)?;
    catalog.volumes.remove(name);
    save(store, catalog)
}

fn ensure_unused(
    store: &Store,
    instances: &[Instance],
    name: &str,
    owned: bool,
) -> anyhow::Result<()> {
    let path = volume_path(store, name);
    let staged = owned.then(|| stage_path(store, name));
    ensure!(
        !instances.iter().any(|instance| instance
            .config
            .mounts
            .iter()
            .any(|mount| mount.source == path || staged.as_ref() == Some(&mount.source))),
        "volume is still referenced by a container: {name}"
    );
    Ok(())
}

// Caller holds the Docker catalog lock.
pub(in crate::docker) fn remove(store: &Store, name: &str) -> anyhow::Result<()> {
    let mut catalog = catalog(store)?;
    let owner = catalog.volumes.get(name);
    ensure_unused(store, &store.all()?, name, owner.is_some())?;
    if let Some(owner) = owner {
        let location = locate(store, name, owner)?;
        remove_owned(store, &mut catalog, name, location)
    } else {
        let path = volume_path(store, name);
        ensure!(directory(&path, name)?.is_some(), "No such volume: {name}");
        fs::remove_dir_all(path)?;
        Ok(())
    }
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
        let mut installable = Vec::new();
        let mut has_owned = false;
        // Check every existing authority before creating any new volume.
        for key in keys {
            let definition = self.document.volumes[key].as_ref().unwrap();
            let name = self.volume_name(key);
            let path = volume_path(store, name);
            let owner = catalog.volumes.get(name);
            if definition.external {
                ensure!(
                    owner.is_none_or(|owner| !owner.removing),
                    "Compose volume removal is incomplete: {name}"
                );
                ensure!(
                    directory(&path, name)?.is_some(),
                    "volume not found: {name}"
                );
                continue;
            }
            has_owned = true;
            if let Some(owner) = owner {
                ensure!(
                    owner.project == self.name && owner.key == key,
                    "volume belongs to another owner: {name}"
                );
                let location = locate(store, name, owner)?;
                ensure!(
                    !owner.removing,
                    "Compose volume removal is incomplete; retry volume rm {name}"
                );
                match location {
                    Location::Installed => {}
                    Location::Staged => installable.push((name, None)),
                    Location::Missing => bail!("volume not found: {name}"),
                }
            } else {
                ensure!(
                    !present(&path)?,
                    "refusing to adopt an unowned volume: {name}"
                );
                installable.push((name, Some(key)));
            }
        }
        for (name, new_key) in installable {
            if let Some(key) = new_key {
                let owner = stage(store, name, &self.name, key)?;
                catalog.volumes.insert(name.into(), owner);
                save(store, &catalog)?;
            } else {
                // The prior catalog rename may have been visible but not synced.
                File::open(&store.root)?.sync_all()?;
            }
            install(store, name)?;
        }
        if has_owned {
            sync_volumes(store)?;
        }
        Ok(())
    }

    pub(super) fn remove_volumes(&self, store: &Store) -> anyhow::Result<()> {
        let _guard = store.lock()?;
        let mut catalog = catalog(store)?;
        let mut removals = Vec::new();
        let instances = store.all()?;
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
                    !present(&volume_path(store, name))?,
                    "refusing to remove an unowned volume: {name}"
                );
                continue;
            };
            ensure!(
                owner.project == self.name && owner.key == *key,
                "volume belongs to another owner: {name}"
            );
            ensure_unused(store, &instances, name, true)?;
            removals.push((name, locate(store, name, owner)?));
        }
        for (name, location) in removals {
            remove_owned(store, &mut catalog, name, location)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod management_tests {
    use super::*;
    use rstest::rstest;

    const NAME: &str = "demo_data";

    fn project() -> (tempfile::TempDir, Store, Project) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("store")).unwrap();
        let file = directory.path().join("compose.yaml");
        fs::write(
            &file,
            "name: demo\nservices: {app: {image: alpine, volumes: ['data:/data']}}\nvolumes: {data: {}}\n",
        )
        .unwrap();
        let project = Project::load(Some(&file), None).unwrap();
        (directory, store, project)
    }

    fn ensure(store: &Store, project: &Project) -> anyhow::Result<()> {
        project.ensure_volumes(store, &["app".into()])
    }

    #[rstest]
    #[case::before_catalog_publish(false)]
    #[case::after_catalog_publish_before_install(true)]
    fn interrupted_compose_volume_create(#[case] published: bool) {
        let (_directory, store, project) = project();
        let staged = stage_path(&store, NAME);
        let owner = stage(&store, NAME, "demo", "data").unwrap();
        let expected = (owner.device, owner.inode);
        if published {
            let mut catalog = catalog(&store).unwrap();
            catalog.volumes.insert(NAME.into(), owner);
            save(&store, &catalog).unwrap();
        }
        assert!(staged.is_dir());
        assert!(!volume_path(&store, NAME).exists());
        assert!(store.volumes().unwrap().is_empty());

        ensure(&store, &project).unwrap();
        assert_eq!(store.volumes().unwrap()[0].name, NAME);
        assert!(!staged.exists());
        let metadata = fs::metadata(volume_path(&store, NAME)).unwrap();
        let catalog = catalog(&store).unwrap();
        let owner = &catalog.volumes[NAME];
        assert_eq!(
            (metadata.dev(), metadata.ino()),
            (owner.device, owner.inode)
        );
        if published {
            assert_eq!(expected, (owner.device, owner.inode));
        }
    }

    #[rstest]
    #[case::staged_inode_replaced(false)]
    #[case::public_name_occupied(true)]
    fn interrupted_compose_volume_create_refuses_collision(#[case] installed: bool) {
        let (directory, store, project) = project();
        let owner = stage(&store, NAME, "demo", "data").unwrap();
        let mut catalog = catalog(&store).unwrap();
        catalog.volumes.insert(NAME.into(), owner);
        save(&store, &catalog).unwrap();
        let staged = stage_path(&store, NAME);
        let substitute = if installed {
            volume_path(&store, NAME)
        } else {
            fs::rename(&staged, directory.path().join("original")).unwrap();
            staged.clone()
        };
        fs::create_dir(&substitute).unwrap();
        assert!(
            ensure(&store, &project)
                .unwrap_err()
                .to_string()
                .contains("identity changed")
        );
        assert!(
            store
                .remove_volume(NAME)
                .unwrap_err()
                .to_string()
                .contains("identity changed")
        );
        if installed {
            assert!(install(&store, NAME).is_err());
            assert!(staged.is_dir());
        }
        assert!(substitute.is_dir());
    }

    #[rstest]
    fn unpublished_nonempty_stage_is_not_adopted_or_removed() {
        let (_directory, store, project) = project();
        stage(&store, NAME, "demo", "data").unwrap();
        let unknown_data = stage_path(&store, NAME).join("keep");
        fs::write(&unknown_data, b"not authorized").unwrap();
        assert!(ensure(&store, &project).is_err());
        assert_eq!(fs::read(unknown_data).unwrap(), b"not authorized");
        assert!(!volume_path(&store, NAME).exists());
        assert!(!catalog(&store).unwrap().volumes.contains_key(NAME));
    }

    #[rstest]
    #[case::partially_deleted(true, false, false)]
    #[case::deleted_before_catalog_cleanup(true, true, false)]
    #[case::legacy_absent_catalog_entry(false, true, false)]
    #[case::compose_down_completes_partial_delete(true, false, true)]
    fn interrupted_compose_volume_removal(
        #[case] removing: bool,
        #[case] missing: bool,
        #[case] compose_down: bool,
    ) {
        let (_directory, store, project) = project();
        let path = volume_path(&store, NAME);
        ensure(&store, &project).unwrap();
        let mut ownership = catalog(&store).unwrap();
        ownership.volumes.get_mut(NAME).unwrap().removing = removing;
        save(&store, &ownership).unwrap();
        if missing {
            fs::remove_dir(&path).unwrap();
        } else {
            fs::write(path.join("remaining"), b"partial deletion").unwrap();
        }

        let error = ensure(&store, &project).unwrap_err().to_string();
        assert!(error.contains(if removing {
            "removal is incomplete"
        } else {
            "volume not found"
        }));
        if compose_down {
            project.remove_volumes(&store).unwrap();
        } else {
            store.remove_volume(NAME).unwrap();
        }
        assert!(!path.exists());
        assert!(!catalog(&store).unwrap().volumes.contains_key(NAME));
        assert!(
            store.remove_volume(NAME).is_err(),
            "unknown absence is not ownership"
        );
        ensure(&store, &project).unwrap();
        assert!(path.is_dir());
    }

    #[rstest]
    fn explicit_volume_removal_reconciles_compose_ownership_and_refuses_substitution() {
        let (directory, store, project) = project();
        ensure(&store, &project).unwrap();
        store.remove_volume(NAME).unwrap();
        assert!(!catalog(&store).unwrap().volumes.contains_key(NAME));
        ensure(&store, &project).unwrap();
        let mut ownership = catalog(&store).unwrap();
        ownership.volumes.get_mut(NAME).unwrap().removing = true;
        save(&store, &ownership).unwrap();
        let path = volume_path(&store, NAME);
        fs::rename(&path, directory.path().join("original")).unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), b"replacement").unwrap();
        assert!(store.remove_volume(NAME).is_err());
        assert!(project.remove_volumes(&store).is_err());
        assert_eq!(fs::read(path.join("keep")).unwrap(), b"replacement");
        assert!(directory.path().join("original").is_dir());
    }
}
