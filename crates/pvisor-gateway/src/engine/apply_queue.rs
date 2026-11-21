//! Per-`story_id` ordered capture apply queue — preserves event order within a story.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc as std_mpsc};

use std::collections::HashMap;

use anyhow::Context;

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
const MAX_EXTERNAL_TASKS: usize = 1024;
const MAX_EXTERNAL_BYTES: usize = 64 * 1024 * 1024;
const MAX_INTERNAL_TASKS: usize = 256;
const MAX_INTERNAL_BYTES: usize = 32 * 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 8 * 1024 * 1024;

#[derive(Default)]
struct BudgetUsage {
    tasks: usize,
    bytes: usize,
}

struct TaskBudget {
    usage: Mutex<BudgetUsage>,
    max_tasks: usize,
    max_bytes: usize,
}

impl TaskBudget {
    fn new(max_tasks: usize, max_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            usage: Mutex::new(BudgetUsage::default()),
            max_tasks,
            max_bytes,
        })
    }

    // Never wait for global capacity: a worker may retain its permit while
    // awaiting a backfill. Waiting here could make admission a dependency cycle.
    fn admit(self: &Arc<Self>, bytes: usize) -> anyhow::Result<TaskPermit> {
        let mut usage = self
            .usage
            .lock()
            .map_err(|_| anyhow::anyhow!("capture task budget mutex poisoned"))?;
        if usage.tasks >= self.max_tasks || bytes > self.max_bytes.saturating_sub(usage.bytes) {
            anyhow::bail!("capture global task/byte budget exhausted");
        }
        usage.tasks += 1;
        usage.bytes += bytes;
        Ok(TaskPermit {
            budget: Arc::clone(self),
            bytes,
        })
    }
}

struct TaskPermit {
    budget: Arc<TaskBudget>,
    bytes: usize,
}

impl Drop for TaskPermit {
    fn drop(&mut self) {
        let Ok(mut usage) = self.budget.usage.lock() else {
            tracing::error!("capture task budget mutex poisoned during permit release");
            return;
        };
        usage.tasks -= 1;
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

struct AdmittedTask {
    task: ApplyTask,
    // Held through preparation, persistence and dependent backfill receipts.
    _permit: Option<TaskPermit>,
}

impl From<ApplyTask> for AdmittedTask {
    fn from(task: ApplyTask) -> Self {
        Self {
            task,
            _permit: None,
        }
    }
}

enum RejectedEventMessage {
    Event {
        ctx: Arc<CallContext>,
        event: Event,
        _permit: TaskPermit,
    },
    Barrier {
        ack: std_mpsc::SyncSender<anyhow::Result<()>>,
    },
    Finish,
}

struct RejectedEventWriter {
    budget: Arc<TaskBudget>,
    tx: std_mpsc::SyncSender<RejectedEventMessage>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl RejectedEventWriter {
    fn new(storage: Arc<PathBuf>) -> anyhow::Result<Self> {
        Self::start(move |rx| {
            std::thread::Builder::new()
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
        })
    }

    fn start(
        spawn: impl FnOnce(
            std_mpsc::Receiver<RejectedEventMessage>,
        ) -> std::io::Result<std::thread::JoinHandle<()>>,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = std_mpsc::sync_channel(REJECTED_EVENT_QUEUE_CAPACITY);
        let worker = spawn(rx).context("start dead-letter writer thread")?;
        Ok(Self {
            budget: TaskBudget::new(REJECTED_EVENT_QUEUE_CAPACITY, MAX_DIAGNOSTIC_BYTES),
            tx,
            worker: Some(worker),
        })
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
        let permit = match self.budget.admit(capture_bytes(&ctx, &event)) {
            Ok(permit) => permit,
            Err(error) => {
                tracing::error!(
                    "dead-letter admission failed: {error:#}; rejected capture gap remains reported by flush/shutdown"
                );
                return;
            }
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
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// Accepted tasks retain the scheduler through backfill; idle workers must not form an ownership cycle.
enum ApplyTask {
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

impl ApplyTask {
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
    queues: HashMap<String, mpsc::Sender<AdmittedTask>>,
    workers: Vec<tokio::task::JoinHandle<()>>,
}

struct SchedulingOwner {
    external: Arc<TaskBudget>,
    internal: Arc<TaskBudget>,
    state: Mutex<SchedulingState>,
    failure: Mutex<Option<String>>,
    rejected: RejectedEventWriter,
}

impl SchedulingOwner {
    fn record_failure(&self, error: &anyhow::Error) {
        match self.failure.lock() {
            Ok(mut failure) => {
                failure.get_or_insert_with(|| format!("{error:#}"));
            }
            Err(_) => tracing::error!("capture failure registry mutex poisoned: {error:#}"),
        }
    }
}

/// The only bounded FIFO and worker per story. The worker owns story state and
/// performs preparation before applying each task; no second actor mailbox.
#[derive(Clone)]
pub(crate) struct ApplyDispatcher {
    inner: Arc<CaptureRuntimeInner>,
    owner: Arc<SchedulingOwner>,
}

impl ApplyDispatcher {
    pub(crate) fn new(inner: Arc<CaptureRuntimeInner>) -> anyhow::Result<Self> {
        let rejected = RejectedEventWriter::new(Arc::clone(&inner.story_deps.storage))?;
        Ok(Self {
            inner,
            owner: Arc::new(SchedulingOwner {
                external: TaskBudget::new(MAX_EXTERNAL_TASKS, MAX_EXTERNAL_BYTES),
                internal: TaskBudget::new(MAX_INTERNAL_TASKS, MAX_INTERNAL_BYTES),
                state: Mutex::new(SchedulingState {
                    accepting: true,
                    stopped: false,
                    queues: HashMap::new(),
                    workers: Vec::new(),
                }),
                failure: Mutex::new(None),
                rejected,
            }),
        })
    }

    pub(crate) fn enqueue(&self, ctx: Arc<CallContext>, event: Event) {
        let bytes = capture_bytes(&ctx, &event);
        let task = ApplyTask::Capture {
            owner: Arc::clone(&self.owner),
            ctx: Arc::clone(&ctx),
            event,
            ack: None,
        };
        let mut state = match self.owner.state.lock() {
            Ok(state) => state,
            Err(poison) => {
                drop(poison);
                self.record_rejected_task(task);
                return;
            }
        };
        if !state.accepting {
            drop(state);
            self.record_rejected_task(task);
            return;
        }
        let admitted = self.owner.external.admit(bytes);
        let tx = admitted.as_ref().map_err(|_| ()).and_then(|_| {
            self.queue(&mut state, ctx.story_id().as_str())
                .map_err(|_| ())
        });
        let rejected = match (admitted, tx) {
            (Ok(permit), Ok(tx)) => tx
                .try_send(AdmittedTask {
                    task,
                    _permit: Some(permit),
                })
                .err()
                .map(|error| error.into_inner().task),
            _ => Some(task),
        };
        drop(state);
        if let Some(task) = rejected {
            self.record_rejected_task(task);
        }
    }

    pub(crate) async fn apply(&self, ctx: Arc<CallContext>, event: Event) -> anyhow::Result<()> {
        let story_id = ctx.story_id().as_str().to_string();
        let (ack, done) = oneshot::channel();
        self.send(
            &story_id,
            ApplyTask::Capture {
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
            ApplyTask::Command {
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

    async fn send(&self, story_id: &str, task: ApplyTask, internal: bool) -> anyhow::Result<()> {
        let budget = if internal {
            &self.owner.internal
        } else {
            &self.owner.external
        };
        let task_permit = budget.admit(
            task.accounted_bytes()
                .saturating_add(story_id.len().saturating_mul(16)),
        )?;
        let tx = {
            let mut state = self
                .owner
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("capture scheduler mutex poisoned"))?;
            if state.stopped || (!internal && !state.accepting) {
                anyhow::bail!("capture scheduling owner is shutting down");
            }
            self.queue(&mut state, story_id)?
        };
        // Reserve outside the lock. Admission is linearized with shutdown only
        // when the reserved task is sent, not when a producer starts waiting.
        let permit = tx
            .reserve_owned()
            .await
            .map_err(|_| anyhow::anyhow!("story queue closed"))?;
        let state = self
            .owner
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("capture scheduler mutex poisoned"))?;
        if state.stopped || (!internal && !state.accepting) {
            anyhow::bail!("capture scheduling owner is shutting down");
        }
        permit.send(AdmittedTask {
            task,
            _permit: Some(task_permit),
        });
        Ok(())
    }

    /// Barrier each owner present at entry. Captures await their cross-story
    /// backfill receipts before passing a barrier, even for a newly made owner.
    pub(crate) async fn flush(&self) -> anyhow::Result<()> {
        let mut error = None;
        let queues: Vec<_> = {
            let state = match self.owner.state.lock() {
                Ok(state) => state,
                Err(poison) => {
                    error = Some(anyhow::anyhow!("capture scheduler mutex poisoned"));
                    poison.into_inner()
                }
            };
            state.queues.values().cloned().collect()
        };
        for tx in queues {
            let (ack, done) = oneshot::channel();
            let result = async {
                tx.send(
                    ApplyTask::Command {
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
        match self.owner.failure.lock() {
            Ok(failure) => {
                if let Some(failure) = failure.as_ref() {
                    error.get_or_insert_with(|| {
                        anyhow::anyhow!("capture has uncommitted events: {failure}")
                    });
                }
            }
            Err(_) => {
                error.get_or_insert_with(|| {
                    anyhow::anyhow!("capture failure registry mutex poisoned")
                });
            }
        }
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Close external admission first, but leave internal backfill admission
    /// open until accepted captures finish. Stop replies contain final snapshots.
    pub(crate) async fn shutdown(&self) -> (anyhow::Result<()>, HashMap<String, Story>) {
        // Recover ownership only for draining; poisoning continues to reject admission.
        match self.owner.state.lock() {
            Ok(mut state) => state.accepting = false,
            Err(poison) => poison.into_inner().accepting = false,
        }
        let mut result = self.flush().await;
        let (queues, workers) = {
            let mut state = match self.owner.state.lock() {
                Ok(state) => state,
                Err(poison) => {
                    result = Err(anyhow::anyhow!("capture scheduler mutex poisoned"));
                    poison.into_inner()
                }
            };
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
                    ApplyTask::Stop {
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
    ) -> anyhow::Result<mpsc::Sender<AdmittedTask>> {
        if let Some(tx) = state.queues.get(story_id) {
            return Ok(tx.clone());
        }
        if state.queues.len() >= MAX_RETAINED_STORIES {
            anyhow::bail!("capture retained story budget exhausted");
        }
        let (tx, mut rx) = mpsc::channel::<AdmittedTask>(APPLY_QUEUE_CAPACITY);
        let inner = Arc::clone(&self.inner);
        let mut story = StoryActor::new(StoryId::new(story_id), inner.story_deps.clone());
        let worker = tokio::spawn(async move {
            while let Some(AdmittedTask { task, _permit }) = rx.recv().await {
                match task {
                    ApplyTask::Capture {
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
                    ApplyTask::Command {
                        owner,
                        command,
                        ack,
                    } => {
                        let result = story.handle(*command).await;
                        if let Err(error) = &result {
                            owner.record_failure(error);
                        }
                        let _ = ack.send(result);
                    }
                    ApplyTask::Stop { owner: _owner, ack } => {
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
        self.owner.record_failure(error);
    }

    fn record_rejected_task(&self, task: ApplyTask) {
        let ApplyTask::Capture { ctx, event, .. } = task else {
            return;
        };
        self.record_failure(&anyhow::anyhow!(
            "capture admission rejected task (queue, global budget or shutdown)"
        ));
        self.owner.rejected.try_record(Arc::clone(&ctx), event);
        tracing::warn!(
            target: "pvisor_gateway",
            story_id = %ctx.story_id().as_str(),
            "capture admission rejected task (queue, global budget or shutdown)"
        );
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
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

    fn poison_budget(budget: &TaskBudget, tasks: usize, bytes: usize) {
        let result = std::panic::catch_unwind(|| {
            let mut usage = budget.usage.lock().unwrap();
            usage.tasks = tasks;
            usage.bytes = bytes;
            panic!("injected budget failure");
        });
        assert!(result.is_err());
        assert!(budget.usage.is_poisoned());
    }

    #[rstest::rstest]
    #[case::empty(0, 0)]
    #[case::apparently_valid(1, 1)]
    #[case::invalid_tasks(usize::MAX, 1)]
    #[case::invalid_bytes(1, usize::MAX)]
    fn poisoned_budget_never_reopens(#[case] tasks: usize, #[case] bytes: usize) {
        let budget = TaskBudget::new(2, 100);
        poison_budget(&budget, tasks, bytes);
        for request in [0, 1, 100, usize::MAX] {
            let error = budget.admit(request).err().unwrap();
            assert!(error.to_string().contains("mutex poisoned"));
        }
        let retained = budget.usage.lock().err().unwrap().into_inner();
        assert_eq!((retained.tasks, retained.bytes), (tasks, bytes));
    }

    #[rstest::rstest]
    #[case::ordinary_drop(false)]
    #[case::during_unwind(true)]
    fn poisoned_permit_release_does_not_panic(#[case] unwind: bool) {
        let budget = TaskBudget::new(2, 100);
        let permit = budget.admit(60).unwrap();
        poison_budget(&budget, 0, 0);
        let result = std::panic::catch_unwind(|| {
            let _permit = permit;
            if unwind {
                panic!("injected outer failure");
            }
        });
        assert_eq!(result.is_err(), unwind);
        assert_eq!(Arc::strong_count(&budget), 1);
        assert!(budget.admit(0).is_err());
        let retained = budget.usage.lock().err().unwrap().into_inner();
        assert_eq!((retained.tasks, retained.bytes), (0, 0));
    }

    #[rstest::rstest]
    fn healthy_permit_release_during_unwind_preserves_other_reservations() {
        let budget = TaskBudget::new(2, 100);
        let first = budget.admit(60).unwrap();
        let result = std::panic::catch_unwind(|| {
            let _second = budget.admit(40).unwrap();
            panic!("injected task failure");
        });
        assert!(result.is_err());
        assert!(!budget.usage.is_poisoned());
        {
            let usage = budget.usage.lock().unwrap();
            assert_eq!((usage.tasks, usage.bytes), (1, 60));
        }
        let replacement = budget.admit(40).unwrap();
        drop((first, replacement));
        let usage = budget.usage.lock().unwrap();
        assert_eq!((usage.tasks, usage.bytes), (0, 0));
    }

    #[rstest::rstest]
    #[case::no_tasks(0, 100, 0, false)]
    #[case::no_bytes(1, 0, 1, false)]
    #[case::zero_byte_task(1, 0, 0, true)]
    #[case::exact_limit(1, 100, 100, true)]
    #[case::over_limit(1, 100, 101, false)]
    #[case::maximum_bytes(1, usize::MAX, usize::MAX, true)]
    #[case::maximum_tasks(usize::MAX, 0, 0, true)]
    fn task_budget_boundaries(
        #[case] max_tasks: usize,
        #[case] max_bytes: usize,
        #[case] bytes: usize,
        #[case] accepted: bool,
    ) {
        let budget = TaskBudget::new(max_tasks, max_bytes);
        let result = budget.admit(bytes);
        assert_eq!(result.is_ok(), accepted);
        {
            let usage = budget.usage.lock().unwrap();
            let expected = if accepted { (1, bytes) } else { (0, 0) };
            assert_eq!((usage.tasks, usage.bytes), expected);
        }
        drop(result);
        let usage = budget.usage.lock().unwrap();
        assert_eq!((usage.tasks, usage.bytes), (0, 0));
    }

    proptest! {
        #[test]
        fn generated_task_budget_sequences_match_live_permits(
            max_tasks in 0..8usize,
            max_bytes in prop_oneof![0..1024usize, Just(usize::MAX)],
            operations in prop::collection::vec((any::<bool>(), any::<usize>()), 0..64),
        ) {
            let budget = TaskBudget::new(max_tasks, max_bytes);
            let mut active = Vec::new();
            for (admit, bytes) in operations {
                let total: u128 = active.iter().map(|(_, bytes)| *bytes as u128).sum();
                if admit {
                    let expected = active.len() < max_tasks
                        && total + bytes as u128 <= max_bytes as u128;
                    let result = budget.admit(bytes);
                    prop_assert_eq!(result.is_ok(), expected);
                    if let Ok(permit) = result {
                        active.push((permit, bytes));
                    }
                } else if !active.is_empty() {
                    drop(active.swap_remove(bytes % active.len()));
                }
                let total: u128 = active.iter().map(|(_, bytes)| *bytes as u128).sum();
                let usage = budget.usage.lock().unwrap();
                prop_assert_eq!(usage.tasks, active.len());
                prop_assert_eq!(usage.bytes as u128, total);
            }
            drop(active);
            let usage = budget.usage.lock().unwrap();
            prop_assert_eq!((usage.tasks, usage.bytes), (0, 0));
        }
    }

    #[test]
    fn global_budget_counts_active_tasks_and_releases_on_drop() {
        let budget = TaskBudget::new(2, 100);
        let first = budget.admit(60).unwrap();
        assert!(budget.admit(41).is_err());
        let second = budget.admit(40).unwrap();
        assert!(budget.admit(0).is_err());
        drop(first);
        let replacement = budget.admit(60).unwrap();
        drop((second, replacement));
        let usage = budget.usage.lock().unwrap();
        assert_eq!((usage.tasks, usage.bytes), (0, 0));
        drop(usage);
        assert!(budget.admit(usize::MAX).is_err());
    }

    #[rstest::rstest]
    #[case::scheduler(true, false)]
    #[case::failure_registry(false, true)]
    #[case::both(true, true)]
    #[tokio::test]
    async fn poisoned_scheduler_drains_accepted_work_before_reporting_failure(
        #[case] scheduler: bool,
        #[case] failures: bool,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
        let sink = OrderRecordingSink::new();
        let engine = CaptureEngine::new(sink.clone(), index, Arc::new(dir.path().to_path_buf()))
            .await
            .unwrap();
        let dispatcher = engine.dispatcher_for_test();
        let owner = Arc::clone(&dispatcher.owner);
        dispatcher.enqueue(Arc::new(sample_ctx("accepted")), cancelled());
        if scheduler {
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _guard = owner.state.lock().unwrap();
                    panic!("injected scheduler failure");
                }))
                .is_err()
            );
        }
        if failures {
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _guard = owner.failure.lock().unwrap();
                    panic!("injected failure registry fault");
                }))
                .is_err()
            );
            dispatcher.record_failure(&anyhow::anyhow!("additional failure"));
        }
        if scheduler {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                dispatcher.apply(Arc::new(sample_ctx("direct-rejected")), cancelled()),
            )
            .await
            .unwrap();
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("scheduler mutex poisoned")
            );
            dispatcher.enqueue(Arc::new(sample_ctx("rejected")), cancelled());
        }
        let flushed = tokio::time::timeout(std::time::Duration::from_secs(2), engine.flush())
            .await
            .unwrap();
        assert!(flushed.unwrap_err().to_string().contains("poisoned"));
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), engine.shutdown())
            .await
            .unwrap();
        assert!(stopped.unwrap_err().to_string().contains("poisoned"));
        assert_eq!(sink.drain_order(), ["llm.call.cancelled:accepted"]);
        let state = owner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(state.stopped);
        assert!(state.queues.is_empty());
        assert!(state.workers.is_empty());
        drop(state);
        assert_eq!(owner.external.usage.lock().unwrap().tasks, 0);
        assert_eq!(
            dead_letter::read_dead_letter_entries(dir.path())
                .unwrap()
                .len(),
            usize::from(scheduler)
        );
    }

    #[rstest::rstest]
    #[case::external(false)]
    #[case::internal(true)]
    #[tokio::test]
    async fn poisoned_task_budget_does_not_block_the_other_budget(#[case] internal: bool) {
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
        let budget = if internal {
            &dispatcher.owner.internal
        } else {
            &dispatcher.owner.external
        };
        poison_budget(budget, 0, 0);
        let rejected = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            dispatcher.command_with_admission("story", LocalStoryCommand::Flush, internal),
        )
        .await
        .unwrap();
        assert!(rejected.unwrap_err().to_string().contains("mutex poisoned"));
        let reply = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            dispatcher.command_with_admission("story", LocalStoryCommand::Flush, !internal),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(reply, StoryReply::Ack));
        assert!(!dispatcher.owner.state.is_poisoned());
        tokio::time::timeout(std::time::Duration::from_secs(2), engine.shutdown())
            .await
            .unwrap()
            .unwrap();
    }

    #[rstest::rstest]
    #[case::healthy_diagnostics(false)]
    #[case::poisoned_diagnostics(true)]
    #[tokio::test]
    async fn poisoned_enqueue_budget_reports_gap_through_shutdown(
        #[case] diagnostics_poisoned: bool,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
        let sink = OrderRecordingSink::new();
        let engine = CaptureEngine::new(sink.clone(), index, Arc::new(dir.path().to_path_buf()))
            .await
            .unwrap();
        let dispatcher = engine.dispatcher_for_test();
        poison_budget(&dispatcher.owner.external, 0, 0);
        if diagnostics_poisoned {
            poison_budget(&dispatcher.owner.rejected.budget, 0, 0);
        }
        dispatcher.enqueue(Arc::new(sample_ctx("poison-rejected")), cancelled());
        assert!(dispatcher.owner.state.lock().unwrap().queues.is_empty());
        let flushed = tokio::time::timeout(std::time::Duration::from_secs(2), engine.flush())
            .await
            .unwrap();
        assert!(flushed.unwrap_err().to_string().contains("uncommitted"));
        let stopped = tokio::time::timeout(std::time::Duration::from_secs(2), engine.shutdown())
            .await
            .unwrap();
        assert!(stopped.unwrap_err().to_string().contains("uncommitted"));
        assert!(sink.drain_order().is_empty());
        let entries = dead_letter::read_dead_letter_entries(dir.path()).unwrap();
        assert_eq!(entries.len(), usize::from(!diagnostics_poisoned));
        if let Some(entry) = entries.first() {
            assert_eq!(entry.context.call.call_id, "poison-rejected");
        }
    }

    #[tokio::test]
    async fn accepted_capture_finishes_after_budget_poison() {
        let dir = tempfile::tempdir().unwrap();
        let index = SessionIndexStore::open(dir.path()).unwrap().clone_handle();
        let sink = OrderRecordingSink::new();
        let engine = CaptureEngine::new(sink.clone(), index, Arc::new(dir.path().to_path_buf()))
            .await
            .unwrap();
        let dispatcher = engine.dispatcher_for_test();
        dispatcher.enqueue(Arc::new(sample_ctx("accepted")), cancelled());
        let budget = Arc::clone(&dispatcher.owner.external);
        assert_eq!(budget.usage.lock().unwrap().tasks, 1);
        poison_budget(&budget, 0, 0);
        tokio::time::timeout(std::time::Duration::from_secs(2), engine.shutdown())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sink.drain_order(), ["llm.call.cancelled:accepted"]);
        assert!(budget.admit(0).is_err());
        assert!(
            dead_letter::read_dead_letter_entries(dir.path())
                .unwrap()
                .is_empty()
        );
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
        assert_eq!(dispatcher.owner.internal.usage.lock().unwrap().tasks, 0);
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
        assert_eq!(dispatcher.owner.internal.usage.lock().unwrap().tasks, 0);
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
        let writer = RejectedEventWriter::new(Arc::new(dir.path().to_path_buf())).unwrap();
        writer.try_record(
            Arc::new(sample_ctx("oversized")),
            Event::ResponseDraft(crate::engine::DraftEvent {
                status: 200,
                assistant_content: "x".repeat(MAX_DIAGNOSTIC_BYTES + 1),
            }),
        );
        assert_eq!(writer.budget.usage.lock().unwrap().tasks, 0);
        writer.drain().unwrap();
        assert!(
            dead_letter::read_dead_letter_entries(dir.path())
                .unwrap()
                .is_empty()
        );
    }

    #[rstest::rstest]
    #[case::resource_limit(std::io::ErrorKind::WouldBlock)]
    #[case::permission(std::io::ErrorKind::PermissionDenied)]
    #[case::other(std::io::ErrorKind::Other)]
    fn rejected_writer_startup_preserves_io_error(#[case] kind: std::io::ErrorKind) {
        let resource = Arc::new(());
        let weak = Arc::downgrade(&resource);
        let mut called = false;
        let result = RejectedEventWriter::start(|rx| {
            called = true;
            let _resource = resource;
            assert!(matches!(rx.try_recv(), Err(std_mpsc::TryRecvError::Empty)));
            Err(std::io::Error::new(kind, "injected spawn failure"))
        });
        let error = result.err().unwrap();
        assert!(called);
        assert!(format!("{error:#}").contains("start dead-letter writer thread"));
        assert!(format!("{error:#}").contains("injected spawn failure"));
        assert_eq!(error.downcast_ref::<std::io::Error>().unwrap().kind(), kind);
        assert!(weak.upgrade().is_none());
    }

    #[rstest::rstest]
    #[case::empty(0)]
    #[case::single(1)]
    #[case::batch(8)]
    #[case::queue_capacity(REJECTED_EVENT_QUEUE_CAPACITY)]
    fn rejected_writer_drop_drains_and_releases_permits(#[case] count: usize) {
        let dir = tempfile::tempdir().unwrap();
        let writer = RejectedEventWriter::new(Arc::new(dir.path().to_path_buf())).unwrap();
        assert_eq!(
            writer.worker.as_ref().unwrap().thread().name(),
            Some("pvisor-dead-letter")
        );
        let budget = Arc::clone(&writer.budget);
        for index in 0..count {
            writer.try_record(Arc::new(sample_ctx(&format!("drop-{index}"))), cancelled());
        }
        drop(writer);
        let entries = dead_letter::read_dead_letter_entries(dir.path()).unwrap();
        assert_eq!(entries.len(), count);
        for (index, entry) in entries.iter().enumerate() {
            assert_eq!(entry.context.call.call_id, format!("drop-{index}"));
        }
        let usage = budget.usage.lock().unwrap();
        assert_eq!((usage.tasks, usage.bytes), (0, 0));
        assert_eq!(Arc::strong_count(&budget), 1);
    }

    #[rstest::rstest]
    #[case::closed_before_barrier(false, "closed before drain")]
    #[case::dropped_barrier_reply(true, "reply dropped")]
    fn rejected_writer_panic_reports_drain_failure_without_drop_panic(
        #[case] drop_reply: bool,
        #[case] message: &str,
    ) {
        let (closed, ready) = std_mpsc::sync_channel(1);
        let writer = RejectedEventWriter::start(move |rx| {
            std::thread::Builder::new().spawn(move || {
                if drop_reply {
                    let message = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
                    assert!(matches!(message, RejectedEventMessage::Barrier { .. }));
                    drop(message);
                }
                drop(rx);
                closed.send(()).unwrap();
                panic!("injected writer failure");
            })
        })
        .unwrap();
        if !drop_reply {
            ready
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
        }
        let drained = writer.drain();
        let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(writer)));
        assert!(drained.unwrap_err().to_string().contains(message));
        assert!(dropped.is_ok());
    }

    #[rstest::rstest]
    #[case::ordinary_drop(false)]
    #[case::during_unwind(true)]
    fn rejected_writer_drop_waits_for_worker_exit(#[case] unwind: bool) {
        let (finish, received) = std_mpsc::sync_channel(1);
        let (release, exit) = std_mpsc::sync_channel(1);
        let writer = RejectedEventWriter::start(move |rx| {
            std::thread::Builder::new().spawn(move || {
                let message = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
                assert!(matches!(message, RejectedEventMessage::Finish));
                finish.send(()).unwrap();
                let _ = exit.recv_timeout(std::time::Duration::from_secs(2));
            })
        })
        .unwrap();
        let (done, completed) = std_mpsc::sync_channel(1);
        let dropper = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _writer = writer;
                if unwind {
                    panic!("injected owner failure");
                }
            }));
            done.send(result.is_err()).unwrap();
        });
        let saw_finish = received.recv_timeout(std::time::Duration::from_secs(2));
        let premature = completed.recv_timeout(std::time::Duration::from_millis(20));
        let returned_early = premature.is_ok();
        let _ = release.send(());
        let completed = match premature {
            Ok(result) => Ok(result),
            Err(_) => completed.recv_timeout(std::time::Duration::from_secs(2)),
        };
        dropper.join().unwrap();
        assert!(saw_finish.is_ok());
        assert!(!returned_early);
        assert_eq!(completed.unwrap(), unwind);
    }

    proptest! {
        #[test]
        fn generated_rejected_writer_markers_preserve_fifo(
            operations in prop::collection::vec(any::<bool>(), 0..16),
        ) {
            let dir = tempfile::tempdir().unwrap();
            let writer = RejectedEventWriter::new(Arc::new(dir.path().to_path_buf())).unwrap();
            let budget = Arc::clone(&writer.budget);
            let mut expected = Vec::new();
            for record in operations {
                if record {
                    let id = format!("generated-{}", expected.len());
                    writer.try_record(Arc::new(sample_ctx(&id)), cancelled());
                    expected.push(id);
                } else {
                    writer.drain().unwrap();
                    let entries = dead_letter::read_dead_letter_entries(dir.path()).unwrap();
                    let actual: Vec<_> = entries.iter().map(|entry| entry.context.call.call_id.clone()).collect();
                    prop_assert_eq!(&actual, &expected);
                    prop_assert!(writer.worker.is_some());
                }
            }
            drop(writer);
            let entries = dead_letter::read_dead_letter_entries(dir.path()).unwrap();
            let actual: Vec<_> = entries.iter().map(|entry| entry.context.call.call_id.clone()).collect();
            prop_assert_eq!(actual, expected);
            let usage = budget.usage.lock().unwrap();
            prop_assert_eq!((usage.tasks, usage.bytes), (0, 0));
        }
    }

    #[test]
    fn rejected_writer_marker_drains_queued_appends_without_drop() {
        let dir = tempfile::tempdir().unwrap();
        let writer = RejectedEventWriter::new(Arc::new(dir.path().to_path_buf())).unwrap();
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
            writer.worker.is_some(),
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
        let writer = RejectedEventWriter::new(Arc::new(dir.path().to_path_buf())).unwrap();
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
        assert!(writer.worker.is_some());
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
