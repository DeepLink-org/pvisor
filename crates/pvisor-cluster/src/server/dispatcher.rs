//! Bounded single-writer HTTP dispatch. Each request keeps its own WAL frames;
//! only the durability barrier is shared. No result escapes before that barrier.
use crate::{journal::JournalFailure, scheduler::Scheduler};
use std::{
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

const QUEUE_CAPACITY: usize = 256;
const GROUP_COMMANDS: usize = 64;
const GROUP_BYTES: usize = 4 * 1024 * 1024;
const GROUP_WORK: Duration = Duration::from_millis(2);

type Reply = Box<dyn FnOnce(bool) + Send>;
type Command = Box<dyn FnOnce(&mut Scheduler) -> Reply + Send>;
pub(super) type SharedScheduler = Arc<Mutex<Scheduler>>;

#[derive(Debug)]
pub(super) struct Overloaded;
impl std::fmt::Display for Overloaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("controller request queue is full; retry with the same request identity")
    }
}
impl std::error::Error for Overloaded {}

pub(super) struct Dispatcher {
    sender: mpsc::Sender<Command>,
}

pub(super) fn lock(scheduler: &SharedScheduler) -> anyhow::Result<MutexGuard<'_, Scheduler>> {
    let guard = scheduler.lock().map_err(|_| JournalFailure)?;
    guard.ensure_available()?;
    Ok(guard)
}

fn command<T: Send + 'static>(
    f: impl FnOnce(&mut Scheduler) -> anyhow::Result<T> + Send + 'static,
) -> (Command, oneshot::Receiver<anyhow::Result<T>>) {
    let (sender, receiver) = oneshot::channel();
    let command: Command = Box::new(move |scheduler| {
        let result = f(scheduler);
        Box::new(move |durable| {
            let result = if durable {
                result
            } else {
                Err(JournalFailure.into())
            };
            // Once queued, an operation remains accepted even if its HTTP client
            // disconnects. Retrying uses the scheduler's existing idempotency.
            let _ = sender.send(result);
        })
    });
    (command, receiver)
}

impl Dispatcher {
    pub(super) fn ensure_available(&self) -> anyhow::Result<()> {
        if self.sender.is_closed() {
            return Err(JournalFailure.into());
        }
        Ok(())
    }

    pub(super) fn new(scheduler: SharedScheduler) -> anyhow::Result<Self> {
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        std::thread::Builder::new()
            .name("pvisor-scheduler".into())
            .spawn(move || serve(scheduler, receiver))?;
        Ok(Self { sender })
    }

    pub(super) async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Scheduler) -> anyhow::Result<T> + Send + 'static,
    ) -> anyhow::Result<T> {
        let (command, response) = command(f);
        self.sender.try_send(command).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => anyhow::Error::new(Overloaded),
            mpsc::error::TrySendError::Closed(_) => JournalFailure.into(),
        })?;
        response.await.map_err(|_| JournalFailure)?
    }

    // There is only one periodic reaper. Reserve a place rather than rejecting
    // it under overload, so a continuous stream of try_send clients cannot
    // starve expiry maintenance. This adds at most one waiting command.
    pub(super) async fn reap(&self) -> anyhow::Result<usize> {
        let (command, response) = command(|s| s.reap(pvisor_core::unix_now_ms()));
        self.sender
            .send(command)
            .await
            .map_err(|_| JournalFailure)?;
        response.await.map_err(|_| JournalFailure)?
    }
}

fn serve(scheduler: SharedScheduler, mut receiver: mpsc::Receiver<Command>) {
    while let Some(first) = receiver.blocking_recv() {
        if !group(
            &scheduler,
            first,
            &mut receiver,
            GROUP_COMMANDS,
            GROUP_BYTES,
            GROUP_WORK,
        ) {
            // Closing also drops replies for queued commands which never ran.
            // They must report unavailable, even if they were read-only.
            receiver.close();
            break;
        }
    }
}

fn group(
    scheduler: &SharedScheduler,
    first: Command,
    receiver: &mut mpsc::Receiver<Command>,
    max_commands: usize,
    max_bytes: usize,
    max_work: Duration,
) -> bool {
    let Ok(mut guard) = lock(scheduler) else {
        return false;
    };
    if guard.begin_group().is_err() {
        return false;
    }
    let started = Instant::now();
    let mut replies = Vec::with_capacity(max_commands);
    replies.push(first(&mut guard));
    while replies.len() < max_commands
        && guard.pending_journal_bytes() < max_bytes
        && started.elapsed() < max_work
        && guard.ensure_available().is_ok()
    {
        let Ok(next) = receiver.try_recv() else {
            break;
        };
        replies.push(next(&mut guard));
    }
    let result = guard.end_group();
    let durable = result.is_ok();
    // Any external scheduler accessor must wait for the same durability barrier,
    // and lock() refuses all access after an uncertain write or sync failure.
    drop(guard);
    if let Err(error) = result {
        eprintln!("controller journal unavailable; restart required: {error}");
    }
    for reply in replies {
        reply(durable);
    }
    durable
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CLUSTER_VERSION, ExecutionClass, Resources, TaskPhase, TaskSpec, scheduler::SchedulerConfig,
    };
    use axum::response::IntoResponse;
    use pvisor_core::{ExecutorKind, IsolationKind, RunInvocation, RunSpec};

    fn task(id: &str) -> TaskSpec {
        let mut run = RunSpec::process(id, "dispatch-test", "/bin/true");
        let RunInvocation::Process(process) = &mut run.invocation;
        process.inherit_env = false;
        TaskSpec {
            version: CLUSTER_VERSION,
            id: id.into(),
            tenant: "tenant".into(),
            run,
            execution: ExecutionClass {
                executor: ExecutorKind::Process,
                isolation: IsolationKind::HostProcess,
            },
            resources: Resources {
                slots: 1,
                memory_bytes: 64 * 1024 * 1024,
                cpu_millis: 250,
            },
            labels: Default::default(),
            cache_keys: vec![],
            retain_bundle: false,
            retain_artifacts: None,
            gateway: None,
            cpu_qos: None,
            restore: None,
            environment: None,
        }
    }

    fn scheduler(path: &std::path::Path) -> SharedScheduler {
        Arc::new(Mutex::new(
            Scheduler::open(path, SchedulerConfig::default()).unwrap(),
        ))
    }

    #[test]
    fn burst_shares_sync_but_keeps_order_conflicts_and_read_barriers() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("wal");
        let scheduler = scheduler(&path);
        let initial_syncs = lock(&scheduler).unwrap().journal_syncs();
        let (sender, mut receiver) = mpsc::channel(16);
        let (first, first_reply) = command(|s| s.submit(task("first"), 10));
        sender.try_send(first).unwrap();
        let first_reply = Arc::new(Mutex::new(first_reply));
        let mut replies = Vec::new();
        for id in 0..4 {
            let (cmd, reply) = command(move |s| s.submit(task(&format!("other-{id}")), 11));
            sender.try_send(cmd).unwrap();
            replies.push(reply);
        }
        // A logical conflict must not suppress commits already accepted in this
        // group, nor expose their responses before the group's fsync.
        let mut conflicting = task("first");
        conflicting.tenant = "other-tenant".into();
        let (cmd, mut conflict) = command(move |s| s.submit(conflicting, 12));
        sender.try_send(cmd).unwrap();
        let (cmd, mut cancellation) = command(|s| s.cancel("other-0", 13));
        sender.try_send(cmd).unwrap();
        let earlier = first_reply.clone();
        let (cmd, mut read) = command(move |s| {
            assert!(matches!(
                earlier.lock().unwrap().try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
            assert!(s.pending_journal_bytes() > 0);
            assert_eq!(s.task("other-0")?.phase, TaskPhase::Cancelled);
            s.task("first")
        });
        sender.try_send(cmd).unwrap();
        assert!(group(
            &scheduler,
            receiver.try_recv().unwrap(),
            &mut receiver,
            64,
            usize::MAX,
            Duration::from_secs(60)
        ));
        assert_eq!(lock(&scheduler).unwrap().journal_syncs(), initial_syncs + 1);
        first_reply.lock().unwrap().try_recv().unwrap().unwrap();
        for mut reply in replies {
            reply.try_recv().unwrap().unwrap();
        }
        assert!(conflict.try_recv().unwrap().is_err());
        assert_eq!(
            cancellation.try_recv().unwrap().unwrap().phase,
            TaskPhase::Cancelled
        );
        assert_eq!(read.try_recv().unwrap().unwrap().spec.id, "first");
        drop(scheduler);
        let reopened = Scheduler::open(&path, SchedulerConfig::default()).unwrap();
        assert_eq!(
            reopened.task("other-0").unwrap().phase,
            TaskPhase::Cancelled
        );
        assert_eq!(reopened.counts().get("queued"), Some(&4));
    }

    #[test]
    fn command_byte_and_work_limits_bound_each_group() {
        for (commands, bytes, work, expected_syncs) in [
            (2, usize::MAX, Duration::from_secs(60), 3),
            (64, 1, Duration::from_secs(60), 5),
            (64, usize::MAX, Duration::ZERO, 5),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let scheduler = scheduler(&temp.path().join("wal"));
            let initial_syncs = lock(&scheduler).unwrap().journal_syncs();
            let (sender, mut receiver) = mpsc::channel(8);
            let mut replies = Vec::new();
            for id in 0..5 {
                let (cmd, reply) = command(move |s| s.submit(task(&format!("task-{id}")), 10));
                sender.try_send(cmd).unwrap();
                replies.push(reply);
            }
            drop(sender);
            while let Some(first) = receiver.blocking_recv() {
                assert!(group(
                    &scheduler,
                    first,
                    &mut receiver,
                    commands,
                    bytes,
                    work
                ));
            }
            assert_eq!(
                lock(&scheduler).unwrap().journal_syncs(),
                initial_syncs + expected_syncs
            );
            for mut reply in replies {
                reply.try_recv().unwrap().unwrap();
            }
        }
    }

    #[tokio::test]
    async fn reaper_reserves_the_next_slot_even_when_normal_requests_keep_arriving() {
        use std::{future::Future, task::Poll};
        let temp = tempfile::tempdir().unwrap();
        let scheduler = scheduler(&temp.path().join("wal"));
        let (sender, mut receiver) = mpsc::channel(1);
        let dispatcher = Dispatcher { sender };
        let (cmd, mut reply) = command(|s| s.submit(task("accepted"), 10));
        dispatcher.sender.try_send(cmd).unwrap();
        let mut reaper = Box::pin(dispatcher.reap());
        std::future::poll_fn(|cx| {
            assert!(reaper.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let first = receiver.try_recv().unwrap();
        // The reaper's waiting reservation owns the freed slot. A new normal
        // request cannot take it before the reaper future gets polled again.
        assert!(
            dispatcher
                .call(|s| s.submit(task("rejected"), 11))
                .await
                .unwrap_err()
                .downcast_ref::<Overloaded>()
                .is_some()
        );
        assert!(group(
            &scheduler,
            first,
            &mut receiver,
            1,
            usize::MAX,
            Duration::from_secs(60)
        ));
        reply.try_recv().unwrap().unwrap();
        std::future::poll_fn(|cx| {
            assert!(reaper.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let maintenance = receiver.try_recv().unwrap();
        assert!(group(
            &scheduler,
            maintenance,
            &mut receiver,
            1,
            usize::MAX,
            Duration::from_secs(60)
        ));
        assert_eq!(reaper.await.unwrap(), 0);
        assert!(lock(&scheduler).unwrap().task("rejected").is_err());
    }

    #[tokio::test]
    async fn closed_dispatcher_rejects_health_and_operations_without_executing() {
        let temp = tempfile::tempdir().unwrap();
        let scheduler = scheduler(&temp.path().join("wal"));
        let (sender, receiver) = mpsc::channel(1);
        drop(receiver);
        let dispatcher = Arc::new(Dispatcher { sender });
        let app = super::super::App {
            scheduler: scheduler.clone(),
            dispatcher: dispatcher.clone(),
        };
        assert_eq!(
            super::super::health(axum::extract::State(app))
                .await
                .err()
                .unwrap()
                .into_response()
                .status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(
            dispatcher
                .call(|s| s.submit(task("never"), 10))
                .await
                .unwrap_err()
                .downcast_ref::<JournalFailure>()
                .is_some()
        );
        assert!(lock(&scheduler).unwrap().task("never").is_err());
    }

    #[tokio::test]
    async fn full_queue_rejects_without_running_and_disconnect_preserves_accepted_work() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("wal");
        let scheduler = scheduler(&path);
        let (sender, receiver) = mpsc::channel(1);
        let dispatcher = Dispatcher { sender };
        let (cmd, reply) = command(|s| s.submit(task("accepted"), 10));
        dispatcher.sender.try_send(cmd).unwrap();
        drop(reply);
        assert!(
            dispatcher
                .call(|s| s.submit(task("rejected"), 11))
                .await
                .unwrap_err()
                .downcast_ref::<Overloaded>()
                .is_some()
        );
        drop(dispatcher);
        // Closing the channel drains work whose client has disappeared.
        let serving = scheduler.clone();
        tokio::task::spawn_blocking(move || serve(serving, receiver))
            .await
            .unwrap();
        assert!(lock(&scheduler).unwrap().task("accepted").is_ok());
        assert!(lock(&scheduler).unwrap().task("rejected").is_err());
        drop(scheduler);
        assert!(
            Scheduler::open(&path, SchedulerConfig::default())
                .unwrap()
                .task("accepted")
                .is_ok()
        );
    }

    #[test]
    fn sync_failure_fails_all_group_replies_and_all_later_access_until_replay() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("wal");
        let scheduler = scheduler(&path);
        let (sender, mut receiver) = mpsc::channel(8);
        let (cmd, mut submitted) = command(|s| s.submit(task("uncertain"), 10));
        sender.try_send(cmd).unwrap();
        let (cmd, mut failure) = command(|s| {
            s.fail_journal_sync();
            Ok(())
        });
        sender.try_send(cmd).unwrap();
        let (cmd, mut query) = command(|s| s.task("uncertain"));
        sender.try_send(cmd).unwrap();
        drop(sender);
        assert!(!group(
            &scheduler,
            receiver.try_recv().unwrap(),
            &mut receiver,
            64,
            usize::MAX,
            Duration::from_secs(60)
        ));
        for error in [
            submitted.try_recv().unwrap().unwrap_err(),
            failure.try_recv().unwrap().unwrap_err(),
            query.try_recv().unwrap().unwrap_err(),
        ] {
            assert!(error.downcast_ref::<JournalFailure>().is_some());
        }
        assert!(
            lock(&scheduler)
                .err()
                .unwrap()
                .downcast_ref::<JournalFailure>()
                .is_some()
        );
        drop(scheduler);
        // An unacknowledged full frame may survive. Only replay decides the
        // authoritative prefix; it is unsafe to roll back in memory and retry.
        assert!(
            Scheduler::open(&path, SchedulerConfig::default())
                .unwrap()
                .task("uncertain")
                .is_ok()
        );
    }
}
