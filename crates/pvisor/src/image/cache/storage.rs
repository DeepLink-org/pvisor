//! Small object API shared by filesystem and S3 caches; keys are internal only.
use anyhow::{Context, ensure};
use object_store::{GetOptions, GetRange, ObjectStore, PutMode, PutOptions, UpdateVersion};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) const MAX_OBJECT: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub(crate) enum Storage {
    Filesystem(PathBuf),
    S3(Arc<S3>),
}
impl Storage {
    pub(crate) fn filesystem(root: PathBuf, create: bool) -> anyhow::Result<Self> {
        ensure!(
            root.is_absolute(),
            "filesystem cache location must be absolute"
        );
        if create {
            fs::create_dir_all(&root)?;
        }
        let root = root.canonicalize()?;
        Ok(Self::Filesystem(root))
    }
    pub(crate) fn s3(location: &str) -> anyhow::Result<Self> {
        let remainder = location
            .strip_prefix("s3://")
            .context("expected s3://BUCKET/PREFIX")?;
        let (bucket, prefix) = remainder.split_once('/').unwrap_or((remainder, ""));
        ensure!(
            !bucket.is_empty() && !bucket.contains(['@', '?', '#', ':']),
            "invalid S3 bucket"
        );
        let prefix = prefix.trim_end_matches('/');
        if !prefix.is_empty() {
            validate_key(prefix)?;
        }
        let store = object_store::aws::AmazonS3Builder::from_env()
            .with_bucket_name(bucket)
            .with_config(
                object_store::aws::AmazonS3ConfigKey::Client(
                    object_store::ClientConfigKey::Timeout,
                ),
                "300s",
            )
            .with_config(
                object_store::aws::AmazonS3ConfigKey::Client(
                    object_store::ClientConfigKey::ConnectTimeout,
                ),
                "10s",
            )
            .build()
            .context("configure S3 cache")?;
        Ok(Self::S3(Arc::new(S3::new(Arc::new(store), prefix.into())?)))
    }

    pub(super) fn get(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.get_versioned(key)?.map(|object| object.bytes))
    }
    pub(super) fn get_versioned(&self, key: &str) -> anyhow::Result<Option<StoredObject>> {
        self.read(key, None, MAX_OBJECT)
    }
    pub(crate) fn get_bounded(&self, key: &str, limit: usize) -> anyhow::Result<Option<Vec<u8>>> {
        ensure!(limit <= MAX_OBJECT, "invalid cache read limit");
        Ok(self.read(key, None, limit)?.map(|object| object.bytes))
    }
    pub(super) fn range(&self, key: &str, range: Range<u64>) -> anyhow::Result<Vec<u8>> {
        ensure!(
            range.end > range.start && range.end - range.start <= MAX_OBJECT as u64,
            "invalid cache range"
        );
        let expected = (range.end - range.start) as usize;
        let object = self
            .read(key, Some(range), MAX_OBJECT)?
            .context("missing cache object")?;
        ensure!(object.bytes.len() == expected, "truncated cache range");
        Ok(object.bytes)
    }
    fn read(
        &self,
        key: &str,
        range: Option<Range<u64>>,
        limit: usize,
    ) -> anyhow::Result<Option<StoredObject>> {
        validate_key(key)?;
        match self {
            Self::S3(s3) => s3.request(
                key,
                if range.is_none() && limit != MAX_OBJECT {
                    Operation::GetBounded(limit)
                } else {
                    Operation::Get(range)
                },
            ),
            Self::Filesystem(root) => {
                let path = confined(root, key, false)?;
                let mut file = match OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&path)
                {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(error) => return Err(error.into()),
                };
                ensure!(
                    file.metadata()?.is_file(),
                    "cache object is not a regular file"
                );
                let length = if let Some(range) = range {
                    ensure!(range.end <= file.metadata()?.len(), "truncated cache range");
                    file.seek(SeekFrom::Start(range.start))?;
                    range.end - range.start
                } else {
                    file.metadata()?.len()
                };
                ensure!(length <= limit as u64, "cache object exceeds size limit");
                let mut bytes = Vec::new();
                file.take(length).read_to_end(&mut bytes)?;
                // A range read must not include the following byte.
                bytes.truncate(length as usize);
                ensure!(bytes.len() == length as usize, "truncated cache object");
                let version = UpdateVersion {
                    e_tag: Some(super::hash(&bytes)),
                    version: None,
                };
                Ok(Some(StoredObject { bytes, version }))
            }
        }
    }
    pub(super) fn put(&self, key: &str, bytes: Vec<u8>, immutable: bool) -> anyhow::Result<()> {
        let result = self.write(
            key,
            bytes,
            if immutable {
                PutMode::Create
            } else {
                PutMode::Overwrite
            },
        );
        match result {
            Err(error) if immutable && is_conflict(&error) => Ok(()),
            result => result,
        }
    }
    pub(crate) fn compare_and_swap(
        &self,
        key: &str,
        bytes: Vec<u8>,
        expected: Option<UpdateVersion>,
    ) -> anyhow::Result<()> {
        self.write(
            key,
            bytes,
            expected.map(PutMode::Update).unwrap_or(PutMode::Create),
        )
    }
    fn write(&self, key: &str, bytes: Vec<u8>, mode: PutMode) -> anyhow::Result<()> {
        validate_key(key)?;
        ensure!(bytes.len() <= MAX_OBJECT, "cache object exceeds size limit");
        match self {
            Self::S3(s3) => {
                s3.request(key, Operation::Put(bytes, mode))?;
            }
            Self::Filesystem(root) => {
                let target = confined(root, key, true)?;
                let parent = target.parent().context("object requires parent")?;
                if mode == PutMode::Create {
                    let mut staging = tempfile::NamedTempFile::new_in(parent)?;
                    use std::io::Write;
                    staging.write_all(&bytes)?;
                    staging.as_file().sync_all()?;
                    match staging.persist_noclobber(&target) {
                        Ok(_) => {}
                        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                            return Err(conflict());
                        }
                        Err(e) => return Err(e.error.into()),
                    }
                    fs::File::open(parent)?.sync_all()?;
                    return Ok(());
                }
                // Keep the lock file permanently: unlinking it would allow two
                // processes to lock different inodes for the same HEAD.
                let lock = OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(parent.join(format!(
                        ".{}.lock",
                        target.file_name().unwrap().to_string_lossy()
                    )))?;
                ensure!(
                    lock.metadata()?.is_file(),
                    "cache lock is not a regular file"
                );
                fs2::FileExt::lock_exclusive(&lock)?;
                match &mode {
                    PutMode::Create => {
                        if fs::symlink_metadata(&target).is_ok() {
                            return Err(conflict());
                        }
                    }
                    PutMode::Update(version) => {
                        let current = self.get_versioned(key)?;
                        if current.as_ref().map(|v| &v.version) != Some(version) {
                            return Err(conflict());
                        }
                    }
                    PutMode::Overwrite => {}
                }
                let mut staging = tempfile::NamedTempFile::new_in(parent)?;
                use std::io::Write;
                staging.write_all(&bytes)?;
                staging.as_file().sync_all()?;
                staging.persist(&target).map_err(|e| e.error)?;
                fs::File::open(parent)?.sync_all()?;
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_key(key: &str) -> anyhow::Result<()> {
    ensure!(
        !key.is_empty()
            && key.split('/').all(|part| !part.is_empty()
                && part != "."
                && part != ".."
                && !part.contains(['\\', '\0', '?', '#', '%'])),
        "invalid cache object key"
    );
    Ok(())
}
fn confined(root: &Path, key: &str, create: bool) -> anyhow::Result<PathBuf> {
    let mut path = root.to_path_buf();
    let parts: Vec<_> = key.split('/').collect();
    for part in &parts[..parts.len() - 1] {
        path.push(part);
        if create {
            match fs::create_dir(&path) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error.into()),
            }
        }
        match fs::symlink_metadata(&path) {
            Ok(metadata) => ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "cache object parent is not a directory"
            ),
            Err(error) if !create && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(path.join(parts[parts.len() - 1]));
            }
            Err(error) => return Err(error.into()),
        }
    }
    path.push(parts[parts.len() - 1]);
    Ok(path)
}

// Own the async runtime on a separate host thread. Blocking cache callers may
// already be in a Tokio context; creating/dropping or block_on there would panic.
pub(super) struct StoredObject {
    pub(super) bytes: Vec<u8>,
    pub(super) version: UpdateVersion,
}
pub(super) enum Operation {
    Get(Option<Range<u64>>),
    GetBounded(usize),
    Put(Vec<u8>, PutMode),
}
#[derive(Debug, thiserror::Error)]
#[error("cache publication conflict: HEAD changed or object already exists")]
struct Conflict;
fn conflict() -> anyhow::Error {
    Conflict.into()
}
pub(crate) fn is_conflict(error: &anyhow::Error) -> bool {
    error.downcast_ref::<Conflict>().is_some()
}
pub(crate) struct S3 {
    store: Arc<dyn ObjectStore>,
    prefix: String,
    runtime: Arc<super::s3_runtime::Runtime>,
}
impl S3 {
    pub(crate) fn new(store: Arc<dyn ObjectStore>, prefix: String) -> anyhow::Result<Self> {
        Ok(Self {
            store,
            prefix,
            runtime: super::s3_runtime::Runtime::shared()?,
        })
    }
    fn request(&self, key: &str, operation: Operation) -> anyhow::Result<Option<StoredObject>> {
        let key = if self.prefix.is_empty() {
            key.into()
        } else {
            format!("{}/{key}", self.prefix)
        };
        self.runtime.request(self.store.clone(), key, operation)
    }
}

pub(super) async fn s3_operation(
    store: &dyn ObjectStore,
    key: &str,
    operation: Operation,
) -> anyhow::Result<Option<StoredObject>> {
    let path = object_store::path::Path::from(key);
    match operation {
        Operation::Put(bytes, mode) => {
            let options = PutOptions {
                mode,
                ..Default::default()
            };
            match store.put_opts(&path, bytes.into(), options).await {
                Ok(_) => Ok(None),
                Err(
                    object_store::Error::AlreadyExists { .. }
                    | object_store::Error::Precondition { .. },
                ) => Err(conflict()),
                Err(error) => Err(error.into()),
            }
        }
        get @ (Operation::Get(_) | Operation::GetBounded(_)) => {
            let (range, limit) = match get {
                Operation::Get(range) => (range, MAX_OBJECT),
                Operation::GetBounded(limit) => (None, limit),
                _ => unreachable!(),
            };
            let expected = range.as_ref().map(|r| (r.end - r.start) as usize);
            let options = GetOptions {
                range: range.map(GetRange::Bounded),
                ..Default::default()
            };
            match store.get_opts(&path, options).await {
                Ok(result) => {
                    ensure!(
                        expected.is_some() || result.meta.size <= limit as u64,
                        "cache object exceeds size limit"
                    );
                    let version = UpdateVersion {
                        e_tag: result.meta.e_tag.clone(),
                        version: result.meta.version.clone(),
                    };
                    let bytes = result.bytes().await?;
                    ensure!(
                        bytes.len() <= limit && expected.is_none_or(|len| bytes.len() == len),
                        "truncated or oversized cache range"
                    );
                    Ok(Some(StoredObject {
                        bytes: bytes.to_vec(),
                        version,
                    }))
                }
                Err(object_store::Error::NotFound { .. }) => Ok(None),
                Err(error) => Err(error.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn many_layer_clients_share_one_io_thread_without_merging_stores_or_namespaces() {
        let endpoints: [Arc<dyn ObjectStore>; 2] = [
            Arc::new(object_store::memory::InMemory::new()),
            Arc::new(object_store::memory::InMemory::new()),
        ];
        let clients: Vec<_> = (0..64)
            .map(|i| {
                Arc::new(
                    S3::new(endpoints[i / 32].clone(), format!("team/layer-{}", i % 32)).unwrap(),
                )
            })
            .collect();
        let runtime = Arc::downgrade(&clients[0].runtime);
        assert!(
            clients
                .iter()
                .all(|client| Arc::ptr_eq(&clients[0].runtime, &client.runtime))
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            std::fs::read_dir("/proc/self/task")
                .unwrap()
                .filter(|entry| {
                    entry
                        .as_ref()
                        .ok()
                        .and_then(|entry| std::fs::read_to_string(entry.path().join("comm")).ok())
                        .is_some_and(|name| name.trim() == "pvisor-cache-s3")
                })
                .count(),
            1
        );
        let ready = Arc::new(std::sync::Barrier::new(clients.len()));
        let producers: Vec<_> = clients
            .iter()
            .enumerate()
            .map(|(i, client)| {
                let client = client.clone();
                let ready = ready.clone();
                std::thread::spawn(move || {
                    ready.wait();
                    let storage = Storage::S3(client);
                    let value = format!("endpoint-specific layer {i}").into_bytes();
                    storage.put("v1/chunk", value.clone(), true).unwrap();
                    assert_eq!(storage.get("v1/chunk").unwrap().unwrap(), value);
                    assert_eq!(storage.range("v1/chunk", 0..8).unwrap(), b"endpoint");
                })
            })
            .collect();
        for producer in producers {
            producer.join().unwrap();
        }
        // Check the same keys again after every endpoint's writers finish.
        for (i, client) in clients.iter().enumerate() {
            assert_eq!(
                Storage::S3(client.clone())
                    .get("v1/chunk")
                    .unwrap()
                    .unwrap(),
                format!("endpoint-specific layer {i}").as_bytes()
            );
        }
        drop(clients);
        assert!(
            runtime.upgrade().is_none(),
            "idle clients must release the shared runtime"
        );
    }
    #[tokio::test(flavor = "current_thread")]
    async fn s3_runtime_can_be_owned_and_dropped_from_an_existing_tokio_context() {
        let storage = Storage::S3(Arc::new(
            S3::new(
                Arc::new(object_store::memory::InMemory::new()),
                "team".into(),
            )
            .unwrap(),
        ));
        storage.put("v1/test", b"value".to_vec(), true).unwrap();
        storage.put("v1/test", b"other".to_vec(), true).unwrap();
        assert_eq!(storage.get("v1/test").unwrap().unwrap(), b"value");
        drop(storage);
    }
    #[test]
    fn filesystem_objects_reject_symlinks_and_non_regular_files() {
        let temp = tempfile::tempdir().unwrap();
        let storage = Storage::filesystem(temp.path().join("objects"), true).unwrap();
        let root = temp.path().join("objects");
        std::os::unix::fs::symlink(temp.path(), root.join("outside")).unwrap();
        assert!(storage.put("outside/escaped", vec![1], false).is_err());
        assert!(storage.get("outside/escaped").is_err());
        std::os::unix::fs::symlink("/etc/passwd", root.join("link")).unwrap();
        assert!(storage.get("link").is_err());
        assert!(storage.get("../outside").is_err());
    }
}

#[cfg(test)]
mod cas_tests {
    use super::*;
    #[test]
    fn conditional_head_updates_allow_exactly_one_writer_on_both_backends() {
        let temp = tempfile::tempdir().unwrap();
        let backends = [
            Storage::filesystem(temp.path().join("fs"), true).unwrap(),
            Storage::S3(Arc::new(
                S3::new(
                    Arc::new(object_store::memory::InMemory::new()),
                    String::new(),
                )
                .unwrap(),
            )),
        ];
        for storage in backends {
            storage
                .compare_and_swap(
                    "meta/image/platforms/linux-amd64/HEAD.json",
                    b"original".to_vec(),
                    None,
                )
                .unwrap();
            let version = storage
                .get_versioned("meta/image/platforms/linux-amd64/HEAD.json")
                .unwrap()
                .unwrap()
                .version;
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let workers: Vec<_> = (0..2)
                .map(|i| {
                    let storage = storage.clone();
                    let version = version.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        storage.compare_and_swap(
                            "meta/image/platforms/linux-amd64/HEAD.json",
                            vec![i],
                            Some(version),
                        )
                    })
                })
                .collect();
            let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
            assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
            assert!(
                results
                    .iter()
                    .filter_map(|r| r.as_ref().err())
                    .all(is_conflict)
            );
            assert!(
                storage
                    .compare_and_swap("meta/other/platforms/linux-amd64/HEAD.json", vec![2], None)
                    .is_ok(),
                "different images need no common publication state"
            );
        }
    }
}
