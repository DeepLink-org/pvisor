//! Docker image bindings to the shared cache's immutable local content pool.
use super::*;
use crate::image::{
    cache,
    oci::{ImageStore, PreparedImage},
};
use pvisor_journal::api::{DurableFiles, Persistence};

impl Store {
    pub(super) fn image_pull_lock(
        &self,
        reference: &str,
        architecture: &str,
    ) -> anyhow::Result<File> {
        let directory = self.root.join("images/pull-locks");
        private_directory(&directory)?;
        let (reference, _) = crate::image::oci::cache_reference(reference)?;
        use sha2::{Digest, Sha256};
        let key = crate::util::encode_hex(&Sha256::digest(serde_json::to_vec(&(
            reference,
            architecture,
        ))?));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(directory.join(key))?;
        file.lock_exclusive()?;
        Ok(file)
    }
    /// Open a pinned image without registry access. Pool metadata/content errors
    /// fail rather than falling back to an unverified directory or mutable tag.
    /// Keep the returned owner until OverlayCore and VM consumers have stopped.
    /// Foreign images may be inspected; VM admission still requires native CPU.
    pub fn open_image(&self, image: &Image) -> anyhow::Result<ImageRoot> {
        let Some(cached) = &image.cache else {
            ensure!(image.rootfs.is_dir(), "legacy image rootfs is missing");
            return Ok(ImageRoot {
                root: image.rootfs.clone(),
                _owner: None,
            });
        };
        let scratch = self.root.join("images/projections");
        private_directory(&scratch)?;
        let owner = cache::open_local_image(
            &cached.location,
            &cached.handle,
            canonical_platform(&image.platform)?
                .strip_prefix("linux/")
                .unwrap(),
            &image.digest,
            &scratch,
        )?;
        Ok(ImageRoot {
            root: owner.root().into(),
            _owner: Some(owner),
        })
    }

    pub(super) fn pool_image(
        &self,
        image: &mut Image,
        source: &ImageStore,
        config: &[u8],
    ) -> anyhow::Result<()> {
        let hex = crate::image::oci::digest_hex(&image.id)?;
        use sha2::{Digest, Sha256};
        ensure!(
            crate::util::encode_hex(&Sha256::digest(config)) == hex,
            "image config digest mismatch"
        );
        let configs = self.root.join("images/configs");
        private_directory(&configs)?;
        Persistence::atomic_write(&configs.join(format!("{hex}.json")), config, 0o600)?;
        let location = self.root.join("images/pool-v1");
        private_directory(&location)?;
        let prepared = PreparedImage {
            rootfs: image.rootfs.clone(),
            digest: image.digest.clone(),
            env: image.env.clone(),
            entrypoint: image.entrypoint.clone(),
            cmd: image.cmd.clone(),
        };
        let handle = cache::publish_local_image(
            &location,
            source,
            &prepared,
            &image.reference,
            canonical_platform(&image.platform)?
                .strip_prefix("linux/")
                .unwrap(),
        )?;
        image.cache = Some(CachedImage { location, handle });
        image.rootfs = PathBuf::new();
        Ok(())
    }

    pub(super) fn image_config_path(&self, image: &Image) -> anyhow::Result<PathBuf> {
        if let Some(cache) = &image.cache {
            return Ok(cache
                .location
                .parent()
                .context("pool has no owner directory")?
                .join("configs")
                .join(format!(
                    "{}.json",
                    crate::image::oci::digest_hex(&image.id)?
                )));
        }
        Ok(
            if image
                .rootfs
                .parent()
                .is_some_and(|p| p.join("blobs").exists())
            {
                image
                    .rootfs
                    .parent()
                    .unwrap()
                    .join("blobs/sha256")
                    .join(&image.id[7..])
            } else {
                self.root.join("images/blobs/sha256").join(&image.id[7..])
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::build::{BuildOptions, build};
    use super::*;
    use pvisor_overlay_core::backend;

    fn options(context: &Path, tag: &str) -> BuildOptions {
        BuildOptions {
            context: context.into(),
            file: context.join("Dockerfile"),
            tags: vec![tag.into()],
            build_args: BTreeMap::new(),
            target: None,
            platform: None,
            pull: false,
            cpus: 1,
            memory_mib: 32,
            network: false,
            timeout: Duration::from_secs(30),
            quiet: true,
            output: None,
        }
    }
    fn files(path: &Path) -> Vec<PathBuf> {
        fs::read_dir(path)
            .unwrap()
            .flat_map(|entry| {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    files(&path)
                } else {
                    vec![path]
                }
            })
            .collect()
    }
    fn create(image: &str) -> Create {
        Create {
            name: None,
            image: image.into(),
            platform: None,
            command: vec![],
            entrypoint: None,
            env: BTreeMap::new(),
            unset_env: vec![],
            cwd: None,
            user: None,
            mounts: vec![],
            ports: vec![],
            cpus: 1,
            memory_mib: 32,
            memory_pool: None,
            shared_kernel_base: None,
            auto_remove: false,
            interactive: false,
            tty: None,
            network: false,
            network_name: None,
            ipv4_address: None,
            peers: None,
        }
    }

    #[test]
    fn default_pool_deduplicates_builds_and_retains_stopped_container_revision() {
        let temporary = tempfile::tempdir().unwrap();
        let context = temporary.path().join("context");
        fs::create_dir(&context).unwrap();
        fs::write(context.join("payload"), vec![71; 1024 * 1024 + 29]).unwrap();
        let store = Store::open(temporary.path().join("docker")).unwrap();
        let opts = options(&context, "app:test");
        fs::write(
            context.join("Dockerfile"),
            "FROM scratch\nCOPY payload /payload\nENV VERSION=first\nCMD [\"/payload\"]\n",
        )
        .unwrap();
        let first = build(&store, &opts).unwrap();
        let container = store.create(create("app:test")).unwrap();
        fs::write(
            context.join("Dockerfile"),
            "FROM scratch\nCOPY payload /renamed\nENV VERSION=second\nCMD [\"/renamed\"]\n",
        )
        .unwrap();
        let second = build(&store, &opts).unwrap();
        assert_ne!(first.id, second.id);
        assert!(first.rootfs.as_os_str().is_empty() && second.rootfs.as_os_str().is_empty());
        let objects = files(&store.root.join("images/pool-v1/data"));
        assert_eq!(
            objects.len(),
            2,
            "path and config changes reuse the two unique chunks"
        );
        assert!(
            objects
                .iter()
                .map(|path| fs::metadata(path).unwrap().len())
                .sum::<u64>()
                < 1024
        );
        for name in ["blobs", "rootfs-v3", "built"] {
            assert!(!store.root.join("images").join(name).exists());
        }
        assert!(
            fs::read_dir(store.root.join("images"))
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".import-"))
        );
        store.rmi("app:test", false).unwrap();
        let reopened = Store::open(store.root.clone()).unwrap();
        let pinned = reopened.resolve(&container.id).unwrap();
        assert_eq!(pinned.image.id, first.id);
        assert_eq!(pinned.image.env["VERSION"], "first");
        let root = reopened.open_image(&pinned.image).unwrap();
        assert_eq!(
            backend::read_at(&root.rootfs().join("payload"), 1024 * 1024 - 1, 3)
                .unwrap()
                .unwrap(),
            vec![71; 3]
        );
        assert!(backend::attributes(&root.rootfs().join("renamed")).is_err());
    }

    #[test]
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn pooled_images_accept_memory_sharing_and_check_projected_root_authority() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let context = temporary.path().join("context");
        fs::create_dir(&context).unwrap();
        fs::write(
            context.join("Dockerfile"),
            "FROM scratch\nCMD [\"/payload\"]\n",
        )
        .unwrap();
        let store = Store::open(temporary.path().join("docker")).unwrap();
        let image = build(&store, &options(&context, "sharing:test")).unwrap();
        assert!(image.rootfs.as_os_str().is_empty());
        let private = temporary.path().join("pool");
        fs::create_dir(&private).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = private.join("pool.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let mut config = create("sharing:test");
        config.memory_pool = Some(socket.canonicalize().unwrap());
        config.shared_kernel_base = Some(private.canonicalize().unwrap());
        let instance = store.create(config.clone()).unwrap();
        assert_eq!(instance.config.memory_pool, config.memory_pool);
        assert_eq!(
            instance.config.shared_kernel_base,
            config.shared_kernel_base
        );

        // Projections are guest-visible even though the stored rootfs field is empty.
        let projections = store.root.join("images/projections");
        config.shared_kernel_base = Some(projections.canonicalize().unwrap());
        assert!(
            store
                .create(config.clone())
                .unwrap_err()
                .to_string()
                .contains("shared kernel authority")
        );
        config.shared_kernel_base = None;
        let exposed_socket = projections.join("pool.sock");
        let _exposed = std::os::unix::net::UnixListener::bind(&exposed_socket).unwrap();
        config.memory_pool = Some(exposed_socket);
        assert!(
            store
                .create(config)
                .unwrap_err()
                .to_string()
                .contains("memory pool authority")
        );
    }

    #[test]
    fn pooled_build_base_preserves_bytes_guest_identity_and_configuration() {
        let temporary = tempfile::tempdir().unwrap();
        let context = temporary.path().join("context");
        fs::create_dir(&context).unwrap();
        fs::write(
            context.join("payload"),
            b"actual content, never sparse zeros",
        )
        .unwrap();
        fs::write(context.join("Dockerfile"), "FROM scratch\nCOPY --chown=123:456 --chmod=4750 payload /payload\nSHELL [\"/custom-shell\",\"-c\"]\nLABEL owner=retained\nCMD [\"/payload\"]\n").unwrap();
        let store = Store::open(temporary.path().join("docker")).unwrap();
        build(&store, &options(&context, "base:test")).unwrap();
        fs::write(
            context.join("Dockerfile"),
            "FROM base:test\nENV VERSION=derived\n",
        )
        .unwrap();
        let derived = build(&store, &options(&context, "derived:test")).unwrap();
        let root = store.open_image(&derived).unwrap();
        let path = root.rootfs().join("payload");
        assert_eq!(
            backend::read_at(&path, 0, 128).unwrap().unwrap(),
            b"actual content, never sparse zeros"
        );
        let attr = backend::attributes(&path).unwrap().unwrap();
        assert_eq!((attr.uid, attr.gid, attr.perm), (123, 456, 0o4750));
        let config: serde_json::Value =
            serde_json::from_slice(&fs::read(store.image_config_path(&derived).unwrap()).unwrap())
                .unwrap();
        assert_eq!(config["config"]["Labels"]["owner"], "retained");
        assert_eq!(config["config"]["Shell"][0], "/custom-shell");
        assert_eq!(derived.cmd, ["/payload"]);
        let resolver = temporary.path().join("resolver");
        prepare_resolver(root.rootfs(), &resolver, pvisor_overlaynet::vm::ROUTER_IPV4).unwrap();
        assert_eq!(
            fs::read(resolver.join("etc/resolv.conf")).unwrap(),
            b"nameserver 192.0.2.1\n"
        );
    }
}
