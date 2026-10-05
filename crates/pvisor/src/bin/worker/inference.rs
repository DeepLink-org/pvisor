//! Attempt-bound cooperative waits; control execution stays in the normal
//! Worker poll loop, including final CPU admission before native resume.
use super::*;
use pvisor_gateway::model_wait::{ModelWait, ModelWaitLifecycle};
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

#[derive(Clone)]
pub(crate) struct Binding {
    pub client: Client,
    pub key: LeaseKey,
    pub stop: watch::Receiver<bool>,
    pub clock: watch::Receiver<Instant>,
    pub live: watch::Receiver<()>,
}

pub(crate) struct Lifecycle {
    pub binding: Binding,
    pub run_id: pvisor_core::RunId,
    group: std::sync::Mutex<Weak<Group>>,
    revision: AtomicU64,
}

impl std::fmt::Debug for Lifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InferenceLifecycle")
            .field("lease", &self.binding.key)
            .finish_non_exhaustive()
    }
}

impl Lifecycle {
    pub fn new(binding: Binding, run_id: pvisor_core::RunId) -> Self {
        Self {
            binding,
            run_id,
            group: std::sync::Mutex::new(Weak::new()),
            revision: AtomicU64::new(0),
        }
    }
}

#[derive(Default)]
struct Progress {
    begun: bool,
    entered: bool,
}
struct Group {
    binding: Binding,
    key: InferenceWaitKey,
    progress: tokio::sync::Mutex<Progress>,
    members: AtomicUsize,
    ready_confirmed: AtomicBool,
    cleanup_started: AtomicBool,
}
struct Wait {
    group: Arc<Group>,
    member: bool,
}

impl ModelWaitLifecycle for Lifecycle {
    fn reserve(
        &self,
        request: pvisor_core::ModelCallRequest,
    ) -> anyhow::Result<Box<dyn ModelWait>> {
        ensure!(
            request.run_id.as_ref() == Some(&self.run_id),
            "inference wait belongs to another Run"
        );
        let mut current = self
            .group
            .lock()
            .map_err(|_| anyhow::anyhow!("inference group poisoned"))?;
        let group = if let Some(group) = current.upgrade() {
            group
        } else {
            let revision = self
                .revision
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |r| r.checked_add(1))
                .map_err(|_| anyhow::anyhow!("inference revision overflow"))?
                + 1;
            let group = Arc::new(Group {
                binding: self.binding.clone(),
                key: InferenceWaitKey {
                    lease: self.binding.key.clone(),
                    revision,
                    call_id: request.call_id,
                },
                progress: tokio::sync::Mutex::new(Progress::default()),
                members: AtomicUsize::new(0),
                ready_confirmed: AtomicBool::new(false),
                cleanup_started: AtomicBool::new(false),
            });
            *current = Arc::downgrade(&group);
            group
        };
        group
            .members
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < 64).then_some(n + 1)
            })
            .map_err(|_| anyhow::anyhow!("inference group has 64 active calls"))?;
        Ok(Box::new(Wait {
            group,
            member: true,
        }))
    }
}

impl Group {
    async fn query(&self, intent: InferenceWaitIntent) -> anyhow::Result<InferenceWaitReceipt> {
        let request = InferenceWaitRequest {
            key: self.key.clone(),
            intent,
        };
        loop {
            let mut stop = self.binding.stop.clone();
            let mut live = self.binding.live.clone();
            ensure!(
                !live.has_changed().unwrap_or(true),
                "native inference execution ended"
            );
            ensure!(
                !*stop.borrow() && Instant::now() < *self.binding.clock.borrow(),
                "inference lease ended"
            );
            let result = tokio::select! {
                biased;
                _ = stop.changed() => anyhow::bail!("inference execution stopped"),
                _ = live.changed() => anyhow::bail!("native inference execution ended"),
                _ = lease_expired(self.binding.clock.clone()) => anyhow::bail!("inference lease expired"),
                result = self.binding.client.inference_wait(&request) => result,
            };
            match result {
                Ok(receipt) => {
                    ensure!(
                        receipt.record.key == self.key,
                        "controller returned another inference wait"
                    );
                    return Ok(receipt);
                }
                Err(error) if outbox::retryable(&error) || outbox::conflict(&error) => {
                    self.delay().await?
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn delay(&self) -> anyhow::Result<()> {
        let mut stop = self.binding.stop.clone();
        let mut live = self.binding.live.clone();
        ensure!(
            !live.has_changed().unwrap_or(true),
            "native inference execution ended"
        );
        ensure!(!*stop.borrow(), "inference execution stopped");
        tokio::select! {
            biased;
            _ = stop.changed() => anyhow::bail!("inference execution stopped"),
            _ = live.changed() => anyhow::bail!("native inference execution ended"),
            _ = lease_expired(self.binding.clock.clone()) => anyhow::bail!("inference lease expired"),
            _ = tokio::time::sleep(Duration::from_millis(50)) => Ok(()),
        }
    }

    fn cleanup(self: &Arc<Self>) {
        if self.ready_confirmed.load(Ordering::SeqCst)
            || self.cleanup_started.swap(true, Ordering::SeqCst)
        {
            return;
        }
        let group = self.clone();
        // At most one cleanup per group. Ready-before-Begin is a durable
        // tombstone, so uncertain publication cannot create a late pause.
        tokio::spawn(async move {
            let _progress = group.progress.lock().await;
            match group.query(InferenceWaitIntent::Ready).await {
                Ok(_) => group.ready_confirmed.store(true, Ordering::SeqCst),
                Err(error) => eprintln!(
                    "inference cleanup for {} ended: {error:#}",
                    group.key.lease.task_id
                ),
            }
        });
    }
}

#[async_trait::async_trait]
impl ModelWait for Wait {
    async fn enter(&mut self) -> anyhow::Result<()> {
        ensure!(self.member, "inference member was cancelled");
        let mut progress = self.group.progress.lock().await;
        while !progress.entered {
            let intent = if progress.begun {
                InferenceWaitIntent::Observe
            } else {
                InferenceWaitIntent::Begin
            };
            let receipt = self.group.query(intent).await?;
            progress.begun = true;
            progress.entered = receipt.entered;
            if !progress.entered {
                self.group.delay().await?;
            }
        }
        Ok(())
    }
    async fn before_delivery(&mut self) -> anyhow::Result<()> {
        ensure!(self.member, "inference member was cancelled");
        let _progress = self.group.progress.lock().await;
        loop {
            let receipt = self.group.query(InferenceWaitIntent::Ready).await?;
            self.group.ready_confirmed.store(true, Ordering::SeqCst);
            if receipt.delivery_ready {
                return Ok(());
            }
            self.group.delay().await?;
        }
    }
    fn cancel(&mut self) {
        self.release();
    }
}

impl Wait {
    fn release(&mut self) {
        if self.member {
            self.member = false;
            if self.group.members.fetch_sub(1, Ordering::SeqCst) == 1 {
                self.group.cleanup();
            }
        }
    }
}

impl Drop for Wait {
    fn drop(&mut self) {
        self.release();
    }
}
