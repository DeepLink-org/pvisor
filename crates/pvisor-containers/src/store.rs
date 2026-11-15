use crate::{api::*, images};
#[cfg(not(windows))]
use anyhow::bail;
use anyhow::{Context, ensure};
use fs2::FileExt;
use pvisor_guest::GuestConfig;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Owns the private catalog location; it never owns a global daemon.
pub struct Store {
    pub(crate) root: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ImageRecord {
    pub summary: Image,
    pub guest: GuestConfig,
    pub entrypoint: usize,
    #[serde(default = "default_stop_signal")]
    pub stop_signal: i32,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Record {
    pub schema: u32,
    pub info: Container,
    pub request: Run,
    pub guest: GuestConfig,
    #[serde(default = "default_stop_signal")]
    pub stop_signal: i32,
    /// Readiness or authenticated completion proved initial extraction finished.
    #[serde(default)]
    pub prepared: bool,
    #[serde(default)]
    pub startup_error: Option<String>,
}

fn default_stop_signal() -> i32 {
    15
}

#[cfg(any(windows, test))]
pub(crate) fn restarted(previous: &Record) -> anyhow::Result<Record> {
    ensure!(
        previous.info.state == State::Exited && previous.prepared,
        "container preparation was not confirmed; remove it and create a new instance"
    );
    ensure!(
        !previous.request.auto_remove,
        "auto-remove instances cannot be restarted"
    );
    let mut record = previous.clone();
    record.info.generation = uuid::Uuid::new_v4().simple().to_string();
    record.info.state = State::Starting;
    record.info.exit_code = None;
    record.request.detach = true;
    record.startup_error = None;
    Ok(record)
}

impl Default for Run {
    fn default() -> Self {
        Self {
            network: Network::Bridge,
            interactive: false,
            tty: None,
            image: String::new(),
            name: None,
            command: Vec::new(),
            env: Default::default(),
            unset_env: Vec::new(),
            cwd: None,
            user: None,
            entrypoint: None,
            detach: false,
            auto_remove: false,
            cpus: 2,
            memory_mib: 512,
        }
    }
}

pub(crate) fn read<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let mut data = Vec::new();
    File::open(path)?
        .take((4 << 20) + 1)
        .read_to_end(&mut data)?;
    ensure!(data.len() <= 4 << 20, "container record exceeds 4 MiB");
    Ok(serde_json::from_slice(&data)?)
}

pub(crate) fn publish<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("record parent")?)?;
    serde_json::to_writer(&mut file, value)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

pub(crate) fn lock(path: &Path) -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(file),
            Err(e) if contended(&e) => {
                ensure!(Instant::now() < deadline, "container store is busy");
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

pub(crate) fn live(directory: &Path) -> anyhow::Result<bool> {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.join("owner.lock"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    match file.try_lock_exclusive() {
        Ok(()) => Ok(false),
        Err(e) if contended(&e) => Ok(true),
        Err(e) => Err(e.into()),
    }
}

fn contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        || error
            .raw_os_error()
            .is_some_and(|code| Some(code) == fs2::lock_contended_error().raw_os_error())
}

pub(crate) fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Store {
    #[cfg(any(windows, test))]
    pub(crate) fn allocate_address(&self) -> anyhow::Result<std::net::Ipv4Addr> {
        let used: std::collections::BTreeSet<_> = self
            .records()?
            .into_iter()
            .filter_map(|record| record.info.ipv4)
            .collect();
        (2..=254)
            .map(|last| std::net::Ipv4Addr::new(172, 17, 0, last))
            .find(|address| !used.contains(address))
            .context("default bridge has no free IPv4 address")
    }
    pub(crate) fn directory(&self, id: &str) -> anyhow::Result<PathBuf> {
        ensure!(valid_id(id), "invalid container identity");
        Ok(self.root.join("instances").join(id))
    }
    pub(crate) fn records(&self) -> anyhow::Result<Vec<Record>> {
        let mut records = Vec::new();
        for entry in fs::read_dir(self.root.join("instances"))? {
            let entry = entry?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if !valid_id(&id) {
                continue;
            }
            let record: Record = read(&entry.path().join("record.json"))?;
            ensure!(
                record.schema == 1 && record.info.id == id,
                "invalid container record binding"
            );
            records.push(record);
        }
        Ok(records)
    }
    pub(crate) fn resolve(&self, selector: &str) -> anyhow::Result<Record> {
        ensure!(!selector.is_empty(), "empty container selector");
        let records = self.records()?;
        let exact: Vec<_> = records
            .iter()
            .filter(|r| r.info.id == selector || r.info.name == selector)
            .collect();
        if exact.len() == 1 {
            return Ok(exact[0].clone());
        }
        let mut matches = records
            .into_iter()
            .filter(|r| r.info.id.starts_with(selector));
        let record = matches
            .next()
            .with_context(|| format!("no such container: {selector}"))?;
        ensure!(
            matches.next().is_none(),
            "ambiguous container selector: {selector}"
        );
        Ok(record)
    }
    #[cfg(windows)]
    pub(crate) fn save(&self, record: &Record) -> anyhow::Result<()> {
        publish(
            &self.directory(&record.info.id)?.join("record.json"),
            record,
        )
    }
    #[cfg(windows)]
    pub(crate) fn image_record(&self, reference: &str) -> anyhow::Result<Option<ImageRecord>> {
        let path = self
            .root
            .join("images")
            .join(format!("{}.json", images::hash(reference.as_bytes())));
        if !path.exists() {
            return Ok(None);
        }
        let record: ImageRecord = read(&path)?;
        ensure!(
            record.summary.reference == reference,
            "image reference record binding mismatch"
        );
        Ok(Some(record))
    }
}

impl Containers for Store {
    fn open(root: Option<PathBuf>) -> anyhow::Result<Self> {
        let root = root
            .or_else(|| std::env::var_os("PVISOR_DOCKER_HOME").map(PathBuf::from))
            .or_else(|| dirs::data_local_dir().map(|p| p.join("pvisor/docker")))
            .context("no local application data directory")?;
        fs::create_dir_all(&root)?;
        ensure!(
            !fs::symlink_metadata(&root)?.file_type().is_symlink(),
            "container root must not be a symlink"
        );
        let root = root.canonicalize()?;
        for name in ["images", "blobs", "instances"] {
            fs::create_dir_all(root.join(name))?;
        }
        Ok(Self { root })
    }
    fn pull(&self, reference: &str) -> anyhow::Result<Image> {
        let record = images::pull(&self.root, reference)?;
        let _lock = lock(&self.root.join("catalog.lock"))?;
        publish(
            &self
                .root
                .join("images")
                .join(format!("{}.json", images::hash(reference.as_bytes()))),
            &record,
        )?;
        Ok(record.summary)
    }
    fn images(&self) -> anyhow::Result<Vec<Image>> {
        let _lock = lock(&self.root.join("catalog.lock"))?;
        let mut images = Vec::new();
        for entry in fs::read_dir(self.root.join("images"))? {
            let entry = entry?;
            if entry.path().extension().is_some_and(|e| e == "json") {
                images.push(read::<ImageRecord>(&entry.path())?.summary);
            }
        }
        images.sort_by(|a, b| a.reference.cmp(&b.reference));
        Ok(images)
    }
    fn remove_image(&self, selector: &str, force: bool) -> anyhow::Result<Vec<Image>> {
        ensure!(!selector.is_empty(), "empty image selector");
        let _catalog = lock(&self.root.join("catalog.lock"))?;
        let mut records = Vec::new();
        for entry in fs::read_dir(self.root.join("images"))? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                records.push((path.clone(), read::<ImageRecord>(&path)?.summary));
            }
        }
        let exact = records.iter().any(|(_, image)| image.reference == selector);
        let hex = selector.strip_prefix("sha256:").unwrap_or(selector);
        let matches: Vec<_> = records
            .into_iter()
            .filter(|(_, image)| {
                if exact {
                    image.reference == selector
                } else {
                    hex.len() >= 12
                        && hex.len() <= 64
                        && hex
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                        && image
                            .id
                            .strip_prefix("sha256:")
                            .is_some_and(|id| id.starts_with(hex))
                }
            })
            .collect();
        ensure!(!matches.is_empty(), "no such image: {selector}");
        ensure!(
            matches.iter().all(|(_, image)| image.id == matches[0].1.id),
            "ambiguous image selector: {selector}"
        );
        ensure!(
            force || matches.len() == 1,
            "image has multiple tags; use --force"
        );
        if !force {
            let id = &matches[0].1.id;
            ensure!(
                !self.records()?.iter().any(|r| r.info.image_id == *id
                    || (r.info.image_id.is_empty()
                        && matches
                            .iter()
                            .any(|(_, image)| image.reference == r.info.image))),
                "image is used by a retained container; use --force to untag only"
            );
        }
        let mut removed = Vec::new();
        for (path, image) in matches {
            fs::remove_file(path)?;
            removed.push(image);
        }
        removed.sort_by(|a, b| a.reference.cmp(&b.reference));
        Ok(removed)
    }
    fn run(&self, request: Run) -> anyhow::Result<Container> {
        #[cfg(windows)]
        {
            crate::native::run(self, request)
        }
        #[cfg(not(windows))]
        {
            let _ = request;
            bail!("this execution profile requires Windows/WHP")
        }
    }
    fn start(&self, selector: &str) -> anyhow::Result<Container> {
        #[cfg(windows)]
        {
            crate::native::start(self, selector)
        }
        #[cfg(not(windows))]
        {
            let _ = selector;
            bail!("this execution profile requires Windows/WHP")
        }
    }
    fn list(&self, all: bool) -> anyhow::Result<Vec<Container>> {
        let _lock = lock(&self.root.join("catalog.lock"))?;
        let mut result = Vec::new();
        for mut r in self.records()? {
            if r.info.state != State::Exited && !live(&self.directory(&r.info.id)?)? {
                r.info.state = State::Unknown;
                r.info.exit_code = None;
            }
            if all || r.info.state == State::Running {
                result.push(r.info);
            }
        }
        Ok(result)
    }
    fn wait(
        &self,
        container: &Container,
        input: Option<&mut dyn InputSource>,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> anyhow::Result<i32> {
        #[cfg(windows)]
        {
            crate::native::wait(self, container, input, stdout, stderr)
        }
        #[cfg(not(windows))]
        {
            let _ = (container, input, stdout, stderr);
            bail!("this execution profile requires Windows/WHP")
        }
    }
    fn exec(
        &self,
        selector: &str,
        launch: pvisor_guest::control::ExecLaunch,
        unset_env: Vec<String>,
        input: Option<&mut dyn InputSource>,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> anyhow::Result<i32> {
        #[cfg(windows)]
        {
            crate::native::exec(self, selector, launch, unset_env, input, stdout, stderr)
        }
        #[cfg(not(windows))]
        {
            let _ = (selector, launch, unset_env, input, stdout, stderr);
            bail!("this execution profile requires Windows/WHP")
        }
    }
    fn logs(
        &self,
        selector: &str,
        follow: bool,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> anyhow::Result<()> {
        let record = self.resolve(selector)?;
        let dir = self.directory(&record.info.id)?;
        let open = |name: &str| -> anyhow::Result<Option<File>> {
            if follow {
                // Open both append targets before following, even during boot,
                // so auto-removal cannot erase a not-yet-opened final stream.
                return Ok(Some(
                    OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create(true)
                        .truncate(false)
                        .open(dir.join(name))?,
                ));
            }
            match File::open(dir.join(name)) {
                Ok(file) => Ok(Some(file)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            }
        };
        let (mut out, mut err) = (open("stdout.log")?, open("stderr.log")?);
        loop {
            if let Some(file) = &mut out {
                std::io::copy(file, stdout)?;
            }
            if let Some(file) = &mut err {
                std::io::copy(file, stderr)?;
            }
            stdout.flush()?;
            stderr.flush()?;
            let current = match read::<Record>(&dir.join("record.json")) {
                Ok(current) => Some(current),
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                {
                    None
                }
                Err(error) => return Err(error),
            };
            let active = if let Some(current) = current {
                current.info.generation == record.info.generation
                    && match live(&dir) {
                        Ok(active) => active,
                        Err(error)
                            if error
                                .downcast_ref::<std::io::Error>()
                                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                        {
                            false
                        }
                        Err(error) => return Err(error),
                    }
            } else {
                false
            };
            if !follow || !active {
                if let Some(file) = &mut out {
                    std::io::copy(file, stdout)?;
                }
                if let Some(file) = &mut err {
                    std::io::copy(file, stderr)?;
                }
                stdout.flush()?;
                stderr.flush()?;
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn stop(&self, selector: &str, timeout: Duration, signal: Option<&str>) -> anyhow::Result<()> {
        let signal = signal
            .map(pvisor_guest::control::parse_signal)
            .transpose()?;
        if timeout != Duration::MAX {
            Instant::now()
                .checked_add(timeout)
                .context("stop timeout exceeds monotonic clock range")?;
        }
        #[cfg(windows)]
        {
            crate::native::stop(self, selector, timeout, signal)
        }
        #[cfg(not(windows))]
        {
            let _ = (selector, timeout, signal);
            bail!("this execution profile requires Windows/WHP")
        }
    }
    fn remove(&self, selector: &str, force: bool) -> anyhow::Result<()> {
        // Pin identity before stopping: a name can be reused after auto-remove.
        let record = self.resolve(selector)?;
        let dir = self.directory(&record.info.id)?;
        #[cfg(windows)]
        if force {
            crate::native::stop(self, &record.info.id, Duration::ZERO, Some(9))?;
        }
        #[cfg(not(windows))]
        let _ = force;
        let _lock = lock(&self.root.join("catalog.lock"))?;
        if !dir.exists() {
            return Ok(());
        }
        ensure!(!live(&dir)?, "container is running; stop it or use --force");
        fs::remove_dir_all(dir)?;
        Ok(())
    }
    fn internal_if_requested() -> anyhow::Result<bool> {
        #[cfg(windows)]
        {
            crate::native::internal()
        }
        #[cfg(not(windows))]
        {
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_bridge_addresses_are_reserved_until_removal_and_exhaustion_is_explicit() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(Some(temp.path().join("store"))).unwrap();
        let first = std::net::Ipv4Addr::new(172, 17, 0, 2);
        assert_eq!(store.allocate_address().unwrap(), first);
        let (dir, mut record) = logs_fixture(&store);
        record.info.network = Network::Bridge;
        record.info.ipv4 = Some(first);
        record.info.state = State::Exited;
        publish(&dir.join("record.json"), &record).unwrap();
        assert_eq!(store.allocate_address().unwrap().octets(), [172, 17, 0, 3]);
        fs::remove_dir_all(dir).unwrap();
        assert_eq!(store.allocate_address().unwrap(), first);
        for last in 2..=254 {
            record.info.id = format!("{last:032x}");
            record.info.ipv4 = Some(std::net::Ipv4Addr::new(172, 17, 0, last));
            fs::create_dir(store.directory(&record.info.id).unwrap()).unwrap();
            publish(
                &store
                    .directory(&record.info.id)
                    .unwrap()
                    .join("record.json"),
                &record,
            )
            .unwrap();
        }
        assert!(
            store
                .allocate_address()
                .unwrap_err()
                .to_string()
                .contains("no free IPv4")
        );
    }

    struct Observed(Vec<u8>, Option<std::sync::mpsc::Sender<()>>);
    impl Write for Observed {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.extend_from_slice(bytes);
            if let Some(ready) = self.1.take() {
                ready.send(()).unwrap();
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn logs_fixture(store: &Store) -> (PathBuf, Record) {
        let id = "e".repeat(32);
        let dir = store.directory(&id).unwrap();
        fs::create_dir(&dir).unwrap();
        let record = Record {
            schema: 1,
            info: Container {
                id,
                generation: "f".repeat(32),
                name: "logs".into(),
                image: "alpine".into(),
                image_id: format!("sha256:{}", "a".repeat(64)),
                command: vec!["sleep".into(), "30".into()],
                state: State::Running,
                exit_code: None,
                network: Network::None,
                ipv4: None,
            },
            request: Run::default(),
            guest: GuestConfig::default(),
            stop_signal: 10,
            prepared: true,
            startup_error: None,
        };
        publish(&dir.join("record.json"), &record).unwrap();
        fs::write(dir.join("stdout.log"), b"initial-out\n").unwrap();
        fs::write(dir.join("stderr.log"), b"initial-err\n").unwrap();
        (dir, record)
    }

    #[test]
    fn following_logs_drains_files_through_auto_removal_and_excludes_boot_output() {
        use std::sync::mpsc;
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(Some(temp.path().join("store"))).unwrap();
        let (dir, _) = logs_fixture(&store);
        fs::write(dir.join("boot.stderr"), b"must-not-be-a-workload-log").unwrap();
        let owner = lock(&dir.join("owner.lock")).unwrap();
        let (ready, seen) = mpsc::channel();
        let (finished, result) = mpsc::channel();
        let follower = std::thread::spawn(move || {
            let mut stdout = Observed(Vec::new(), Some(ready));
            let mut stderr = Vec::new();
            store.logs("logs", true, &mut stdout, &mut stderr).unwrap();
            finished.send((stdout.0, stderr)).unwrap();
        });
        seen.recv_timeout(Duration::from_secs(5)).unwrap();
        OpenOptions::new()
            .append(true)
            .open(dir.join("stdout.log"))
            .unwrap()
            .write_all(b"final-out\n")
            .unwrap();
        OpenOptions::new()
            .append(true)
            .open(dir.join("stderr.log"))
            .unwrap()
            .write_all(b"final-err\n")
            .unwrap();
        drop(owner);
        fs::remove_dir_all(&dir).unwrap();
        let (stdout, stderr) = result.recv_timeout(Duration::from_secs(5)).unwrap();
        follower.join().unwrap();
        assert_eq!(stdout, b"initial-out\nfinal-out\n");
        assert_eq!(stderr, b"initial-err\nfinal-err\n");
    }

    #[test]
    fn logs_stop_on_generation_change_and_surface_writer_failures() {
        use std::sync::mpsc;
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(Some(temp.path().join("store"))).unwrap();
        let (dir, mut record) = logs_fixture(&store);
        let owner = lock(&dir.join("owner.lock")).unwrap();
        let root = store.root.clone();
        let (ready, seen) = mpsc::channel();
        let (finished, result) = mpsc::channel();
        let follower = std::thread::spawn(move || {
            let store = Store::open(Some(root)).unwrap();
            let mut out = Observed(Vec::new(), Some(ready));
            let mut err = Vec::new();
            store.logs("logs", true, &mut out, &mut err).unwrap();
            finished.send(()).unwrap();
        });
        // A new generation with a live lease must not retain the old follower.
        seen.recv_timeout(Duration::from_secs(5)).unwrap();
        record.info.generation = "b".repeat(32);
        publish(&dir.join("record.json"), &record).unwrap();
        result.recv_timeout(Duration::from_secs(5)).unwrap();
        follower.join().unwrap();
        drop(owner);
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(
            store
                .logs("logs", false, &mut Broken, &mut Vec::new())
                .is_err()
        );
        assert_eq!(record.stop_signal, 10);
        let mut legacy = serde_json::to_value(&record).unwrap();
        legacy.as_object_mut().unwrap().remove("stop_signal");
        assert_eq!(
            serde_json::from_value::<Record>(legacy)
                .unwrap()
                .stop_signal,
            15
        );
    }

    #[test]
    fn retained_instances_pin_image_identity_even_after_a_tag_changes() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(Some(temp.path().join("store"))).unwrap();
        let image = Image {
            reference: "fixture:latest".into(),
            id: format!("sha256:{}", "a".repeat(64)),
            digest: format!("sha256:{}", "b".repeat(64)),
        };
        let path = store
            .root
            .join("images")
            .join(format!("{}.json", images::hash(image.reference.as_bytes())));
        publish(
            &path,
            &ImageRecord {
                summary: image.clone(),
                guest: GuestConfig::default(),
                entrypoint: 0,
                stop_signal: 15,
            },
        )
        .unwrap();
        let id = "c".repeat(32);
        let directory = store.directory(&id).unwrap();
        fs::create_dir(&directory).unwrap();
        publish(
            &directory.join("record.json"),
            &Record {
                schema: 1,
                info: Container {
                    id: id.clone(),
                    generation: "d".repeat(32),
                    name: "retained".into(),
                    image: "old-tag".into(),
                    image_id: image.id.clone(),
                    command: Vec::new(),
                    state: State::Exited,
                    exit_code: Some(0),
                    network: Network::None,
                    ipv4: None,
                },
                request: Run::default(),
                guest: GuestConfig::default(),
                stop_signal: 15,
                prepared: true,
                startup_error: None,
            },
        )
        .unwrap();
        assert!(store.remove_image(&image.reference, false).is_err());
        assert!(path.exists());
        store.remove_image(&image.reference, true).unwrap();
        assert!(!path.exists());
        assert_eq!(store.list(true).unwrap()[0].image_id, image.id);
        assert!(directory.exists());
        let previous = store.resolve(&id).unwrap();
        let next = restarted(&previous).unwrap();
        assert_eq!(next.info.id, previous.info.id);
        assert_ne!(next.info.generation, previous.info.generation);
        assert_eq!(next.info.state, State::Starting);
        assert_eq!(next.info.exit_code, None);
        assert!(next.prepared && next.request.detach);
        assert_eq!(
            serde_json::to_vec(&next.guest).unwrap(),
            serde_json::to_vec(&previous.guest).unwrap()
        );
        assert_ne!(
            restarted(&previous).unwrap().info.generation,
            next.info.generation
        );
        let mut invalid = previous.clone();
        invalid.info.state = State::Unknown;
        assert!(restarted(&invalid).is_err());
        invalid.info.state = State::Exited;
        invalid.prepared = false;
        assert!(restarted(&invalid).is_err());
        invalid.prepared = true;
        invalid.request.auto_remove = true;
        assert!(restarted(&invalid).is_err());
    }
}
