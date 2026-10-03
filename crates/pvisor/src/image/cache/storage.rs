//! Small object API shared by filesystem and S3 caches; keys are internal only.
use anyhow::{Context, ensure};
use object_store::{GetOptions, GetRange, ObjectStore, PutMode, PutOptions, UpdateVersion};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

pub(super) const MAX_OBJECT: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub(super) enum Storage {
    Filesystem(PathBuf),
    S3(Arc<S3>),
}
impl Storage {
    pub(super) fn filesystem(root: PathBuf, create: bool) -> anyhow::Result<Self> {
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
    pub(super) fn s3(location: &str) -> anyhow::Result<Self> {
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
        self.read(key, None)
    }
    pub(super) fn range(&self, key: &str, range: Range<u64>) -> anyhow::Result<Vec<u8>> {
        ensure!(
            range.end > range.start && range.end - range.start <= MAX_OBJECT as u64,
            "invalid cache range"
        );
        let expected = (range.end - range.start) as usize;
        let object = self
            .read(key, Some(range))?
            .context("missing cache object")?;
        ensure!(object.bytes.len() == expected, "truncated cache range");
        Ok(object.bytes)
    }
    fn read(&self, key: &str, range: Option<Range<u64>>) -> anyhow::Result<Option<StoredObject>> {
        validate_key(key)?;
        match self {
            Self::S3(s3) => s3.request(key, Operation::Get(range)),
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
                ensure!(
                    length <= MAX_OBJECT as u64,
                    "cache object exceeds size limit"
                );
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
    pub(super) fn compare_and_swap(
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

fn validate_key(key: &str) -> anyhow::Result<()> {
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
enum Operation {
    Get(Option<Range<u64>>),
    Put(Vec<u8>, PutMode),
}
#[derive(Debug, thiserror::Error)]
#[error("cache publication conflict: HEAD changed or object already exists")]
struct Conflict;
fn conflict() -> anyhow::Error {
    Conflict.into()
}
pub(super) fn is_conflict(error: &anyhow::Error) -> bool {
    error.downcast_ref::<Conflict>().is_some()
}
type ResultSender = mpsc::SyncSender<anyhow::Result<Option<StoredObject>>>;
struct Job {
    key: String,
    operation: Operation,
    reply: ResultSender,
}
pub(super) struct S3 {
    send: Option<mpsc::SyncSender<Job>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl S3 {
    fn new(store: Arc<dyn ObjectStore>, prefix: String) -> anyhow::Result<Self> {
        let (send, receive) = mpsc::sync_channel::<Job>(32);
        let (ready, initialized) = mpsc::sync_channel::<anyhow::Result<()>>(1);
        let worker = std::thread::Builder::new()
            .name("pvisor-cache-s3".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready.send(Err(error.into()));
                        return;
                    }
                };
                let _ = ready.send(Ok(()));
                let permits = Arc::new(tokio::sync::Semaphore::new(32));
                while let Ok(job) = receive.recv() {
                    let permit = match runtime.block_on(permits.clone().acquire_owned()) {
                        Ok(permit) => permit,
                        Err(_) => break,
                    };
                    let store = store.clone();
                    let key = if prefix.is_empty() {
                        job.key.clone()
                    } else {
                        format!("{prefix}/{}", job.key)
                    };
                    runtime.spawn(async move {
                        let _permit = permit;
                        let result = s3_operation(store.as_ref(), &key, job.operation).await;
                        let _ = job.reply.send(result);
                    });
                }
                runtime.shutdown_timeout(std::time::Duration::from_secs(10));
            })?;
        initialized
            .recv()
            .context("S3 cache runtime initialization failed")??;
        Ok(Self {
            send: Some(send),
            worker: Some(worker),
        })
    }
    fn request(&self, key: &str, operation: Operation) -> anyhow::Result<Option<StoredObject>> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.send
            .as_ref()
            .context("S3 cache worker stopped")?
            .send(Job {
                key: key.into(),
                operation,
                reply,
            })
            .context("S3 cache worker stopped")?;
        receive.recv().context("S3 cache worker dropped response")?
    }
}
impl Drop for S3 {
    fn drop(&mut self) {
        self.send.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

async fn s3_operation(
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
        Operation::Get(range) => {
            let expected = range.as_ref().map(|r| (r.end - r.start) as usize);
            let options = GetOptions {
                range: range.map(GetRange::Bounded),
                ..Default::default()
            };
            match store.get_opts(&path, options).await {
                Ok(result) => {
                    ensure!(
                        expected.is_some() || result.meta.size <= MAX_OBJECT as u64,
                        "cache object exceeds size limit"
                    );
                    let version = UpdateVersion {
                        e_tag: result.meta.e_tag.clone(),
                        version: result.meta.version.clone(),
                    };
                    let bytes = result.bytes().await?;
                    ensure!(
                        bytes.len() <= MAX_OBJECT && expected.is_none_or(|len| bytes.len() == len),
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
