//! Attempt-local ownership for cooperative waits and explicit VM controls.
//!
//! A single obligation owns its timer task until cleanup completes. Cancellation
//! is a latched token, not a best-effort message in a possibly full queue. Native
//! transitions run to completion under `state`; waking invalidates timers before
//! acquiring that lock. No scheduler reservation is released here.

use crate::CooperativeModelWait;
use async_trait::async_trait;
use pvisor_core::RunState;
use pvisor_core::operation::{OperationKind, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as SyncMutex};
use tokio::sync::{Mutex, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Kept internal: production uses the same Run control path as explicit calls.
// async_trait adds a redundant must_use to the already must-use future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub(super) trait WaitControl: Send + Sync {
    fn state(&self) -> RunState;
    fn pending_host_reads(&self) -> u64 {
        0
    }
    async fn idle_permit(&self, _wake: CancellationToken) -> anyhow::Result<Option<u64>> {
        Ok(None)
    }
    async fn idle_pause(&self, _epoch: u64) -> anyhow::Result<bool> {
        Ok(false)
    }
    async fn idle_offload(&self, _epoch: u64) -> anyhow::Result<bool> {
        Ok(false)
    }
    fn supports_resume_rearm(&self) -> bool {
        false
    }
    async fn idle_settled(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn idle_resume_rearm(
        &self,
        _epoch: u64,
        _wake: CancellationToken,
    ) -> anyhow::Result<tokio::sync::oneshot::Receiver<Option<u64>>> {
        anyhow::bail!("resume-rearm unsupported")
    }
    fn can_offload(&self) -> bool;
    async fn execute(&self, operation: OperationKind) -> anyhow::Result<Value>;
}

#[cfg(test)]
mod tests;

#[derive(Default)]
struct Ownership {
    manual_pause: bool,
    automatic: Option<u64>,
    next_offload_at: Option<Instant>,
}

pub(crate) struct WaitCoordinator {
    // Gateway owns a separate Tokio runtime which can shut down before cleanup.
    // Accepted work must stay on the Session owner's runtime instead.
    runtime: tokio::runtime::Handle,
    control: Arc<dyn WaitControl>,
    cancellation: CancellationToken,
    policy: CooperativeModelWait,
    state: Mutex<Ownership>,
    active: SyncMutex<Option<Arc<WaitEntry>>>,
    next_id: AtomicU64,
    manual_epoch: AtomicU64,
    // Conservative transparent fallback: no rearm after any observed overlap.
    automatic_disabled: AtomicBool,
    passive_waits: AtomicU64,
    changed: watch::Sender<()>,
}

impl std::fmt::Debug for WaitCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaitCoordinator")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

pub(super) struct WaitEntry {
    coordinator: std::sync::Weak<WaitCoordinator>,
    id: u64,
    manual_epoch: u64,
    entered: CancellationToken,
    wake: CancellationToken,
    retired: CancellationToken,
    completion: watch::Receiver<Option<Result<(), String>>>,
    rounds: SyncMutex<RoundState>,
    round_changed: watch::Sender<()>,
    last_woken_generation: AtomicU64,
}

struct IdleRound {
    generation: u64,
    epoch: AtomicU64,
    prepared: SyncMutex<Option<tokio::sync::oneshot::Receiver<Option<u64>>>>,
    wake: CancellationToken,
    completion: watch::Sender<Option<Result<(), String>>>,
}

impl IdleRound {
    fn new(generation: u64) -> Arc<Self> {
        Self::with_wake(generation, CancellationToken::new())
    }
    fn with_wake(generation: u64, wake: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            generation,
            epoch: AtomicU64::new(0),
            prepared: SyncMutex::new(None),
            wake,
            completion: watch::channel(None).0,
        })
    }

    async fn completed(&self) -> anyhow::Result<()> {
        let mut completion = self.completion.subscribe();
        loop {
            if let Some(result) = completion.borrow_and_update().clone() {
                return result.map_err(anyhow::Error::msg);
            }
            completion.changed().await?;
        }
    }
}

struct RoundState {
    current: Arc<IdleRound>,
    delivery_held: bool,
    rearm_at: Instant,
}

impl WaitEntry {
    pub(super) fn repeat_stream_batches(&self) -> bool {
        self.coordinator
            .upgrade()
            .is_some_and(|owner| owner.policy.rounds.is_some())
    }

    pub(super) fn stream_buffer(&self) -> Option<pvisor_gateway::model_wait::ModelStreamBuffer> {
        self.coordinator.upgrade().and_then(|owner| {
            if self.wake.is_cancelled() || owner.cancellation.is_cancelled() {
                None
            } else {
                owner.policy.stream_buffer
            }
        })
    }

    fn current_round(&self) -> Arc<IdleRound> {
        self.rounds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current
            .clone()
    }

    /// A consumed wake belongs only to its generation. A new buffer must not
    /// immediately flush because the preceding round has already been revoked.
    pub(super) async fn wait_for_round_wake(&self, cursor: &mut u64) {
        let Some(owner) = self.coordinator.upgrade() else {
            return;
        };
        let mut changed = self.round_changed.subscribe();
        loop {
            // Watch notifications can coalesce while Gateway is busy. Retain
            // the latest completed generation even after current is replaced.
            let woken = self.last_woken_generation.load(Ordering::Acquire);
            if woken > *cursor {
                *cursor = woken;
                return;
            }
            let round = self.current_round();
            tokio::select! {
                biased;
                _ = self.wake.cancelled() => return,
                _ = owner.cancellation.cancelled() => return,
                _ = round.wake.cancelled(), if round.generation > *cursor => {
                    *cursor = round.generation;
                    return;
                },
                _ = self.cleanup_completed() => return,
                _ = changed.changed() => {},
            }
        }
    }

    #[cfg(test)]
    pub(super) async fn wait_for_wake(&self) {
        self.wait_for_round_wake(&mut 0).await;
    }

    /// Hold the VM running until the outgoing batch passes the Gateway body
    /// queue. Taking the hold and selecting a generation are one atomic action.
    pub(super) async fn before_batch(&self) -> anyhow::Result<u64> {
        anyhow::ensure!(!self.retired.is_cancelled(), "model wait already retired");
        let round = {
            let mut rounds = self.rounds.lock().unwrap_or_else(|e| e.into_inner());
            rounds.delivery_held = true;
            rounds.current.clone()
        };
        round.wake.cancel();
        self.round_changed.send_replace(());
        round.completed().await?;
        let owner = self
            .coordinator
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("model wait owner stopped"))?;
        owner.release(self, true).await?;
        Ok(round.generation)
    }

    pub(super) fn after_batch(&self) {
        if let Some(owner) = self.coordinator.upgrade() {
            let delay = owner
                .policy
                .rounds
                .map_or(std::time::Duration::ZERO, |p| p.recheck_after);
            let mut rounds = self.rounds.lock().unwrap_or_else(|e| e.into_inner());
            rounds.delivery_held = false;
            rounds.rearm_at = Instant::now() + delay;
            self.round_changed.send_replace(());
        }
    }

    pub(super) fn enter(&self) {
        self.entered.cancel();
    }

    pub(super) fn cancel(&self) {
        self.wake.cancel();
        self.retired.cancel();
    }

    pub(super) async fn before_delivery(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.retired.is_cancelled(), "model wait already retired");
        self.wake.cancel();
        self.cleanup_completed().await?;
        let coordinator = self
            .coordinator
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("model wait owner stopped"))?;
        // Explicit resume can end automatic cleanup long before the supplier
        // replies. Recheck current human ownership and termination at delivery.
        coordinator.release(self, true).await?;
        self.retired.cancel();
        coordinator.clear_active(self.id);
        Ok(())
    }

    async fn cleanup_completed(&self) -> anyhow::Result<()> {
        let mut completion = self.completion.clone();
        loop {
            if let Some(result) = completion.borrow_and_update().clone() {
                return result.map_err(anyhow::Error::msg);
            }
            completion
                .changed()
                .await
                .map_err(|_| anyhow::anyhow!("model wait coordinator stopped"))?;
        }
    }
}

impl WaitCoordinator {
    pub(super) fn new(
        control: Arc<dyn WaitControl>,
        cancellation: CancellationToken,
        policy: CooperativeModelWait,
    ) -> Arc<Self> {
        Arc::new(Self {
            runtime: tokio::runtime::Handle::current(),
            control,
            cancellation,
            policy,
            state: Mutex::new(Ownership::default()),
            active: SyncMutex::new(None),
            next_id: AtomicU64::new(1),
            manual_epoch: AtomicU64::new(0),
            automatic_disabled: AtomicBool::new(false),
            passive_waits: AtomicU64::new(0),
            changed: watch::channel(()).0,
        })
    }

    pub(super) fn downloads_enabled(&self) -> bool {
        self.policy.downloads
    }
    pub(super) fn start_host_reads(self: &Arc<Self>) {
        if !self.policy.filesystem_reads {
            return;
        }
        let owner = self.clone();
        self.runtime.spawn(async move {
            let mut entry: Option<Arc<WaitEntry>> = None;
            loop {
                tokio::select! {
                    _ = owner.cancellation.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {},
                }
                if owner.control.pending_host_reads() > 0 {
                    if entry.is_none()
                        && let Ok(Some(wait)) = owner.reserve_inner(true, true)
                    {
                        tracing::debug!(
                            wait_id = wait.id,
                            source = "filesystem_read",
                            "Host wait observed"
                        );
                        wait.enter();
                        entry = Some(wait);
                    }
                } else if let Some(wait) = entry.take() {
                    // Device completion independently wakes BEFORE RAM reentry.
                    if wait.before_delivery().await.is_err() {
                        wait.cancel();
                    }
                }
            }
            if let Some(wait) = entry {
                wait.cancel();
            }
        });
    }

    // Unit tests can request an automatic obligation explicitly. Production
    // observes requests and falls back to passive delivery on overlap.
    #[cfg(test)]
    pub(super) fn reserve(self: &Arc<Self>) -> anyhow::Result<Arc<WaitEntry>> {
        self.reserve_inner(false, false)?
            .ok_or_else(|| anyhow::anyhow!("automatic model wait unavailable"))
    }

    fn reserve_inner(
        self: &Arc<Self>,
        observed: bool,
        skip_overlap: bool,
    ) -> anyhow::Result<Option<Arc<WaitEntry>>> {
        anyhow::ensure!(
            !self.cancellation.is_cancelled() && self.available(),
            "attempt is not live"
        );
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if self.automatic_disabled.load(Ordering::Acquire) || active.is_some() {
            if skip_overlap {
                return Ok(None);
            }
            self.automatic_disabled.store(true, Ordering::Release);
            if let Some(previous) = active.as_ref() {
                previous.wake.cancel();
            }
            if observed {
                // Count before releasing active so manual controls cannot miss
                // an admitted passive request when the old wait retires.
                self.passive_waits.fetch_add(1, Ordering::AcqRel);
                return Ok(None);
            }
            anyhow::bail!("automatic model waits disabled after overlapping calls");
        }
        let (completion, result) = watch::channel(None);
        let entry = Arc::new(WaitEntry {
            coordinator: Arc::downgrade(self),
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            manual_epoch: self.manual_epoch.load(Ordering::Acquire),
            entered: CancellationToken::new(),
            wake: CancellationToken::new(),
            retired: CancellationToken::new(),
            completion: result,
            rounds: SyncMutex::new(RoundState {
                current: IdleRound::new(1),
                delivery_held: false,
                rearm_at: Instant::now(),
            }),
            round_changed: watch::channel(()).0,
            last_woken_generation: AtomicU64::new(0),
        });
        *active = Some(entry.clone());
        let coordinator = self.clone();
        let task_entry = entry.clone();
        self.runtime.spawn(async move {
            let result = coordinator.run_wait(&task_entry).await;
            if let Err(error) = &result {
                tracing::warn!(wait_id = task_entry.id, %error, "cooperative wait ended without delivery");
            }
            completion.send_replace(Some(result.map_err(|error| error.to_string())));
            // Native cleanup may finish on explicit resume before the HTTP call
            // ends. Keep the single-call slot until delivery or handler drop.
            tokio::select! {
                _ = task_entry.retired.cancelled() => {},
                _ = coordinator.cancellation.cancelled() => {},
            }
            coordinator.clear_active(task_entry.id);
        });
        Ok(Some(entry))
    }

    /// Host observation must not reject normal parallel model requests merely
    /// because the optimization supports only one wait. Keep a delivery barrier
    /// but disable further automatic transitions after the first overlap.
    pub(super) fn observe(self: &Arc<Self>) -> anyhow::Result<Option<Arc<WaitEntry>>> {
        self.reserve_inner(true, false)
    }

    pub(super) fn finish_passive(&self) {
        self.passive_waits.fetch_sub(1, Ordering::AcqRel);
    }

    pub(super) async fn passive_delivery(&self) -> anyhow::Result<()> {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(entry) = active {
            entry.cleanup_completed().await?;
        }
        let mut changed = self.changed.subscribe();
        loop {
            let ownership = self.state.lock().await;
            anyhow::ensure!(
                !self.cancellation.is_cancelled() && self.available(),
                "attempt ended during model wait"
            );
            if !ownership.manual_pause {
                anyhow::ensure!(
                    matches!(
                        self.control.state(),
                        RunState::Running | RunState::Created | RunState::Starting
                    ),
                    "VM is not running for model delivery"
                );
                return Ok(());
            }
            drop(ownership);
            tokio::select! {
                _ = self.cancellation.cancelled() => anyhow::bail!("attempt cancelled during model wait"),
                result = changed.changed() => { result?; },
            }
        }
    }

    fn clear_active(&self, id: u64) {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if active.as_ref().is_some_and(|entry| entry.id == id) {
            *active = None;
        }
    }

    fn live(&self) -> bool {
        matches!(
            self.control.state(),
            RunState::Running | RunState::Suspended
        )
    }

    fn available(&self) -> bool {
        matches!(
            self.control.state(),
            RunState::Created | RunState::Starting | RunState::Running | RunState::Suspended
        )
    }

    async fn until(&self, entry: &WaitEntry, round: &IdleRound, deadline: Instant) -> bool {
        tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => false,
            _ = entry.wake.cancelled() => false,
            _ = round.wake.cancelled() => false,
            _ = tokio::time::sleep_until(deadline) => true,
        }
    }

    async fn run_wait(&self, entry: &WaitEntry) -> anyhow::Result<()> {
        tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => {},
            _ = entry.wake.cancelled() => {},
            _ = entry.entered.cancelled() => {},
        }
        loop {
            let round = entry.current_round();
            let result = self.run_round(entry, &round).await;
            // Always revoke, including errors before pause, so the owned idle
            // client retires this epoch before a later prepare can acquire it.
            round.wake.cancel();
            entry
                .last_woken_generation
                .store(round.generation, Ordering::Release);
            entry.round_changed.send_replace(());
            let settled = self.control.idle_settled().await;
            let mut result = result.and(settled);
            let rearmed = if result.is_ok() {
                match self.resume_next(entry, &round).await {
                    Ok(value) => value,
                    Err(error) => {
                        self.cancellation.cancel();
                        result = Err(error);
                        false
                    }
                }
            } else {
                false
            };
            if !rearmed && result.is_ok() {
                result = self.release(entry, false).await;
            }
            round
                .completion
                .send_replace(Some(result.as_ref().map(|_| ()).map_err(|e| e.to_string())));
            tracing::debug!(
                wait_id = entry.id,
                generation = round.generation,
                "model idle round retired"
            );
            if let Err(error) = &result {
                self.cancellation.cancel();
                entry
                    .current_round()
                    .completion
                    .send_replace(Some(Err(error.to_string())));
            }
            result?;
            if rearmed {
                continue;
            }
            let Some(policy) = self.policy.rounds else {
                return Ok(());
            };
            let earliest = Instant::now() + policy.recheck_after;
            let mut changed = entry.round_changed.subscribe();
            loop {
                if entry.wake.is_cancelled() || self.cancellation.is_cancelled() {
                    return Ok(());
                }
                let deadline = {
                    let mut rounds = entry.rounds.lock().unwrap_or_else(|e| e.into_inner());
                    let deadline = earliest.max(rounds.rearm_at);
                    if !rounds.delivery_held && Instant::now() >= deadline {
                        rounds.current = IdleRound::new(round.generation + 1);
                        entry.round_changed.send_replace(());
                        break;
                    }
                    (!rounds.delivery_held).then_some(deadline)
                };
                tokio::select! {
                    _ = entry.wake.cancelled() => return Ok(()),
                    _ = self.cancellation.cancelled() => return Ok(()),
                    _ = changed.changed() => {},
                    _ = async {
                        if let Some(deadline) = deadline {
                            tokio::time::sleep_until(deadline).await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                    } => {},
                }
            }
        }
    }

    async fn run_round(&self, entry: &WaitEntry, round: &IdleRound) -> anyhow::Result<()> {
        let started = Instant::now();
        let mut idle_epoch = None;
        let prepared = round
            .prepared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if prepared.is_none()
            && !self
                .until(entry, round, started + self.policy.pause_after)
                .await
        {
            return Ok(());
        }
        if self.policy.require_guest_idle {
            let epoch = tokio::select! {
                biased;
                _ = self.cancellation.cancelled() => None,
                _ = entry.wake.cancelled() => None,
                _ = round.wake.cancelled() => None,
                value = async {
                    if let Some(rx) = prepared { Ok(rx.await?) }
                    else { self.control.idle_permit(round.wake.clone()).await }
                } => value?,
            };
            let Some(epoch) = epoch else {
                return Ok(());
            };
            let mut ownership = self.state.lock().await;
            if entry.wake.is_cancelled()
                || round.wake.is_cancelled()
                || self.cancellation.is_cancelled()
                || !self.live()
                || ownership.manual_pause
                || entry.manual_epoch != self.manual_epoch.load(Ordering::Acquire)
            {
                return Ok(());
            }
            // Native transitions are never dropped on wake; cleanup follows ACK.
            let paused = self
                .control
                .idle_pause(epoch)
                .await
                .inspect_err(|_| self.cancellation.cancel())?;
            if !paused || self.control.state() != RunState::Suspended {
                return Ok(());
            }
            ownership.automatic = Some(entry.id);
            idle_epoch = Some(epoch);
            round.epoch.store(epoch, Ordering::Release);
        } else {
            self.automatic(entry, round, false, None).await?;
        }
        if let Some(delay) = self.policy.offload_after
            && self.until(entry, round, started + delay).await
        {
            self.automatic(entry, round, true, idle_epoch).await?;
        }
        tokio::select! {
            _ = self.cancellation.cancelled() => {},
            _ = entry.wake.cancelled() => {},
            _ = round.wake.cancelled() => {},
        }
        Ok(())
    }

    async fn automatic(
        &self,
        entry: &WaitEntry,
        round: &IdleRound,
        offload: bool,
        idle_epoch: Option<u64>,
    ) -> anyhow::Result<()> {
        let mut ownership = self.state.lock().await;
        if entry.wake.is_cancelled()
            || round.wake.is_cancelled()
            || self.cancellation.is_cancelled()
            || !self.live()
            || entry.manual_epoch != self.manual_epoch.load(Ordering::Acquire)
            || ownership.manual_pause
        {
            return Ok(());
        }
        let operation = if offload {
            if ownership.automatic != Some(entry.id) || !self.control.can_offload() {
                return Ok(());
            }
            if ownership
                .next_offload_at
                .is_some_and(|at| Instant::now() < at)
            {
                if std::env::var("PVISOR_DIAG_OFFLOAD").as_deref() == Ok("1") {
                    let record = serde_json::json!({"source":"coordinator", "host_pid":std::process::id(),
                        "epoch":idle_epoch, "reason":"cooldown", "wait_id":entry.id,
                        "cooldown_remaining_ns":ownership.next_offload_at.unwrap().saturating_duration_since(Instant::now()).as_nanos(),
                        "unix_ns":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()});
                    use std::io::Write;
                    let _ = writeln!(
                        std::io::stderr().lock(),
                        "pvisor-offload-diagnostic {record}"
                    );
                }
                return Ok(());
            }
            if self.policy.require_guest_idle {
                let Some(epoch) = idle_epoch else {
                    round.wake.cancel();
                    return Ok(());
                };
                // Do not select against wake: native reclaim and backing
                // publication must finish before release can reopen RAM writers.
                let offloaded = self
                    .control
                    .idle_offload(epoch)
                    .await
                    .inspect_err(|_| self.cancellation.cancel())?;
                if offloaded && let Some(policy) = self.policy.rounds {
                    ownership.next_offload_at = Some(Instant::now() + policy.offload_cooldown);
                }
                // A denied reclaim leaves a valid pause intact. In particular a
                // short timer budget must not manufacture an immediate resume.
                // The independent idle channel still cancels revoked/expired
                // epochs, and delivery/manual control retains normal ownership.
                return Ok(());
            }
            OperationKind::RunOffload { file: None }
        } else {
            if self.control.state() != RunState::Running {
                return Ok(());
            }
            ownership.automatic = Some(entry.id);
            OperationKind::RunPause
        };
        // Do not drop this future on wake: completion includes backing publication.
        self.execute(operation).await?;
        if offload && let Some(policy) = self.policy.rounds {
            ownership.next_offload_at = Some(Instant::now() + policy.offload_cooldown);
        }
        Ok(())
    }

    async fn resume_next(&self, entry: &WaitEntry, old: &IdleRound) -> anyhow::Result<bool> {
        if self.policy.rounds.is_none() || !self.control.supports_resume_rearm() {
            return Ok(false);
        }
        let mut ownership = self.state.lock().await;
        if ownership.automatic != Some(entry.id)
            || ownership.manual_pause
            || entry.wake.is_cancelled()
            || self.cancellation.is_cancelled()
            || self.automatic_disabled.load(Ordering::Acquire)
            || entry.manual_epoch != self.manual_epoch.load(Ordering::Acquire)
        {
            return Ok(false);
        }
        let epoch = old.epoch.load(Ordering::Acquire);
        if epoch == 0 {
            return Ok(false);
        }
        let next = {
            let mut rounds = entry.rounds.lock().unwrap_or_else(|e| e.into_inner());
            if rounds.delivery_held || Instant::now() < rounds.rearm_at {
                return Ok(false);
            }
            let next = IdleRound::with_wake(old.generation + 1, entry.wake.child_token());
            // Publish before native release so a concurrent delivery hold can
            // revoke this exact new round and wait for its cleanup completion.
            rounds.current = next.clone();
            next
        };
        if std::env::var("PVISOR_DIAG_IDLE").as_deref() == Ok("1") {
            eprintln!(
                "pvisor-coordinator-idle {}",
                serde_json::json!({
                    "event":"resume_rearm_requested", "wait_id":entry.id,
                    "old_generation":old.generation, "new_generation":next.generation,
                    "old_epoch":epoch, "unix_ms":crate::unix_now_ms()
                })
            );
        }
        let rx = self
            .control
            .idle_resume_rearm(epoch, next.wake.clone())
            .await?;
        *next.prepared.lock().unwrap_or_else(|e| e.into_inner()) = Some(rx);
        ownership.automatic = None;
        entry.round_changed.send_replace(());
        Ok(true)
    }

    async fn release(&self, entry: &WaitEntry, delivery: bool) -> anyhow::Result<()> {
        let mut changed = self.changed.subscribe();
        loop {
            let mut ownership = self.state.lock().await;
            anyhow::ensure!(
                !self.cancellation.is_cancelled() && self.available(),
                "attempt ended during model wait"
            );
            if delivery {
                anyhow::ensure!(!entry.retired.is_cancelled(), "model wait already retired");
            }
            if ownership.manual_pause && !delivery {
                // Human ownership needs no automatic resume. Actual response
                // delivery has its own live barrier and still waits below.
                return Ok(());
            }
            if !ownership.manual_pause {
                if ownership.automatic == Some(entry.id) {
                    self.execute(OperationKind::RunResume).await?;
                    ownership.automatic = None;
                }
                anyhow::ensure!(
                    matches!(
                        self.control.state(),
                        RunState::Running | RunState::Created | RunState::Starting
                    ),
                    "VM is not running for model delivery"
                );
                return Ok(());
            }
            drop(ownership);
            // Manual pause remains authoritative; cancellation still unblocks cleanup.
            tokio::select! {
                _ = self.cancellation.cancelled() => anyhow::bail!("attempt cancelled during model wait"),
                result = changed.changed() => { result?; },
            }
        }
    }

    async fn execute(&self, operation: OperationKind) -> anyhow::Result<Value> {
        match self.control.execute(operation).await {
            Ok(value) => Ok(value),
            Err(error) => {
                // Generic control errors can follow an effect (including event
                // publication failure). Never guess that a failed pause did nothing.
                self.cancellation.cancel();
                Err(error)
            }
        }
    }

    /// Explicit controls share ownership ordering and survive caller cancellation.
    pub(super) async fn control(
        self: &Arc<Self>,
        operation: OperationKind,
    ) -> anyhow::Result<Value> {
        operation.validate()?;
        anyhow::ensure!(
            matches!(
                operation,
                OperationKind::RunPause
                    | OperationKind::RunResume
                    | OperationKind::RunOffload { .. }
                    | OperationKind::RunCheckpoint { .. }
                    | OperationKind::RunSuspend { .. }
            ),
            "unsupported live VM control primitive"
        );
        let coordinator = self.clone();
        self.runtime
            .spawn(async move { coordinator.manual(operation).await })
            .await?
    }

    async fn manual(&self, operation: OperationKind) -> anyhow::Result<Value> {
        let mut ownership = self.state.lock().await;
        let snapshot = matches!(
            operation,
            OperationKind::RunCheckpoint { .. } | OperationKind::RunSuspend { .. }
        );
        {
            let active = self.active.lock().unwrap_or_else(|e| e.into_inner());
            anyhow::ensure!(
                !snapshot
                    || (active.is_none()
                        && self.passive_waits.load(Ordering::Acquire) == 0
                        && !ownership.manual_pause),
                "checkpoint/suspend conflicts with cooperative wait or manual pause"
            );
            self.manual_epoch.fetch_add(1, Ordering::AcqRel);
            if let Some(entry) = active.as_ref() {
                entry.wake.cancel();
            }
        }
        let paused = matches!(
            operation,
            OperationKind::RunPause | OperationKind::RunOffload { .. }
        );
        let resumed = matches!(operation, OperationKind::RunResume);
        if paused {
            ownership.manual_pause = true;
        }
        let result = self.execute(operation).await;
        if result.is_ok() {
            if resumed {
                ownership.manual_pause = false;
            }
            ownership.automatic = None;
        }
        self.changed.send_replace(());
        result
    }
}
