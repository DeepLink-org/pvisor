//! Small object API shared by filesystem and S3 caches; keys are internal only.
use anyhow::{Context, ensure};
use object_store::{ObjectStore, ObjectStoreExt, PutMode, PutOptions};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

pub(super) const MAX_OBJECT: usize = 64 * 1024 * 1024;

pub(super) enum Storage {
    Filesystem(PathBuf),
    S3(S3),
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
        Ok(Self::S3(S3::new(Arc::new(store), prefix.into())?))
    }
    pub(super) fn get(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        validate_key(key)?;
        match self {
            Self::S3(s3) => s3.request(key, None, false),
            Self::Filesystem(root) => {
                let path = confined(root, key, false)?;
                let file = match OpenOptions::new()
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
                let mut body = Vec::new();
                file.take((MAX_OBJECT + 1) as u64).read_to_end(&mut body)?;
                ensure!(body.len() <= MAX_OBJECT, "cache object exceeds size limit");
                Ok(Some(body))
            }
        }
    }
    pub(super) fn put(&self, key: &str, bytes: Vec<u8>, immutable: bool) -> anyhow::Result<()> {
        validate_key(key)?;
        ensure!(bytes.len() <= MAX_OBJECT, "cache object exceeds size limit");
        match self {
            Self::S3(s3) => {
                s3.request(key, Some(bytes), immutable)?;
            }
            Self::Filesystem(root) => {
                let target = confined(root, key, true)?;
                let parent = target.parent().context("object requires parent")?;
                let mut staging = tempfile::NamedTempFile::new_in(parent)?;
                use std::io::Write;
                staging.write_all(&bytes)?;
                staging.as_file().sync_all()?;
                if immutable {
                    match staging.persist_noclobber(&target) {
                        Ok(_) => (),
                        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => (),
                        Err(error) => return Err(error.error.into()),
                    }
                } else {
                    staging.persist(&target).map_err(|e| e.error)?;
                }
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
type ResultSender = mpsc::SyncSender<anyhow::Result<Option<Vec<u8>>>>;
struct Job {
    key: String,
    bytes: Option<Vec<u8>>,
    immutable: bool,
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
                        let result =
                            s3_operation(store.as_ref(), &key, job.bytes, job.immutable).await;
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
    fn request(
        &self,
        key: &str,
        bytes: Option<Vec<u8>>,
        immutable: bool,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let (reply, receive) = mpsc::sync_channel(1);
        self.send
            .as_ref()
            .context("S3 cache worker stopped")?
            .send(Job {
                key: key.into(),
                bytes,
                immutable,
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
    bytes: Option<Vec<u8>>,
    immutable: bool,
) -> anyhow::Result<Option<Vec<u8>>> {
    let path = object_store::path::Path::from(key);
    if let Some(bytes) = bytes {
        let options = PutOptions {
            mode: if immutable {
                PutMode::Create
            } else {
                PutMode::Overwrite
            },
            ..Default::default()
        };
        match store.put_opts(&path, bytes.into(), options).await {
            Ok(_) => Ok(None),
            Err(
                object_store::Error::AlreadyExists { .. }
                | object_store::Error::Precondition { .. },
            ) if immutable => Ok(None),
            Err(error) => Err(error.into()),
        }
    } else {
        match store.get(&path).await {
            Ok(result) => {
                ensure!(
                    result.meta.size <= MAX_OBJECT as u64,
                    "cache object exceeds size limit"
                );
                let bytes = result.bytes().await?;
                ensure!(bytes.len() <= MAX_OBJECT, "cache object exceeds size limit");
                Ok(Some(bytes.to_vec()))
            }
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn s3_runtime_can_be_owned_and_dropped_from_an_existing_tokio_context() {
        let storage = Storage::S3(
            S3::new(
                Arc::new(object_store::memory::InMemory::new()),
                "team".into(),
            )
            .unwrap(),
        );
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
