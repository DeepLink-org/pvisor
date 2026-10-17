//! EXP-003 M0: one trusted whole-guest wait, owned by one live Attempt.
//!
//! Native resource admission remains held. This does not infer idleness from
//! traffic, transfer dirty guest pages, or provide a recoverable task queue.

use super::RunControlHandle;
use crate::NativeModelWait;
use pvisor_core::operation::OperationKind;
use pvisor_core::{ModelCallRequest, RunState};
use pvisor_gateway::model_wait::{ModelWait, ModelWaitLifecycle};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Default)]
struct Ownership {
    next: u64,
    reserved: Option<u64>,
    owner: Option<u64>,
    manual_epoch: u64,
}

#[derive(Default)]
pub(super) struct WaitRegistry(Mutex<Ownership>);

impl WaitRegistry {
    // Called under the same native transition lock as automatic entry/delivery.
    pub(super) fn manual_control(&self, kind: &OperationKind) -> anyhow::Result<()> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match kind {
            OperationKind::RunCheckpoint { .. } | OperationKind::RunSuspend { .. } => {
                anyhow::ensure!(
                    state.reserved.is_none(),
                    "execution capture cannot include an active native model wait"
                );
            }
            OperationKind::RunPause
            | OperationKind::RunResume
            | OperationKind::RunOffload { .. } => {
                state.manual_epoch = state
                    .manual_epoch
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("native control epoch exhausted"))?;
                state.owner = None;
            }
            _ => {}
        }
        Ok(())
    }
}

pub(crate) struct NativeLifecycle {
    controls: RunControlHandle,
    mode: NativeModelWait,
}

impl std::fmt::Debug for NativeLifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeModelWait")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl NativeLifecycle {
    pub(crate) fn new(controls: RunControlHandle, mode: NativeModelWait) -> Self {
        Self { controls, mode }
    }
}

// This reference owns the single reservation through native ACK and cleanup,
// even when Gateway drops its future. It never holds guest memory or credentials.
struct Reservation {
    controls: RunControlHandle,
    epoch: u64,
    manual_epoch: u64,
    cancelled: AtomicBool,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut state = self
            .controls
            .waits
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if state.reserved == Some(self.epoch) {
            state.reserved = None;
            state.owner = None;
        }
    }
}

impl ModelWaitLifecycle for NativeLifecycle {
    fn reserve(&self, request: ModelCallRequest) -> anyhow::Result<Box<dyn ModelWait>> {
        self.controls.ensure_live()?;
        let status = self.controls.status();
        anyhow::ensure!(
            request.run_id.as_ref() == Some(&status.run_id)
                && request.attempt_id.as_ref() == Some(&status.attempt.attempt_id),
            "model wait identity does not match the bound Attempt"
        );
        let mut state = self
            .controls
            .waits
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        anyhow::ensure!(
            state.reserved.is_none(),
            "whole-guest model wait already reserved"
        );
        state.next = state
            .next
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("model wait epoch exhausted"))?;
        state.reserved = Some(state.next);
        Ok(Box::new(NativeWait {
            reservation: Some(Arc::new(Reservation {
                controls: self.controls.clone(),
                epoch: state.next,
                manual_epoch: state.manual_epoch,
                cancelled: AtomicBool::new(false),
            })),
            mode: self.mode,
            entered: false,
        }))
    }
}

struct NativeWait {
    reservation: Option<Arc<Reservation>>,
    mode: NativeModelWait,
    entered: bool,
}

impl NativeWait {
    fn cancel_inner(&mut self) {
        let Some(reservation) = self.reservation.take() else {
            return;
        };
        reservation.cancelled.store(true, Ordering::Release);
        // At most one cleanup can exist per Attempt. The reservation stays held
        // until this task and any accepted entry/delivery operation finish.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let controls = &reservation.controls;
                let _transition = controls.vm_control.transition.lock().await;
                if controls.ensure_live().is_err() {
                    return;
                }
                let owns_pause = controls
                    .waits
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .owner
                    == Some(reservation.epoch);
                if owns_pause {
                    // A rejected/uncertain cleanup must never leave an
                    // unowned automatic pause in a nominally live Attempt.
                    if controls
                        .control_locked(OperationKind::RunResume, Some(reservation.epoch))
                        .await
                        .is_err()
                    {
                        controls.cancel();
                    }
                }
            });
        } else {
            // There is no executor on which to acknowledge cleanup. Fail stop.
            reservation.controls.cancel();
        }
    }
}

impl Drop for NativeWait {
    fn drop(&mut self) {
        self.cancel_inner();
    }
}

#[async_trait::async_trait]
impl ModelWait for NativeWait {
    async fn enter(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.entered, "model wait already entered");
        let reservation = self
            .reservation
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("model wait already released"))?
            .clone();
        let mode = self.mode;
        tokio::spawn(async move {
            let controls = &reservation.controls;
            let _transition = controls.vm_control.transition.lock().await;
            controls.ensure_live()?;
            anyhow::ensure!(
                !reservation.cancelled.load(Ordering::Acquire),
                "model wait cancelled"
            );
            anyhow::ensure!(
                controls.status().state == RunState::Running,
                "model wait cannot acquire a manually paused guest"
            );
            {
                let mut state = controls.waits.0.lock().unwrap_or_else(|e| e.into_inner());
                anyhow::ensure!(
                    state.reserved == Some(reservation.epoch)
                        && state.manual_epoch == reservation.manual_epoch,
                    "model wait was superseded by manual control"
                );
                state.owner = Some(reservation.epoch);
            }
            controls
                .control_locked(
                    match mode {
                        NativeModelWait::Pause => OperationKind::RunPause,
                        NativeModelWait::Offload => OperationKind::RunOffload { file: None },
                    },
                    Some(reservation.epoch),
                )
                .await?;
            controls.ensure_live()?;
            anyhow::ensure!(
                !reservation.cancelled.load(Ordering::Acquire),
                "model wait cancelled"
            );
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        self.entered = true;
        Ok(())
    }

    async fn before_delivery(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(self.entered, "model wait has not entered");
        let reservation = self
            .reservation
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("model wait already released"))?
            .clone();
        let mut status = reservation.controls.status.clone();
        loop {
            status.borrow_and_update();
            let pending = reservation.clone();
            let delivered = tokio::spawn(async move {
                let controls = &pending.controls;
                let _transition = controls.vm_control.transition.lock().await;
                controls.ensure_live()?;
                anyhow::ensure!(
                    !pending.cancelled.load(Ordering::Acquire),
                    "model wait cancelled"
                );
                let owns_pause = controls
                    .waits
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .owner
                    == Some(pending.epoch);
                if owns_pause {
                    controls
                        .control_locked(OperationKind::RunResume, Some(pending.epoch))
                        .await?;
                    controls
                        .waits
                        .0
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .owner = None;
                }
                controls.ensure_live()?;
                Ok::<_, anyhow::Error>(controls.status().state == RunState::Running)
            })
            .await??;
            if delivered {
                self.reservation.take();
                return Ok(());
            }
            // Manual pause/offload took ownership. Only its explicit resume can
            // permit delivery; cancellation/termination fences the old result.
            tokio::select! {
                _ = reservation.controls.cancellation.cancelled() => anyhow::bail!("attempt is cancelling"),
                result = status.changed() => { result.map_err(|_| anyhow::anyhow!("attempt status closed"))?; }
            }
        }
    }

    fn cancel(&mut self) {
        self.cancel_inner();
    }
}
