//! Per-`story_id` ordered capture apply queue — preserves event order within a story.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc as std_mpsc};

use std::collections::HashMap;

use super::actors::StoryActor;
use super::story::{Story, StoryId};
use super::wire::{LocalStoryCommand, StoryReply};
use tokio::sync::{mpsc, oneshot};

use super::coordinator::CaptureRuntimeInner;
use super::{CallContext, Event};
use crate::dead_letter;

const APPLY_QUEUE_CAPACITY: usize = 256;
const REJECTED_EVENT_QUEUE_CAPACITY: usize = 256;
const MAX_RETAINED_STORIES: usize = 256;
const MAX_EXTERNAL_JOBS: usize = 1024;
const MAX_EXTERNAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_INTERNAL_JOBS: usize = 256;
const MAX_INTERNAL_BYTES: usize = 32 * 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
struct BudgetUsage {
    jobs: usize,
    bytes: usize,
}

struct JobBudget {
    usage: Mutex<BudgetUsage>,
    max_jobs: usize,
    max_bytes: usize,
}

impl JobBudget {
    fn new(max_jobs: usize, max_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            usage: Mutex::new(BudgetUsage::default()),
            max_jobs,
            max_bytes,
        })
    }

    // Never wait for global capacity: a worker may retain its permit while
    // awaiting a backfill. Waiting here could make admission a dependency cycle.
    fn admit(self: &Arc<Self>, bytes: usize) -> anyhow::Result<JobPermit> {
        let mut usage = self.usage.lock().unwrap();
        if usage.jobs >= self.max_jobs || bytes > self.max_bytes.saturating_sub(usage.bytes) {
            anyhow::bail!("capture global job/byte budget exhausted");
        }
        usage.jobs += 1;
        usage.bytes += bytes;
        Ok(JobPermit {
            budget: Arc::clone(self),
            bytes,
        })
    }
}

struct JobPermit {
    budget: Arc<JobBudget>,
    bytes: usize,
}

impl Drop for JobPermit {
    fn drop(&mut self) {
        let mut usage = self.budget.usage.lock().unwrap();
        usage.jobs -= 1;
        usage.bytes -= self.bytes;
    }
}

// Account serialized payload bytes without allocating a temporary JSON tree or
// buffer. A conservative multiplier covers JSON/typed node overhead; this is
// an admission budget, not a process RSS limit (projections also retain state).
fn payload_bytes(value: &impl serde::Serialize) -> usize {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    if serde_json::to_writer(&mut counter, value).is_err() {
        return usize::MAX;
    }
    counter.0.saturating_mul(16)
}

fn capture_bytes(ctx: &CallContext, event: &Event) -> usize {
    let context = payload_bytes(&(
        &ctx.story,
        &ctx.call,
        &ctx.request_headers,
        &ctx.client_model,
        &ctx.upstream_model,
        &ctx.client_peer,
        &ctx.client_meta,
        &ctx.http_version,
        &ctx.upstream_url,
    ));
    let content = match event {
        Event::Request(e) => payload_bytes(&(
            &e.path,
            &e.method,
            &e.url,
            &e.user_content,
            &e.body_json,
            e.semantic.as_deref(),
            &e.headers,
        )),
        Event::ResponseComplete(e) => {
            e.resp_bytes
                .len()
                .saturating_mul(16)
                .saturating_add(payload_bytes(&(
                    &e.assistant_content,
                    e.semantic.as_deref(),
                    &e.headers,
                )))
        }
        Event::ResponseDraft(e) => e.assistant_content.capacity(),
        Event::Cancelled(e) => e.reason.as_ref().map_or(0, String::capacity),
    };
    context.saturating_add(content).saturating_add(4096)
}

struct AdmittedJob {
    job: ApplyJob,
    // Held through preparation, persistence and dependent backfill receipts.
    _permit: Option<JobPermit>,
}

impl From<ApplyJob> for AdmittedJob {
    fn from(job: ApplyJob) -> Self {
        Self { job, _permit: None }
    }
}

enum RejectedEventMessage {
    Event {
        ctx: Arc<CallContext>,
        event: Event,
        _permit: JobPermit,
    },
    Barrier {
        ack: std_mpsc::SyncSender<anyhow::Result<()>>,
    },
    Finish,
}

struct RejectedEventWriter {
    budget: Arc<JobBudget>,
    tx: std_mpsc::SyncSender<RejectedEventMessage>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl RejectedEventWriter {
    fn new(storage: Arc<PathBuf>) -> Self {
        let (tx, rx) = std_mpsc::sync_channel(REJECTED_EVENT_QUEUE_CAPACITY);
        let worker = std::thread::Builder::new()
            .name("pvisor-dead-letter".to_string())
            .spawn(move || {
                let mut failure = None;
                while let Ok(message) = rx.recv() {
                    match message {
                        RejectedEventMessage::Event {
                            ctx,
                            event,
                            _permit,
                        } => {
                            if let Err(error) = dead_letter::append_dead_letter(
                                storage.as_path(),
                                &ctx,
                                &event,
                                "apply queue full or closed",
                                None,
                            ) {
                                failure.get_or_insert_with(|| format!("{error:#}"));
                                tracing::error!("dead letter write failed: {error:#}");
                            }
                        }
                        RejectedEventMessage::Barrier { ack } => {
                            let result = match &failure {
                                Some(error) => {
                                    Err(anyhow::anyhow!("dead letter write failed: {error}"))
                                }
                                None => Ok(()),
                            };
                            let _ = ack.send(result);
                        }
                        RejectedEventMessage::Finish => break,
                    }
                }
            })
            .expect("dead-letter writer thread must start with capture runtime");
        Self {
            budget: JobBudget::new(REJECTED_EVENT_QUEUE_CAPACITY, MAX_DIAGNOSTIC_BYTES),
            tx,
            worker: Mutex::new(Some(worker)),
        }
    }

    /// Complete appends queued before the marker, reporting writer I/O errors.
    /// Called on the blocking pool; unlike Drop this does not stop the writer.
    fn drain(&self) -> anyhow::Result<()> {
        let (ack, done) = std_mpsc::sync_channel(1);
        self.tx
            .send(RejectedEventMessage::Barrier { ack })
            .map_err(|_| anyhow::anyhow!("dead letter writer closed before drain"))?;
        done.recv()
            .map_err(|_| anyhow::anyhow!("dead letter drain reply dropped"))?
    }

    fn try_record(&self, ctx: Arc<CallContext>, event: Event) {
        let Ok(permit) = self.budget.admit(capture_bytes(&ctx, &event)) else {
            tracing::error!(
                "dead-letter byte/job budget exhausted; rejected capture gap remains reported by flush/shutdown"
            );
            return;
        };
        if let Err(error) = self.tx.try_send(RejectedEventMessage::Event {
            ctx,
            event,
            _permit: permit,
        }) {
            tracing::warn!(
                target: "pvisor_gateway",
                "dead-letter queue rejected overloaded capture event: {error}"
            );
        }
    }
}

impl Drop for RejectedEventWriter {
    fn drop(&mut self) {
        let _ = self.tx.send(RejectedEventMessage::Finish);
        if let Some(worker) = self.worker.lock().expect("dead-letter worker mutex").take() {
            let _ = worker.join();
        }
    }
}

// Each admitted job holds the scheduler alive until it finishes. Idle workers
// hold no scheduler reference, so dropping the runtime still drains accepted
// work (including cross-story backfills) without a permanent sender/task cycle.
enum ApplyJob {
    Capture {
        owner: Arc<SchedulingOwner>,
        ctx: Arc<CallContext>,
        event: Event,
        ack: Option<oneshot::Sender<anyhow::Result<()>>>,
    },
    Command {
        owner: Arc<SchedulingOwner>,
        command: Box<LocalStoryCommand>,
        ack: oneshot::Sender<anyhow::Result<StoryReply>>,
    },
    Stop {
        owner: Arc<SchedulingOwner>,
        ack: oneshot::Sender<anyhow::Result<StoryReply>>,
    },
}

impl ApplyJob {
    fn accounted_bytes(&self) -> usize {
        match self {
            Self::Capture { ctx, event, .. } => capture_bytes(ctx, event),
            Self::Command { command, .. } => {
                let bytes = match command.as_ref() {
                    LocalStoryCommand::PersistRecord { scope, record }
                    | LocalStoryCommand::Restore { scope, record } => {
                        payload_bytes(&(&scope.context, record))
                    }
                    LocalStoryCommand::Snapshot { scope } => payload_bytes(&scope.context),
                    LocalStoryCommand::Flush | LocalStoryCommand::LocalSnapshot => 0,
                };
                bytes.saturating_add(4096)
            }
            Self::Stop { .. } => 4096,
        }
    }
}

struct SchedulingState {
    accepting: bool,
    stopped: bool,
    queues: HashMap<String, mpsc::Sender<AdmittedJob>>,
    workers: Vec<tokio::task::JoinHandle<()>>,
}

struct SchedulingOwner {
    external: Arc<JobBudget>,
    internal: Arc<JobBudget>,
    state: Mutex<SchedulingState>,
    failure: Mutex<Option<String>>,
    rejected: RejectedEventWriter,
}

/// The only bounded FIFO and worker per story. The worker owns story state and
/// performs preparation before applying each job; no second actor mailbox.
#[derive(Clone)]
pub(crate) struct ApplyDispatcher {
    inner: Arc<CaptureRuntimeInner>,
    owner: Arc<SchedulingOwner>,
}

impl ApplyDispatcher {
    pub(crate) fn new(inner: Arc<CaptureRuntimeInner>) -> Self {
        let rejected = RejectedEventWriter::new(Arc::clone(&inner.story_deps.storage));
        Self {
            inner,
            owner: Arc::new(SchedulingOwner {
                external: JobBudget::new(MAX_EXTERNAL_JOBS, MAX_EXTERNAL_BYTES),
                internal: JobBudget::new(MAX_INTERNAL_JOBS, MAX_INTERNAL_BYTES),
                state: Mutex::new(SchedulingState {
                    accepting: true,
                    stopped: false,
                    queues: HashMap::new(),
                    workers: Vec::new(),
                }),
                failure: Mutex::new(None),
                rejected,
            }),
        }
    }

    pub(crate) fn enqueue(&self, ctx: Arc<CallContext>, event: Event) {
        let bytes = capture_bytes(&ctx, &event);
        let mut state = self.owner.state.lock().unwrap();
        let job = ApplyJob::Capture {
            owner: Arc::clone(&self.owner),
            ctx: Arc::clone(&ctx),
            event,
            ack: None,
        };
        if !state.accepting {
            drop(state);
            self.record_rejected_job(job);
            return;
        }
        let admitted = self.owner.external.admit(bytes);
        let tx = admitted.as_ref().map_err(|_| ()).and_then(|_| {
            self.queue(&mut state, ctx.story_id().as_str())
                .map_err(|_| ())
        });
        let rejected = match (admitted, tx) {
            (Ok(permit), Ok(tx)) => tx
                .try_send(AdmittedJob {
                    job,
                    _permit: Some(permit),
                })
                .err()
                .map(|error| error.into_inner().job),
            _ => Some(job),
        };
        drop(state);
        if let Some(job) = rejected {
            self.record_rejected_job(job);
        }
    }

    pub(crate) async fn apply(&self, ctx: Arc<CallContext>, event: Event) -> anyhow::Result<()> {
        let story_id = ctx.story_id().as_str().to_string();
        let (ack, done) = oneshot::channel();
        self.send(
            &story_id,
            ApplyJob::Capture {
                owner: Arc::clone(&self.owner),
                ctx,
                event,
                ack: Some(ack),
            },
            false,
        )
        .await?;
        done.await
            .map_err(|_| anyhow::anyhow!("capture apply reply dropped"))?
    }

    pub(crate) async fn command(
        &self,
        story_id: &str,
        command: LocalStoryCommand,
    ) -> anyhow::Result<StoryReply> {
        self.command_with_admission(story_id, command, false).await
    }

    pub(crate) async fn command_internal(
        &self,
        story_id: &str,
        command: LocalStoryCommand,
    ) -> anyhow::Result<StoryReply> {
        self.command_with_admission(story_id, command, true).await
    }

    async fn command_with_admission(
        &self,
        story_id: &str,
        command: LocalStoryCommand,
        internal: bool,
    ) -> anyhow::Result<StoryReply> {
        let (ack, done) = oneshot::channel();
        self.send(
            story_id,
            ApplyJob::Command {
                owner: Arc::clone(&self.owner),
                command: Box::new(command),
                ack,
            },
            internal,
        )
        .await?;
        done.await
            .map_err(|_| anyhow::anyhow!("story command reply dropped"))?
    }

    async fn send(&self, story_id: &str, job: ApplyJob, internal: bool) -> anyhow::Result<()> {
        let budget = if internal {
            &self.owner.internal
        } else {
            &self.owner.external
        };
        let job_permit = budget.admit(
            job.accounted_bytes()
                .saturating_add(story_id.len().saturating_mul(16)),
        )?;
        let tx = {
            let mut state = self.owner.state.lock().unwrap();
            if state.stopped || (!internal && !state.accepting) {
                anyhow::bail!("capture scheduling owner is shutting down");
            }
            self.queue(&mut state, story_id)?
        };
        // Reserve outside the lock. Admission is linearized with shutdown only
        // when the reserved job is sent, not when a producer starts waiting.
        let permit = tx
            .reserve_owned()
            .await
            .map_err(|_| anyhow::anyhow!("story queue closed"))?;
        let state = self.owner.state.lock().unwrap();
        if state.stopped || (!internal && !state.accepting) {
            anyhow::bail!("capture scheduling owner is shutting down");
        }
        permit.send(AdmittedJob {
            job,
            _permit: Some(job_permit),
        });
        Ok(())
    }

    /// Barrier each owner present at entry. Captures await their cross-story
    /// backfill receipts before passing a barrier, even for a newly made owner.
    pub(crate) async fn flush(&self) -> anyhow::Result<()> {
        let queues: Vec<_> = self
            .owner
            .state
            .lock()
            .unwrap()
            .queues
            .values()
            .cloned()
            .collect();
        let mut error = None;
        for tx in queues {
            let (ack, done) = oneshot::channel();
            let result = async {
                tx.send(
                    ApplyJob::Command {
                        owner: Arc::clone(&self.owner),
                        command: Box::new(LocalStoryCommand::Flush),
                        ack,
                    }
                    .into(),
                )
                .await
                .map_err(|_| anyhow::anyhow!("story queue closed while flushing"))?;
                super::coordinator::story_reply_ack(
                    done.await
                        .map_err(|_| anyhow::anyhow!("story queue barrier dropped"))??,
                )
            }
            .await;
            if let Err(failure) = result {
                error.get_or_insert(failure);
            }
        }
        if let Some(failure) = self.owner.failure.lock().unwrap().as_ref() {
            error.get_or_insert_with(|| {
                anyhow::anyhow!("capture has uncommitted events: {failure}")
            });
        }
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Close external admission first, but leave internal backfill admission
    /// open until accepted captures finish. Stop replies contain final snapshots.
    pub(crate) async fn shutdown(&self) -> (anyhow::Result<()>, HashMap<String, Story>) {
        self.owner.state.lock().unwrap().accepting = false;
        let mut result = self.flush().await;
        let (queues, workers) = {
            let mut state = self.owner.state.lock().unwrap();
            state.stopped = true;
            (
                std::mem::take(&mut state.queues),
                std::mem::take(&mut state.workers),
            )
        };
        let mut snapshots = HashMap::new();
        for (_, tx) in queues {
            let (ack, done) = oneshot::channel();
            let stopped = async {
                tx.send(
                    ApplyJob::Stop {
                        owner: Arc::clone(&self.owner),
                        ack,
                    }
                    .into(),
                )
                .await
                .map_err(|_| anyhow::anyhow!("story queue closed while stopping"))?;
                done.await
                    .map_err(|_| anyhow::anyhow!("story stop reply dropped"))?
            }
            .await;
            match stopped {
                Ok(StoryReply::LocalSnapshot {
                    storage_session_id,
                    story,
                }) => {
                    snapshots.insert(storage_session_id, story);
                }
                Ok(_) => {
                    result = Err(anyhow::anyhow!("unexpected story stop reply"));
                }
                Err(error) => {
                    result = Err(error);
                }
            }
        }
        for worker in workers {
            if let Err(error) = worker.await {
                result = Err(error.into());
            }
        }
        // A runtime clone can keep the writer alive beyond shutdown. Its Drop
        // join is not a shutdown barrier; explicitly await an ordered marker.
        let owner = Arc::clone(&self.owner);
        let diagnostics = tokio::task::spawn_blocking(move || owner.rejected.drain())
            .await
            .map_err(anyhow::Error::from)
            .and_then(|result| result);
        if let Err(error) = diagnostics {
            tracing::error!("shutdown dead letter drain failed: {error:#}");
            if result.is_ok() {
                result = Err(error);
            }
        }
        (result, snapshots)
    }

    fn queue(
        &self,
        state: &mut SchedulingState,
        story_id: &str,
    ) -> anyhow::Result<mpsc::Sender<AdmittedJob>> {
        if let Some(tx) = state.queues.get(story_id) {
            return Ok(tx.clone());
        }
        if state.queues.len() >= MAX_RETAINED_STORIES {
            anyhow::bail!("capture retained story budget exhausted");
        }
        let (tx, mut rx) = mpsc::channel::<AdmittedJob>(APPLY_QUEUE_CAPACITY);
        let inner = Arc::clone(&self.inner);
        let mut story = StoryActor::new(StoryId::new(story_id), inner.story_deps.clone());
        let worker = tokio::spawn(async move {
            while let Some(AdmittedJob { job, _permit }) = rx.recv().await {
                match job {
                    ApplyJob::Capture {
                        owner,
                        ctx,
                        event,
                        ack,
                    } => {
                        let dispatcher = Self {
                            inner: Arc::clone(&inner),
                            owner,
                        };
                        let result = inner
                            .apply_to_story(&dispatcher, &mut story, &ctx, event)
                            .await;
                        if let Err(error) = &result {
                            dispatcher.record_failure(error);
                            tracing::warn!(target: "pvisor_gateway", "capture apply: {error:#}");
                        }
                        if let Some(ack) = ack {
                            let _ = ack.send(result);
                        }
                    }
                    ApplyJob::Command {
                        owner,
                        command,
                        ack,
                    } => {
                        let result = story.handle(*command).await;
                        if let Err(error) = &result {
                            owner
                                .failure
                                .lock()
                                .unwrap()
                                .get_or_insert_with(|| format!("{error:#}"));
                        }
                        let _ = ack.send(result);
                    }
                    ApplyJob::Stop { owner: _owner, ack } => {
                        let _ = ack.send(story.handle(LocalStoryCommand::LocalSnapshot).await);
                        break;
                    }
                }
            }
        });
        state.workers.push(worker);
        state.queues.insert(story_id.to_string(), tx.clone());
        Ok(tx)
    }

    pub(crate) fn record_failure(&self, error: &anyhow::Error) {
        self.owner
            .failure
            .lock()
            .unwrap()
            .get_or_insert_with(|| format!("{error:#}"));
    }

    fn record_rejected_job(&self, job: ApplyJob) {
        let ApplyJob::Capture { ctx, event, .. } = job else {
            return;
        };
        self.owner.failure.lock().unwrap().get_or_insert_with(|| {
            "capture admission rejected job (queue, global budget or shutdown)".to_string()
        });
        self.owner.rejected.try_record(Arc::clone(&ctx), event);
        tracing::warn!(
            target: "pvisor_gateway",
            story_id = %ctx.story_id().as_str(),
            "capture admission rejected job (queue, global budget or shutdown)"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::Call;
    use crate::config::CaptureLevel;
    use crate::engine::CaptureEngine;
    use crate::engine::{CompleteEvent, Event, RequestEvent};
    use crate::protocol::ProtocolKind;
    use crate::provider::ProviderKind;
    use crate::record::CaptureRecord;
    use crate::session::index::SessionIndexStore;
    use crate::session::storage::CaptureRoute;
    use crate::sink::CaptureEventObserver;

    struct OrderRecordingSink {
        order: Mutex<Vec<String>>,
    }

    struct SlowSink;

    impl CaptureEventObserver for SlowSink {
        fn observe(&self, _event: &pvisor_core::event::Event) -> anyhow::Result<()> {
            std::thread::sleep(std::time::Duration::from_millis(400));
            Ok(())
        }
    }

    impl OrderRecordingSink {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                order: Mutex::new(Vec::new()),
            })
        }

        fn drain_order(&self) -> Vec<String> {
            self.order.lock().unwrap().clone()
        }
    }

    impl CaptureEventObserver for OrderRecordingSink {
        fn observe(&self, event: &pvisor_core::event::Event) -> anyhow::Result<()> {
            let record = CaptureRecord::from_event(event, 0)?;
            self.order.lock().unwrap().push(format!(
                "{}:{}",
                record.kind,
                record.call_id.as_deref().unwrap_or("")
            ));
            Ok(())
        }
    }

    fn sample_ctx(call_id: &str) -> CallContext {
        CallContext::new(
            crate::engine::StoryContext::from_route(
                CaptureRoute {
                    root_session: Some("run-1".into()),
                    session_id: "sess".into(),
                    storage_session_id: "run-1".into(),
                    subagent_id: None,
                },
                "agent",
            ),
            Call {
                call_id: call_id.into(),
                trace_id: "t1".into(),
                started_at: "2026-01-01T00:00:00Z".into(),
            },
            Vec::new(),
            crate::engine::CallCaptureConfig {
                level: CaptureLevel::Dialogue,
                client_model: "m".into(),
                upstream_model: "m".into(),
                provider: ProviderKind::OpenAi,
                protocol: ProtocolKind::ChatCompletions,
                debug_on: false,
            },
        )
    }

    fn cancelled() -> Event {
        Event::Cancelled(crate::engine::CancelEvent {
            reason: None,
            status: 200,
            bytes_received: 0,
            streaming: false,
        })
    }

    #[test]
    fn global_budget_counts_active_jobs_and_releases_on_drop() {
        let budget = JobBudget::new(2, 100);
        let first = budget.admit(60).unwrap();
        assert!(budget.admit(41).is_err());
        let second = budget.admit(40).unwrap();
        assert!(budget.admit(0).is_err());
        drop(first);
        let replacement = budget.admit(60).unwrap();
        drop((second, replacement));
        let usage = budget.usage.lock().unwrap();
        assert_eq!((usage.jobs, usage.bytes), (0, 0));
        drop(usage);
        assert!(budget.admit(usize::MAX).is_err());
    }

    #[tokio::test]
    async fn saturated_external_budget_does_not_block_cross_story_backfill() {
        let dir = tempfile::tempdir().unwrap();
        let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
        let engine = CaptureEngine::new(
            OrderRecordingSink::new(),
            index,
            Arc::new(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
        let dispatcher = engine.dispatcher_for_test();
        // Model an active child retaining the entire external byte budget while
        // awaiting the main-story receipt. Internal work must still progress.
        let retained = engine
            .dispatcher_for_test()
            .owner
            .external
            .admit(MAX_EXTERNAL_BYTES)
            .unwrap();
        assert!(
            dispatcher
                .apply(Arc::new(sample_ctx("overload")), cancelled())
                .await
                .is_err()
        );
        let main = sample_ctx("main-call");
        let record = crate::subagent_link::spawn_link_backfill_record("parent", &[], &main.call);
        let command = LocalStoryCommand::persist_record(
            super::super::wire::StoryScope::from_context(&main),
            record,
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            dispatcher.command_internal(main.story_id().as_str(), command),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(dispatcher.owner.internal.usage.lock().unwrap().jobs, 0);
        let internal_retained = dispatcher.owner.internal.admit(MAX_INTERNAL_BYTES).unwrap();
        let exhausted = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            dispatcher.command_internal(main.story_id().as_str(), LocalStoryCommand::Flush),
        )
        .await
        .unwrap();
        assert!(
            exhausted
                .unwrap_err()
                .to_string()
                .contains("budget exhausted")
        );
        drop(internal_retained);
        drop(retained);
        engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn retained_story_limit_rejects_without_spawning_another_worker() {
        let dir = tempfile::tempdir().unwrap();
        let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
        let engine = CaptureEngine::new(
            OrderRecordingSink::new(),
            index,
            Arc::new(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
        let dispatcher = engine.dispatcher_for_test();
        {
            let mut state = dispatcher.owner.state.lock().unwrap();
            for i in 0..MAX_RETAINED_STORIES {
                dispatcher.queue(&mut state, &format!("story-{i}")).unwrap();
            }
            assert!(dispatcher.queue(&mut state, "overflow").is_err());
            assert_eq!(state.queues.len(), MAX_RETAINED_STORIES);
            assert_eq!(state.workers.len(), MAX_RETAINED_STORIES);
        }
        assert!(
            dispatcher
                .command_internal("overflow", LocalStoryCommand::Flush)
                .await
                .is_err()
        );
        assert_eq!(dispatcher.owner.internal.usage.lock().unwrap().jobs, 0);
        engine.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn nonblocking_overload_is_reported_by_flush_and_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
        let engine = CaptureEngine::new(
            OrderRecordingSink::new(),
            index,
            Arc::new(dir.path().to_path_buf()),
        )
        .await
        .unwrap();
        let retained = engine
            .dispatcher_for_test()
            .owner
            .external
            .admit(MAX_EXTERNAL_BYTES)
            .unwrap();
        engine.spawn_apply(sample_ctx("rejected-budget"), cancelled());
        assert!(
            engine
                .flush()
                .await
                .unwrap_err()
                .to_string()
                .contains("uncommitted")
        );
        drop(retained);
        assert!(engine.shutdown().await.is_err());
        assert_eq!(
            dead_letter::read_dead_letter_entries(dir.path())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn diagnostic_byte_budget_does_not_retain_oversized_events() {
        let dir = tempfile::tempdir().unwrap();
        let writer = RejectedEventWriter::new(Arc::new(dir.path().to_path_buf()));
        writer.try_record(
            Arc::new(sample_ctx("oversized")),
            Event::ResponseDraft(crate::engine::DraftEvent {
                status: 200,
                assistant_content: "x".repeat(MAX_DIAGNOSTIC_BYTES + 1),
            }),
        );
        assert_eq!(writer.budget.usage.lock().unwrap().jobs, 0);
        writer.drain().unwrap();
        assert!(
            dead_letter::read_dead_letter_entries(dir.path())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rejected_writer_marker_drains_queued_appends_without_drop() {
        let dir = tempfile::tempdir().unwrap();
        let writer = RejectedEventWriter::new(Arc::new(dir.path().to_path_buf()));
        for i in 0..8 {
            writer.try_record(
                Arc::new(sample_ctx(&format!("rejected-{i}"))),
                Event::Cancelled(crate::engine::CancelEvent {
                    reason: Some("overloaded".into()),
                    status: 200,
                    bytes_received: 0,
                    streaming: true,
                }),
            );
        }
        writer.drain().unwrap();
        assert!(
            writer.worker.lock().unwrap().is_some(),
            "drain must not rely on joining Drop"
        );
        let entries = dead_letter::read_dead_letter_entries(dir.path()).unwrap();
        assert_eq!(entries.len(), 8);
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.context.call.call_id, format!("rejected-{i}"));
        }
        writer.drain().unwrap();
    }

    #[test]
    fn rejected_writer_marker_reports_append_failure() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".capture"), b"not a directory").unwrap();
        let writer = RejectedEventWriter::new(Arc::new(dir.path().to_path_buf()));
        writer.try_record(
            Arc::new(sample_ctx("rejected")),
            Event::Cancelled(crate::engine::CancelEvent {
                reason: None,
                status: 200,
                bytes_received: 0,
                streaming: true,
            }),
        );
        assert!(
            writer
                .drain()
                .unwrap_err()
                .to_string()
                .contains("dead letter write failed")
        );
        assert!(writer.worker.lock().unwrap().is_some());
    }

    #[tokio::test]
    async fn dispatcher_preserves_request_before_response_order() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(dir.path().to_path_buf());
        let sink = OrderRecordingSink::new();
        let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
        let engine = CaptureEngine::new(sink.clone(), index, storage.clone())
            .await
            .unwrap();

        let ctx_req = sample_ctx("call-a");
        let ctx_resp = sample_ctx("call-a");
        engine.spawn_apply(
            ctx_req,
            Event::Request(RequestEvent {
                path: "/v1/chat/completions".into(),
                method: "POST".into(),
                url: None,
                body_bytes: 10,
                user_content: Some("hi".into()),
                body_json: None,
                semantic: None,
                model_rewritten: false,
                headers: vec![],
            }),
        );
        engine.spawn_apply(
            ctx_resp,
            Event::ResponseComplete(CompleteEvent {
                status: 200,
                resp_bytes: bytes::Bytes::from_static(
                    br#"{"choices":[{"message":{"content":"ok"}}]}"#,
                ),
                streaming: false,
                stream_metrics: None,
                assistant_content: Some("ok".into()),
                semantic: None,
                headers: vec![],
            }),
        );

        engine.flush().await.unwrap();

        let order = sink.drain_order();
        assert!(
            order.len() >= 2
                && order[0].starts_with("llm.request:")
                && order[1].starts_with("llm.response"),
            "expected request before response, got {order:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn durable_sink_wait_does_not_block_tokio_worker() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(dir.path().to_path_buf());
        let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
        let engine = CaptureEngine::new(Arc::new(SlowSink), index, storage)
            .await
            .unwrap();

        let started = std::time::Instant::now();
        engine.spawn_apply(
            sample_ctx("slow-call"),
            Event::Request(RequestEvent {
                path: "/v1/chat/completions".into(),
                method: "POST".into(),
                url: None,
                body_bytes: 10,
                user_content: Some("hi".into()),
                body_json: None,
                semantic: None,
                model_rewritten: false,
                headers: vec![],
            }),
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "synchronous sink wait starved the only Tokio worker"
        );
        engine.flush().await.unwrap();
    }
}
