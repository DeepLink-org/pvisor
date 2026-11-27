//! Admit Linux cold runs from a leased, verified OCI import.
use super::*;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::DirBuilderExt;

mod view;

pub(super) const PREFIX: &str = "pvisor-import-v1:";

#[derive(Serialize, Deserialize)]
enum Event {
    Progress(PullProgress),
    Created(String),
    Failed(String),
}

fn event(message: Event) -> anyhow::Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, &message)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

fn require_frontend(pid: libc::pid_t) -> io::Result<()> {
    if unsafe { libc::getppid() } != pid {
        return Err(io::Error::from(io::ErrorKind::BrokenPipe));
    }
    Ok(())
}

fn set_parent_death_signal(signal: libc::c_int) -> io::Result<()> {
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, signal as libc::c_ulong, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

impl Store {
    /// Prepare an instance for immediate Linux/KVM execution. A verified cold
    /// import stays pinned by the instance. A separate process publishes the
    /// durable image unless the caller sets `PVISOR_OCI_EPHEMERAL=1` for a
    /// one-shot run. One-shot instances require the import on the current boot.
    pub fn create_for_run(
        &self,
        config: Create,
        progress: &mut dyn FnMut(PullProgress),
    ) -> anyhow::Result<Instance> {
        if imports(self).try_exists()? {
            let _guard = self.lock()?;
            collect(self)?;
        }
        let ephemeral = crate::env::cache::experiment_setting(
            "PVISOR_OCI_EPHEMERAL",
            crate::env::os("PVISOR_OCI_EPHEMERAL").as_deref(),
        )?;
        if self.registry_credentials.is_some() && !ephemeral {
            return self.create_with_progress(config, progress);
        }
        if let Some(image) = self.local_image(&config.image)? {
            return self.create_pinned_member(config, progress, None, Some(image));
        }
        ensure!(
            self.registry_credentials.is_none(),
            "ephemeral cold run requires a local image or a registry accessible to the cold-run worker"
        );
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("--pvisor-internal-cold-run")
            .arg(&self.root)
            .arg(&self.images.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if std::env::var_os("PVISOR_IMAGE_BATCH_DIRECTORY_SYNC").is_none() {
            command.env("PVISOR_IMAGE_BATCH_DIRECTORY_SYNC", "1");
        }
        let frontend = unsafe { libc::getpid() };
        unsafe {
            command.pre_exec(move || {
                set_parent_death_signal(libc::SIGKILL)?;
                require_frontend(frontend)
            });
        }
        let mut child = command.spawn()?;
        let result = (|| {
            serde_json::to_writer(
                child.stdin.take().context("missing cold run input")?,
                &(&config, ephemeral),
            )?;
            let output = BufReader::new(child.stdout.take().context("missing cold run output")?);
            for line in output.lines() {
                match serde_json::from_str(&line?)? {
                    Event::Progress(p) => progress(p),
                    Event::Created(id) => return self.resolve(&id),
                    Event::Failed(error) => bail!("{error}"),
                }
            }
            bail!("cold run publisher closed before instance admission")
        })();
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        result
    }
}

fn imports(store: &Store) -> PathBuf {
    store.root.join("cold-imports")
}

fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Serialize, Deserialize)]
struct Binding {
    digest: String,
    config: String,
    platform: String,
    boot: String,
    device: u64,
    inode: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    prefix_view: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    ephemeral: bool,
}

pub(super) fn open(store: &Store, image: &Image) -> anyhow::Result<ImageRoot> {
    let id = image
        .cache
        .handle
        .strip_prefix(PREFIX)
        .filter(|id| valid_id(id))
        .context("invalid cold import binding")?;
    ensure!(
        image.cache.repository.is_none() && image.cache.mirrors.is_empty(),
        "foreign cold import"
    );
    let dir = imports(store).join(id);
    ensure!(dir.canonicalize()? == dir, "cold import owner changed");
    let lease = File::open(&dir)?;
    FileExt::lock_shared(&lease)?;
    let binding: Binding = serde_json::from_reader(File::open(dir.join("binding.json"))?)?;
    ensure!(
        binding.digest == image.digest
            && binding.config == image.id
            && binding.platform == image.platform,
        "cold import identity mismatch"
    );
    let published = || -> anyhow::Result<ImageRoot> {
        ensure!(
            !binding.ephemeral,
            "ephemeral OCI import expired after reboot or loss of its mounted view. Run the OCI image again"
        );
        // The extracted files are only verified for this boot. Reopen the
        // exact durable revision after a host restart, even if the tag moved.
        let handle: String = serde_json::from_reader(
            File::open(dir.join("published.json"))
                .context("cold image publication did not finish before reboot or view loss")?,
        )?;
        let mut cached = image.cache.clone();
        cached.handle = handle;
        store.open_cached(&cached, &image.digest, &image.platform)
    };
    if binding.boot != fs::read_to_string("/proc/sys/kernel/random/boot_id")? {
        return published();
    }
    let root = dir
        .join("rootfs-v3/sha256")
        .join(crate::image::oci::digest_hex(&image.digest)?);
    let accessible = || -> io::Result<bool> {
        let meta = fs::symlink_metadata(&root)?;
        Ok(meta.is_dir()
            && root.canonicalize()? == root
            && (meta.dev(), meta.ino()) == (binding.device, binding.inode))
    };
    if binding.prefix_view && !accessible().unwrap_or(false) {
        return published();
    }
    if !binding.prefix_view {
        ensure!(accessible()?, "cold import root changed");
    }
    Ok(ImageRoot {
        root,
        _owner: None,
        _import_lease: Some(lease),
    })
}

// Caller holds the catalog lock, which serializes admission, fork and removal.
// Filesystem locks additionally pin live readers and a publisher yet to admit a Session.
pub(super) fn collect(store: &Store) -> anyhow::Result<()> {
    let root = imports(store);
    if !root.try_exists()? {
        return Ok(());
    }
    private_directory(&root)?;
    let retained = store
        .all()?
        .into_iter()
        .map(|i| i.image.cache.handle)
        .collect::<std::collections::HashSet<_>>();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let id = entry.file_name();
        let Some(id) = id.to_str().filter(|id| valid_id(id)) else {
            continue;
        };
        if retained.contains(&format!("{PREFIX}{id}")) {
            continue;
        }
        ensure!(entry.file_type()?.is_dir(), "invalid cold import owner");
        let lease = File::open(entry.path())?;
        match FileExt::try_lock_exclusive(&lease) {
            Ok(()) => view::remove_unmounted(&entry.path())?,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                view::request_retirement(&entry.path())?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    match fs::remove_dir(root) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn prepare(
    store: &Store,
    reference: &str,
    dir: &Path,
    id: &str,
    ephemeral: bool,
    prefix_view: &mut Option<crate::image::oci::PrefixView>,
) -> anyhow::Result<(Image, Option<PreparedPull>)> {
    if let Some(image) = store.local_image(reference)? {
        return Ok((image, None));
    }
    let mut write_error = Ok(());
    let mut prepared = store.prepare_pull(
        reference,
        &native_platform(),
        &mut |p| {
            if write_error.is_ok() {
                write_error = event(Event::Progress(p));
            }
        },
        Some(ColdImport {
            directory: dir,
            ephemeral,
        }),
    )?;
    write_error?;
    if prepared.cached.is_some() {
        return Ok((store.publish_pull(prepared)?, None));
    }
    if !crate::image::cache::supports_extracted_run(
        &prepared.source,
        &prepared.image.digest,
        prepared.prefix_view.as_ref(),
    )? {
        ensure!(
            !ephemeral,
            "image is ineligible for an ephemeral run from its extracted root"
        );
        return Ok((store.publish_pull(prepared)?, None));
    }
    store
        .images
        .retain_config(&prepared.image.id, &prepared.config)?;
    let meta = fs::metadata(&prepared.rootfs)?;
    let binding = Binding {
        digest: prepared.image.digest.clone(),
        config: prepared.image.id.clone(),
        platform: prepared.image.platform.clone(),
        boot: fs::read_to_string("/proc/sys/kernel/random/boot_id")?,
        device: meta.dev(),
        inode: meta.ino(),
        prefix_view: prepared.prefix_view.is_some(),
        ephemeral,
    };
    crate::util::write_private_json(&dir.join("binding.json"), &binding)?;
    let image = prepared.image.clone().map_cache(|()| CachedImage {
        location: store.images.pool(),
        handle: format!("{PREFIX}{id}"),
        repository: None,
        mirrors: vec![],
    });
    *prefix_view = prepared.prefix_view.take();
    if ephemeral {
        fs::remove_dir_all(dir.join("blobs"))?;
        return Ok((image, None));
    }
    Ok((image, Some(prepared)))
}

pub(super) fn worker(store: &Store) -> anyhow::Result<()> {
    let frontend = unsafe { libc::getppid() };
    let mut admitted = false;
    let result = (|| {
        let (config, ephemeral): (Create, bool) = serde_json::from_reader(io::stdin().lock())?;
        let id = uuid();
        let dir = imports(store).join(&id);
        let lease = {
            let _guard = store.lock()?;
            collect(store)?;
            private_directory(&imports(store))?;
            fs::DirBuilder::new().mode(0o700).create(&dir)?;
            let lease = File::open(&dir)?;
            FileExt::lock_shared(&lease)?;
            lease
        };
        view::prepare(&dir)?;
        let mut prefix_view = None;
        let mut publication = None;
        let instance = store.create_resolving(config, None, |reference| {
            let (image, prepared) =
                prepare(store, reference, &dir, &id, ephemeral, &mut prefix_view)?;
            // Once admission can change the catalog, use the Created pipe to
            // roll back a lost handoff instead of dying during the write.
            if unsafe { libc::setsid() } < 0 {
                return Err(io::Error::last_os_error().into());
            }
            set_parent_death_signal(0)?;
            require_frontend(frontend).context("cold run frontend exited")?;
            publication = prepared;
            Ok(image)
        })?;
        if let Err(e) = event(Event::Created(instance.id.clone())) {
            store.remove(&instance.id, true)?;
            return Err(e);
        }
        admitted = true;
        let published = (|| -> anyhow::Result<()> {
            let Some(prepared) = publication else {
                return Ok(());
            };
            let result = store.publish_pull(prepared);
            if let Ok(published) = &result {
                crate::util::write_private_json(
                    &dir.join("published.json"),
                    &published.cache.handle,
                )?;
            } else if let Err(e) = &result {
                crate::util::write_private_json(
                    &dir.join("publication-error.json"),
                    &format!("{e:#}"),
                )?;
            }
            let blobs = dir.join("blobs");
            if blobs.try_exists()? {
                fs::remove_dir_all(blobs)?;
            }
            result.map(|_| ())
        })();
        if let Some(view) = prefix_view {
            view::serve_until_retired(store, &id, &dir, &lease, view)?;
        }
        drop(lease);
        published
    })();
    if let Err(e) = &result
        && !admitted
    {
        let _ = event(Event::Failed(format!("{e:#}")));
    }
    let _guard = store.lock()?;
    collect(store)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_cold_import_collection_resumes_after_reopen() {
        use pvisor_journal::api::{DurableFiles, Persistence};

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("docker");
        let store = Store::open(root.clone()).unwrap();
        let id = "b".repeat(64);
        let directory = store.directory(&id);
        private_directory(&directory).unwrap();
        private_directory(&store.retired_root()).unwrap();
        let plan = RetiredImages {
            collect_cold_imports: true,
            projections: Vec::new(),
        };
        Persistence::atomic_write(
            &directory.join(RETIRED_IMAGES),
            &serde_json::to_vec(&plan).unwrap(),
            0o600,
        )
        .unwrap();
        let owner = imports(&store).join(uuid());
        private_directory(&owner).unwrap();
        let obstructed = imports(&store).join(uuid());
        fs::write(&obstructed, b"invalid owner fixture").unwrap();

        // End this invocation immediately after the active directory was retired.
        let retired = store.retired_directory(&id);
        fs::rename(&directory, &retired).unwrap();
        drop(store);
        let store = Store::open(root).unwrap();
        let error = store.remove_retired(&id).unwrap_err();
        assert!(format!("{error:#}").contains("invalid cold import owner"));
        assert!(retired.join(RETIRED_IMAGES).is_file());

        fs::remove_file(obstructed).unwrap();
        store.remove_retired(&id).unwrap();
        store.remove_retired(&id).unwrap();
        assert!(!owner.exists() && !retired.exists());
    }

    #[test]
    fn recovery_collects_abandoned_imports_and_respects_reader_locks() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("docker")).unwrap();
        let _guard = store.lock().unwrap();
        let abandoned = imports(&store).join(uuid());
        let reading = imports(&store).join(uuid());
        private_directory(&abandoned).unwrap();
        private_directory(&reading).unwrap();
        let lease = File::open(&reading).unwrap();
        FileExt::lock_shared(&lease).unwrap();
        collect(&store).unwrap();
        assert!(!abandoned.exists());
        assert!(reading.exists());
        drop(lease);
        collect(&store).unwrap();
        assert!(!reading.exists());
    }
    #[test]
    fn retiring_an_import_signals_the_view_and_preserves_readers() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("docker")).unwrap();
        let _guard = store.lock().unwrap();
        let dir = imports(&store).join(uuid());
        private_directory(&dir).unwrap();
        File::create_new(dir.join("prefix-view-owner")).unwrap();
        let reader = File::open(&dir).unwrap();
        FileExt::lock_shared(&reader).unwrap();
        collect(&store).unwrap();
        assert!(dir.join("retire-prefix-view").is_file());
        drop(reader);
        collect(&store).unwrap();
        assert!(!dir.exists());
    }
    #[test]
    fn cleanup_refuses_a_redirected_import_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("docker")).unwrap();
        let other = tmp.path().join("other");
        let keep = other.join(uuid());
        private_directory(&keep).unwrap();
        std::os::unix::fs::symlink(&other, imports(&store)).unwrap();
        let _guard = store.lock().unwrap();
        assert!(collect(&store).is_err());
        assert!(keep.is_dir());
    }

    #[test]
    fn an_unpublished_import_cannot_reopen_after_a_host_reboot() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("docker")).unwrap();
        let id = uuid();
        let dir = imports(&store).join(&id);
        let digest = format!("sha256:{}", "a".repeat(64));
        let root = dir.join("rootfs-v3/sha256").join(&digest[7..]);
        private_directory(&root).unwrap();
        let image: Image = serde_json::from_value(serde_json::json!({
            "reference": "fixture:latest", "id": digest, "digest": digest,
            "platform": native_platform(), "cache": {"location": store.images.pool(),
            "handle": format!("{PREFIX}{id}")}, "env": {}, "entrypoint": [], "cmd": [],
            "cwd": "/", "user": "", "stop_signal": 15, "volumes": [], "created_ms": 0
        }))
        .unwrap();
        let meta = fs::metadata(root).unwrap();
        let mut binding = Binding {
            digest: image.digest.clone(),
            config: image.id.clone(),
            platform: image.platform.clone(),
            boot: fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap(),
            device: meta.dev(),
            inode: meta.ino(),
            prefix_view: false,
            ephemeral: false,
        };
        let path = dir.join("binding.json");
        crate::util::write_private_json(&path, &binding).unwrap();
        assert!(!fs::read_to_string(&path).unwrap().contains("ephemeral"));
        assert!(open(&store, &image).is_ok());
        binding.boot = "previous boot".into();
        crate::util::write_private_json(&path, &binding).unwrap();
        assert!(
            open(&store, &image)
                .err()
                .unwrap()
                .to_string()
                .contains("before reboot")
        );
        binding.ephemeral = true;
        crate::util::write_private_json(&path, &binding).unwrap();
        assert!(
            open(&store, &image)
                .err()
                .unwrap()
                .to_string()
                .contains("ephemeral OCI import expired")
        );
    }
}
