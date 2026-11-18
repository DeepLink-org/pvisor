//! Docker image bindings to the shared cache's immutable local content pool.
use super::*;
use crate::image::{
    cache,
    oci::{ImageStore, PreparedImage},
};

impl Store {
    /// Validate an explicit S3 or absolute file publication destination without I/O.
    /// Credentials, query strings, fragments and writable HTTP destinations are rejected.
    ///
    /// ```
    /// use pvisor::docker::Store;
    /// assert!(Store::validate_image_destination("s3://images/releases").is_ok());
    /// assert!(Store::validate_image_destination("file:///tmp/releases").is_ok());
    /// assert!(Store::validate_image_destination("https://example.com/images").is_err());
    /// assert!(Store::validate_image_destination("file://relative").is_err());
    /// ```
    pub fn validate_image_destination(destination: &str) -> anyhow::Result<()> {
        cache::validate_destination(destination)
    }

    /// Publish a snapshot of an existing local tag or unambiguous image ID.
    /// Never pulls OCI or changes the source catalog. Uses the recorded platform;
    /// later tag changes cannot change this publication. See
    /// [`Self::publish_image_repository`] for destination and failure semantics.
    /// Returns JSON containing `source`, `image_handle`, `digest` and `platform`.
    pub fn push_image(
        &self,
        selector: &str,
        destination: &str,
    ) -> anyhow::Result<serde_json::Value> {
        Self::validate_image_destination(destination)?;
        let image = self.local_image(selector)?.context("No such local image")?;
        let mut portable = image.clone();
        portable.cache.location = "/pvisor/store/images/pool-v2".into();
        portable.cache.repository = None;
        portable.cache.mirrors.clear();
        let source =
            self.publish_image_repository(&image, destination, &serde_json::to_vec(&portable)?)?;
        Ok(serde_json::json!({
            "source": source, "image_handle": image.cache.handle,
            "digest": image.digest, "platform": image.platform,
        }))
    }

    /// Import an ordinary image from a fixed S3/HTTP(S) publication, fetching only
    /// its descriptor, OCI configuration and V2 root metadata. Host paths are
    /// rebound to this store. A digest-qualified name must match the published
    /// manifest. Existing names require `refresh`; failures leave
    /// the catalog unchanged, though verified metadata may remain cached.
    /// The publisher is trusted for launch defaults. No mutable name resolution,
    /// OCI fallback, image data download or remote writes occur.
    pub fn pull_image_repository(
        &self,
        reference: &str,
        source: &str,
        refresh: bool,
    ) -> anyhow::Result<Image> {
        let (key, pinned) = crate::image::oci::cache_reference(reference)?;
        let _pull = self.image_pull_lock(reference, "repository")?;
        {
            let _guard = self.lock()?;
            ensure!(
                refresh || !self.catalog()?.contains_key(&key),
                "image already exists; use --refresh"
            );
        }
        let mut image: Image = serde_json::from_slice(&Self::read_image_repository(source)?)
            .context("publication is not an ordinary image")?;
        if pinned {
            let (_, expected) = key.rsplit_once('@').context("missing manifest digest")?;
            ensure!(
                image.digest == expected,
                "publication manifest digest mismatch: expected {expected}, got {}",
                image.digest
            );
        }
        image.reference = reference.into();
        image.cache.location = self.images.pool();
        image.cache.repository = Some(source.trim_end_matches('/').into());
        image.cache.mirrors.clear();
        ensure!(
            self.import_image_config(&image)?,
            "publication has no OCI configuration"
        );
        drop(self.open_image(&image)?);
        let _guard = self.lock()?;
        let mut images = self.catalog()?;
        ensure!(
            refresh || !images.contains_key(&key),
            "image already exists; use --refresh"
        );
        images.insert(key, image.clone());
        self.save_catalog(&images)?;
        Ok(image)
    }

    /// Retain a remote image's original OCI configuration after checking its
    /// digest and bounded JSON. Returns false only when the asset is absent
    /// (older prepared publications lack it); corruption and transport errors
    /// fail. Does not install an image reference or fetch image data. The image
    /// must already be rebound to this store; no publisher-supplied paths are used.
    pub fn import_image_config(&self, image: &Image) -> anyhow::Result<bool> {
        let source = image
            .cache
            .repository
            .as_ref()
            .context("image has no remote repository")?;
        let repositories = std::iter::once(source.clone())
            .chain(image.cache.mirrors.clone())
            .collect::<Vec<_>>();
        let Some(bytes) = cache::read_repository_config(&repositories, &image.id)? else {
            return Ok(false);
        };
        self.images.retain_config(&image.id, &bytes)?;
        Ok(true)
    }

    /// Validate prepared-image/registry keys without filesystem or network I/O.
    /// Accepts 1..=128 ASCII alphanumeric/`-_.` characters, not leading `.`.
    pub fn validate_image_key(name: &str) -> anyhow::Result<()> {
        cache::validate_registry_name(name)
    }

    /// Validate ordered registry configuration without network I/O. At most
    /// eight entries, unique names/locations, S3 or credential-free HTTP(S).
    pub fn validate_image_registries(registries: &[ImageRegistry]) -> anyhow::Result<()> {
        cache::validate_registries(registries)
    }

    /// Resolve a mutable name in configured order, then fetch the authenticated
    /// template only from mirrors of that exact publication. Download-only
    /// entries cannot resolve names. No name fallback after selecting a version.
    /// Each source gets at most five seconds, each resolution/read stage thirty
    /// seconds total. Nothing is installed or downloaded in full by this call.
    pub fn resolve_image_repository(
        name: &str,
        registries: &[ImageRegistry],
    ) -> anyhow::Result<ResolvedImageRepository> {
        cache::resolve_repository(name, registries)
    }

    /// Observe `refs/NAME.json` before publishing a complete immutable bundle,
    /// then update the reference conditionally. Conflicting updates fail rather than overwrite
    /// an unobserved winner; uncertain acknowledgements are read back. Failure
    /// can leave an unreferenced bundle. Returns its immutable repository root.
    pub fn publish_registry_image(
        &self,
        image: &Image,
        name: &str,
        destination: &str,
        template: &[u8],
    ) -> anyhow::Result<String> {
        let reference = cache::PublicationReference::observe(destination, name)?;
        let location = self.publish_image_repository(image, destination, template)?;
        reference.commit(&location)?;
        Ok(location)
    }

    /// Publish a local pinned image to S3 or a `file:///ABSOLUTE/PATH` static
    /// mirror staging directory. `template`
    /// is the caller's portable launch JSON, at most 1 MiB, and must not contain
    /// credentials. Returns the immutable root named by its SHA-256. Uploads
    /// dependencies sequentially, then writes `image.json` as the visibility
    /// point. Existing objects must match exactly; no overwrites are allowed.
    /// Retries reconcile uncertain writes. Failure may leave unreferenced objects
    /// but never installs a partial local image. Credentials/endpoint/region are
    /// taken only from host AWS environment configuration.
    pub fn publish_image_repository(
        &self,
        image: &Image,
        destination: &str,
        template: &[u8],
    ) -> anyhow::Result<String> {
        cache::validate_destination(destination)?;
        let staging = tempfile::tempdir()?;
        let pool = staging.path().join("pool");
        self.export_image_pool(image, &pool)?;
        cache::publish_repository(&pool, destination, template)
    }

    /// Fetch a bounded portable `image.json` from an S3 or HTTP(S) repository.
    /// This does not fetch image contents or install a record.
    /// The caller must trust the selected publisher, deserialize/validate its
    /// launch contract, and pin its handle before VM access. Hash-named roots
    /// authenticate the template identity; transport failures remain errors.
    pub fn read_image_repository(location: &str) -> anyhow::Result<Vec<u8>> {
        cache::read_repository_template(location)
    }

    /// Export a local pinned V2 image into a new directory for explicit public
    /// distribution. Copies only original OCI configuration, revision metadata
    /// and its dependency inventory,
    /// not other revisions, catalog tags, credentials or writable projections.
    /// Rejects packs retaining slots not reachable from this revision, so a
    /// public export cannot disclose files removed from a private predecessor.
    /// The destination must not exist; failure may leave a partial export that
    /// the caller must discard. Remote bindings cannot be re-exported.
    pub fn export_image_pool(&self, image: &Image, destination: &Path) -> anyhow::Result<()> {
        ensure!(
            image.cache.repository.is_none(),
            "export requires a local pool"
        );
        // Validate the DTO's platform/digest against its pinned revision before
        // publishing a template that could not subsequently be opened.
        let _owner = self.open_image(image)?;
        let config = fs::read(self.image_config_path(image)?)?;
        ensure!(
            config.len() <= 1024 * 1024,
            "image configuration exceeds publication limit"
        );
        crate::image::oci::verify_digest(&image.id, &config)?;
        crate::image::cache::export_pool(&image.cache.location, &image.cache.handle, destination)?;
        let configs = destination.join("configs");
        fs::create_dir(&configs)?;
        fs::write(
            configs.join(format!(
                "{}.json",
                crate::image::oci::digest_hex(&image.id)?
            )),
            config,
        )?;
        Ok(())
    }

    pub(super) fn image_pull_lock(
        &self,
        reference: &str,
        architecture: &str,
    ) -> anyhow::Result<File> {
        self.images.pull_lock(reference, architecture)
    }
    /// Open a pinned image without registry access. Pool metadata/content errors
    /// fail rather than falling back to an unverified directory or mutable tag.
    /// Keep the returned owner until OverlayCore and VM consumers have stopped.
    /// Foreign images may be inspected; VM admission still requires native CPU.
    pub fn open_image(&self, image: &Image) -> anyhow::Result<ImageRoot> {
        self.open_cached(&image.cache, &image.digest, &image.platform)
    }

    pub(super) fn open_cached(
        &self,
        cached: &CachedImage,
        digest: &str,
        platform: &str,
    ) -> anyhow::Result<ImageRoot> {
        let scratch = self.images.projections();
        private_directory(&scratch)?;
        let owner = cache::open_local_image(
            &cached.location,
            cached.repository.as_deref(),
            &cached.mirrors,
            &cached.handle,
            canonical_platform(platform)?
                .strip_prefix("linux/")
                .unwrap(),
            digest,
            &scratch,
        )?;
        Ok(ImageRoot {
            root: owner.root().into(),
            _owner: Some(owner),
        })
    }

    pub(super) fn pool_image<Backing>(
        &self,
        image: Image<Backing>,
        rootfs: &Path,
        source: &ImageStore,
        config: &[u8],
    ) -> anyhow::Result<Image> {
        self.images.retain_config(&image.id, config)?;
        let location = self.images.writable_pool()?;
        let prepared = PreparedImage {
            rootfs: rootfs.into(),
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
        Ok(image.map_cache(|_| CachedImage {
            location,
            handle,
            repository: None,
            mirrors: vec![],
        }))
    }

    pub(super) fn image_config_path(&self, image: &Image) -> anyhow::Result<PathBuf> {
        image_config_path(image)
    }
}

pub(super) fn image_config_path(image: &Image) -> anyhow::Result<PathBuf> {
    Ok(image
        .cache
        .location
        .parent()
        .context("pool has no owner directory")?
        .join("configs")
        .join(format!(
            "{}.json",
            crate::image::oci::digest_hex(&image.id)?
        )))
}

#[cfg(test)]
mod tests {
    use super::super::build::{BuildOptions, build};
    use super::*;

    use pvisor_overlay_core::backend;

    #[test]
    fn retired_ipfs_field_cannot_decode_as_a_local_image() {
        let mut record = serde_json::json!({
            "location": "/tmp/image-pool",
            "handle": "pinned"
        });
        assert!(
            serde_json::from_value::<CachedImage>(record.clone())
                .unwrap()
                .repository
                .is_none()
        );
        record["ipfs"] = "ipfs+http://127.0.0.1:8080/ipfs/bafyobsolete".into();
        assert!(serde_json::from_value::<CachedImage>(record).is_err());
    }

    fn options(context: &Path, tag: &str) -> BuildOptions {
        BuildOptions {
            context: context.into(),
            file: context.join("Dockerfile"),
            tags: vec![tag.into()],
            build_args: BTreeMap::new(),
            target: None,
            platform: None,
            pull: false,
            no_cache: false,
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
            labels: BTreeMap::new(),
            restart: RestartPolicy::No,
            stop_signal: None,
            stop_timeout_ms: 10_000,
            healthcheck: None,
            no_healthcheck: false,
            branchable: false,
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
        assert!(
            serde_json::to_value(&first)
                .unwrap()
                .get("rootfs")
                .is_none()
        );
        let objects = files(&store.root.join("images/pool-v2/data"));
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
    fn canonical_aliases_replace_one_tag_and_keep_containers_and_other_tags_pinned() {
        let temporary = tempfile::tempdir().unwrap();
        let context = temporary.path().join("context");
        fs::create_dir(&context).unwrap();
        let store = Store::open(temporary.path().join("docker")).unwrap();
        fs::write(
            context.join("Dockerfile"),
            "FROM scratch\nENV VERSION=first\nCMD [\"/first\"]\n",
        )
        .unwrap();
        let mut opts = options(&context, "app:test");
        opts.tags
            .extend(["docker.io/library/app:test".into(), "app:other".into()]);
        let first = build(&store, &opts).unwrap();
        assert_eq!(store.images().unwrap().len(), 2);
        let pinned = store
            .create(create("index.docker.io/library/app:test"))
            .unwrap();
        fs::write(
            context.join("Dockerfile"),
            "FROM scratch\nENV VERSION=second\nCMD [\"/second\"]\n",
        )
        .unwrap();
        let second = build(
            &store,
            &options(&context, "registry-1.docker.io/library/app:test"),
        )
        .unwrap();
        assert_ne!(first.id, second.id);
        for alias in [
            "app:test",
            "library/app:test",
            "docker.io/library/app:test",
            "index.docker.io/library/app:test",
            "registry-1.docker.io/library/app:test",
        ] {
            let current = store.create(create(alias)).unwrap();
            assert_eq!(current.image.id, second.id, "{alias}");
            assert_eq!(current.image.env["VERSION"], "second");
            store.remove(&current.id, false).unwrap();
        }
        assert_eq!(
            store.local_image("app:other").unwrap().unwrap().id,
            first.id
        );
        assert_eq!(store.resolve(&pinned.id).unwrap().image.id, first.id);
        store.rmi("docker.io/library/app:test", false).unwrap();
        assert!(store.local_image("app:test").unwrap().is_none());
        assert_eq!(store.images().unwrap().len(), 1);
        assert_eq!(
            store.resolve(&pinned.id).unwrap().image.env["VERSION"],
            "first"
        );
    }

    #[test]
    fn conflicting_canonical_catalog_and_ambiguous_id_prefix_fail_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let context = temporary.path().join("context");
        fs::create_dir(&context).unwrap();
        fs::write(
            context.join("Dockerfile"),
            "FROM scratch\nCMD [\"/first\"]\n",
        )
        .unwrap();
        let store = Store::open(temporary.path().join("docker")).unwrap();
        let first = build(&store, &options(&context, "app:test")).unwrap();
        let mut duplicate = first.clone();
        duplicate.reference = "docker.io/library/app:test".into();
        duplicate.id = format!("sha256:{}", "1".repeat(64));
        fs::write(
            store.root.join("images.json"),
            serde_json::to_vec(&[first.clone(), duplicate.clone()]).unwrap(),
        )
        .unwrap();
        for result in [
            store.images().map(|_| ()),
            store.local_image("app:test").map(|_| ()),
            store.rmi("app:test", true).map(|_| ()),
        ] {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("duplicate canonical")
            );
        }
        duplicate.reference = "app:other".into();
        duplicate.id = format!("sha256:{}", "a".repeat(64));
        let mut first = first;
        first.id = format!("sha256:aaaa{}", "b".repeat(60));
        store
            .save_catalog(&BTreeMap::from([
                (image_reference_key(&first.reference).unwrap(), first),
                (
                    image_reference_key(&duplicate.reference).unwrap(),
                    duplicate,
                ),
            ]))
            .unwrap();
        assert!(
            store
                .local_image("aaaa")
                .unwrap_err()
                .to_string()
                .contains("ambiguous image ID")
        );
        assert!(
            store
                .rmi("aaaa", true)
                .unwrap_err()
                .to_string()
                .contains("ambiguous image ID")
        );
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
        assert!(
            serde_json::to_value(&image)
                .unwrap()
                .get("rootfs")
                .is_none()
        );
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
        prepare_resolver(
            root.rootfs(),
            &resolver,
            pvisor_overlaynet::api::ROUTER_IPV4,
        )
        .unwrap();
        assert_eq!(
            fs::read(resolver.join("etc/resolv.conf")).unwrap(),
            b"nameserver 192.0.2.1\n"
        );
    }
}
