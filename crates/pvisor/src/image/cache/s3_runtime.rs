//! One bounded host I/O loop per process. Clients retain their own ObjectStore
//! (credentials, endpoint, retry policy) and namespace; only execution is pooled.
use super::storage::{Operation, StoredObject, s3_operation};
use anyhow::{Context, ensure};
use object_store::ObjectStore;
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak, mpsc};

const MAX_REQUESTS: usize = 32;
static SHARED: OnceLock<Mutex<Weak<Runtime>>> = OnceLock::new();
#[derive(Default)]
struct Admission {
    active: Mutex<usize>,
    available: Condvar,
}
impl Admission {
    fn acquire(self: &Arc<Self>) -> anyhow::Result<Permit> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| anyhow::anyhow!("S3 cache admission unavailable"))?;
        while *active >= MAX_REQUESTS {
            active = self
                .available
                .wait(active)
                .map_err(|_| anyhow::anyhow!("S3 cache admission unavailable"))?;
        }
        *active += 1;
        Ok(Permit(self.clone()))
    }
}
struct Permit(Arc<Admission>);
impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut active) = self.0.active.lock() {
            *active -= 1;
            self.0.available.notify_one();
        }
    }
}
struct Job {
    store: Arc<dyn ObjectStore>,
    key: String,
    operation: Operation,
    reply: mpsc::SyncSender<anyhow::Result<Option<StoredObject>>>,
    _permit: Permit,
}
pub(super) struct Runtime {
    pid: u32,
    admission: Arc<Admission>,
    send: Option<tokio::sync::mpsc::UnboundedSender<Job>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Runtime {
    pub(super) fn shared() -> anyhow::Result<Arc<Self>> {
        let mut shared = SHARED
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| anyhow::anyhow!("S3 cache runtime registry unavailable"))?;
        if let Some(runtime) = shared.upgrade()
            && runtime.pid == std::process::id()
        {
            return Ok(runtime);
        }
        let runtime = Arc::new(Self::new()?);
        *shared = Arc::downgrade(&runtime);
        Ok(runtime)
    }
    fn new() -> anyhow::Result<Self> {
        // The admission permit bounds queued plus executing jobs. UnboundedSender
        // itself never blocks or requires entering the caller's Tokio runtime.
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel::<Job>();
        let (ready, initialized) = mpsc::sync_channel::<anyhow::Result<()>>(1);
        let worker = std::thread::Builder::new().name("pvisor-cache-s3".into()).spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => { let _ = ready.send(Err(error.into())); return; }
            };
            let _ = ready.send(Ok(()));
            runtime.block_on(async move {
                let mut operations = tokio::task::JoinSet::new();
                loop {
                    tokio::select! {
                        job = receive.recv() => match job {
                            Some(job) => { operations.spawn(async move {
                                let result = s3_operation(job.store.as_ref(), &job.key, job.operation).await;
                                let _ = job.reply.send(result);
                                // Keep admission charged until response publication.
                                drop(job._permit);
                            }); },
                            None => break,
                        },
                        _ = operations.join_next(), if !operations.is_empty() => {}
                    }
                }
                // The final client may drop immediately after receiving a result.
                // Join remaining completions before releasing the I/O runtime.
                while operations.join_next().await.is_some() {}
            });
        })?;
        match initialized
            .recv()
            .context("S3 cache runtime initialization failed")?
        {
            Ok(()) => Ok(Self {
                pid: std::process::id(),
                admission: Arc::new(Admission::default()),
                send: Some(send),
                worker: Some(worker),
            }),
            Err(error) => {
                let _ = worker.join();
                Err(error)
            }
        }
    }
    pub(super) fn request(
        &self,
        store: Arc<dyn ObjectStore>,
        key: String,
        operation: Operation,
    ) -> anyhow::Result<Option<StoredObject>> {
        ensure!(
            self.pid == std::process::id(),
            "S3 cache client inherited across fork; reopen it in the child"
        );
        ensure!(
            self.worker
                .as_ref()
                .is_none_or(|worker| worker.thread().id() != std::thread::current().id()),
            "recursive synchronous S3 cache request on the I/O loop"
        );
        let permit = self.admission.acquire()?;
        let (reply, receive) = mpsc::sync_channel(1);
        self.send
            .as_ref()
            .context("S3 cache worker stopped")?
            .send(Job {
                store,
                key,
                operation,
                reply,
                _permit: permit,
            })
            .map_err(|_| anyhow::anyhow!("S3 cache worker stopped"))?;
        receive.recv().context("S3 cache worker dropped response")?
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.send.take();
        if let Some(worker) = self.worker.take()
            && self.pid == std::process::id()
            && worker.thread().id() != std::thread::current().id()
        {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
#[path = "s3_runtime_tests.rs"]
mod tests;
