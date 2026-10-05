use super::*;

impl Scheduler {
    pub fn inference_wait_record(&self, id: &str) -> anyhow::Result<Option<InferenceWaitRecord>> {
        ensure!(self.tasks.contains_key(id), "unknown task");
        Ok(self.inference_waits.get(id).cloned())
    }
    pub(super) fn control(&self, id: &str, revision: u64) -> Option<&ControlRecord> {
        self.tasks[id]
            .controls
            .iter()
            .chain(
                self.inference_controls
                    .get(id)
                    .into_iter()
                    .flat_map(|r| r.iter()),
            )
            .find(|c| c.command.revision == revision)
    }

    pub(super) fn control_mut(&mut self, id: &str, revision: u64) -> Option<&mut ControlRecord> {
        if let Some(record) = self
            .tasks
            .get_mut(id)?
            .controls
            .iter_mut()
            .find(|c| c.command.revision == revision)
        {
            return Some(record);
        }
        self.inference_controls
            .get_mut(id)?
            .iter_mut()
            .find(|c| c.command.revision == revision)
    }

    pub(super) fn latest_control(&self, id: &str) -> Option<&ControlRecord> {
        self.tasks[id]
            .controls
            .last()
            .into_iter()
            .chain(self.inference_controls.get(id).and_then(|r| r.back()))
            .max_by_key(|c| c.command.revision)
    }

    pub(super) fn interrupt_inference(&mut self, id: &str, at: u64) {
        if let Some(wait) = self.inference_waits.get_mut(id) {
            wait.interrupted = true;
            wait.updated_at_ms = at;
        }
        if let Some(controls) = self.inference_controls.get_mut(id) {
            for control in controls.iter_mut().filter(|c| !c.phase.terminal()) {
                control.phase = ControlPhase::Aborted;
                control.completed_at_ms = Some(at);
            }
        }
    }

    pub(super) fn inference_resume(
        &self,
        mut wait: InferenceWaitRecord,
        now: u64,
    ) -> anyhow::Result<Change> {
        let id = &wait.key.lease.task_id;
        let revision = self
            .latest_control(id)
            .context("missing pause")?
            .command
            .revision
            .checked_add(1)
            .context("control revision overflow")?;
        let control = ControlRecord {
            command: ControlCommand {
                key: wait.key.lease.clone(),
                revision,
                request: ControlRequest {
                    request_id: format!("{INFERENCE_CONTROL_PREFIX}{}-resume", wait.key.revision),
                    action: ControlAction::Resume,
                },
            },
            phase: ControlPhase::Pending,
            outcome: None,
            requested_at_ms: now,
            issued_at_ms: None,
            completed_at_ms: None,
            admission: Resources::default(),
        };
        wait.resume_revision = Some(control.command.revision);
        wait.updated_at_ms = now;
        Ok(Change::InferenceWait {
            record: wait,
            control: Some(control),
            abort_pause: false,
        })
    }

    fn inference_receipt(&self, id: &str) -> anyhow::Result<InferenceWaitReceipt> {
        let wait = &self.inference_waits[id];
        let task = &self.tasks[id];
        let running = task.phase == TaskPhase::Running
            && task.current_reservation() == task.spec.resources
            && self.latest_control(id).is_none_or(|c| c.phase.terminal());
        let paused = !wait.interrupted
            && wait.resume_revision.is_none()
            && self
                .control(id, wait.pause_revision)
                .is_some_and(|c| c.phase == ControlPhase::Succeeded);
        Ok(InferenceWaitReceipt {
            record: wait.clone(),
            entered: paused || (wait.ready && running),
            delivery_ready: wait.ready && running,
        })
    }

    pub fn inference_wait(
        &mut self,
        request: InferenceWaitRequest,
        now: u64,
    ) -> anyhow::Result<InferenceWaitReceipt> {
        self.reap(now)?;
        let key = request.key;
        let id = &key.lease.task_id;
        ensure!(
            key.revision > 0 && !key.call_id.is_empty() && key.call_id.len() <= 256,
            "invalid inference wait identity"
        );
        ensure!(
            self.reported_key(&key.lease, now),
            "stale inference wait lease"
        );
        ensure!(
            !self.tasks[id].reconciliation_pending,
            "inference wait needs a fresh Worker lease report"
        );
        ensure!(
            !matches!(
                self.tasks[id].phase,
                TaskPhase::Cancelling | TaskPhase::RetainingArtifacts | TaskPhase::Suspending
            ),
            "task ended inference execution"
        );
        if request.intent == InferenceWaitIntent::Begin {
            if let Some(previous) = self.inference_waits.get(id) {
                if previous.key == key {
                    return self.inference_receipt(id);
                }
                ensure!(
                    previous.key.lease != key.lease || key.revision > previous.key.revision,
                    "stale or conflicting inference wait revision"
                );
                ensure!(
                    previous.key.lease != key.lease
                        || (previous.ready && self.inference_receipt(id)?.delivery_ready),
                    "another inference wait still owns the pause"
                );
            }
            ensure!(
                self.tasks[id].spec.gateway.is_some(),
                "inference wait requires an Attempt Gateway"
            );
            ensure!(
                self.tasks[id].phase == TaskPhase::Running,
                "inference wait requires running execution"
            );
            let control = self.prepare_control(
                id,
                ControlRequest {
                    request_id: format!("{INFERENCE_CONTROL_PREFIX}{}-pause", key.revision),
                    action: ControlAction::Pause,
                },
                now,
            )?;
            ensure!(
                control.command.revision < u64::MAX,
                "no revision available for inference resume"
            );
            ensure!(
                self.workers[&key.lease.worker_id]
                    .registration
                    .vm_control_actions
                    .contains(&ControlAction::Resume),
                "worker cannot resume inference waits"
            );
            let record = InferenceWaitRecord {
                key: key.clone(),
                pause_revision: control.command.revision,
                resume_revision: None,
                ready: false,
                interrupted: false,
                requested_at_ms: now,
                updated_at_ms: now,
            };
            self.commit(vec![Change::InferenceWait {
                record,
                control: Some(control),
                abort_pause: false,
            }])?;
        } else {
            // A handler can disappear while Begin is still in flight. Persist
            // a Ready tombstone first; a late identical Begin cannot pause it.
            if request.intent == InferenceWaitIntent::Ready
                && self.inference_waits.get(id).is_none_or(|w| w.key != key)
            {
                if let Some(previous) = self.inference_waits.get(id) {
                    ensure!(
                        previous.key.lease != key.lease
                            || (key.revision > previous.key.revision
                                && self.inference_receipt(id)?.delivery_ready),
                        "stale or overlapping inference cleanup"
                    );
                }
                ensure!(
                    self.tasks[id].spec.gateway.is_some()
                        && self.tasks[id].spec.execution.executor
                            == pvisor_core::ExecutorKind::VirtualMachine,
                    "inference cleanup requires VM Gateway execution"
                );
                self.commit(vec![Change::InferenceWait {
                    record: InferenceWaitRecord {
                        key: key.clone(),
                        pause_revision: 0,
                        resume_revision: None,
                        ready: true,
                        interrupted: false,
                        requested_at_ms: now,
                        updated_at_ms: now,
                    },
                    control: None,
                    abort_pause: false,
                }])?;
                return self.inference_receipt(id);
            }
            let mut record = self
                .inference_waits
                .get(id)
                .context("unknown inference wait")?
                .clone();
            ensure!(
                record.key == key,
                "stale or conflicting inference wait identity"
            );
            if request.intent == InferenceWaitIntent::Ready && !record.ready {
                record.ready = true;
                record.updated_at_ms = now;
                let pause = self
                    .control(id, record.pause_revision)
                    .context("missing inference pause")?;
                let change = if !record.interrupted && pause.phase == ControlPhase::Succeeded {
                    self.inference_resume(record, now)?
                } else {
                    let abort_pause = !record.interrupted && pause.phase == ControlPhase::Pending;
                    Change::InferenceWait {
                        record,
                        control: None,
                        abort_pause,
                    }
                };
                self.commit(vec![change])?;
            }
        }
        self.inference_receipt(id)
    }
}
