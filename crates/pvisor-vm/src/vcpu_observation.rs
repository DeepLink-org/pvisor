use crate::api::{
    VcpuObservation, VcpuObservationControl, VcpuObservationRejection as Rejection,
    VcpuObservationSnapshot, VcpuObservedState as State, VmmHandle,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use std::time::{Duration, Instant};

/// One VM-local collector, no worker or unbounded event stream. The atomic is
/// only a disabled fast path; the lock is the authoritative enable boundary.
pub(crate) struct Collector {
    enabled: AtomicBool,
    origin: Instant,
    data: Mutex<VcpuObservationSnapshot>,
}

impl Collector {
    pub(crate) fn new(count: usize) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            origin: Instant::now(),
            data: Mutex::new(VcpuObservationSnapshot {
                hypervisor: if cfg!(target_os = "macos") {
                    crate::api::Hypervisor::Hvf
                } else {
                    crate::api::Hypervisor::Kvm
                },
                enabled: false,
                session: 0,
                topology_generation: 1,
                sequence: 0,
                sampled_at: Duration::ZERO,
                vcpus: (0..count)
                    .map(|id| VcpuObservation {
                        id: id as u8,
                        online: true,
                        state: State::Unknown,
                        sequence: 0,
                        since: Duration::ZERO,
                        transitions: 0,
                        wait_entries: 0,
                        wait_exits: 0,
                        completed_wait: Duration::ZERO,
                    })
                    .collect(),
                all_waiting: false,
                idle_epoch: 0,
                all_waiting_since: None,
                completed_all_waiting: Duration::ZERO,
                rejection: Rejection::Disabled,
            }),
        }
    }

    pub(crate) fn enable(&self, enabled: bool) -> Result<(), String> {
        let mut data = self.data.lock().map_err(|_| "observation lock poisoned")?;
        if data.enabled == enabled {
            return Ok(());
        }
        let now = self.origin.elapsed();
        Self::close_window(&mut data, now);
        data.enabled = enabled;
        data.sequence = data.sequence.saturating_add(1);
        if enabled {
            data.session = data.session.saturating_add(1);
            let sequence = data.sequence;
            for cpu in &mut data.vcpus {
                cpu.state = if cpu.online {
                    State::Unknown
                } else {
                    State::Stopped
                };
                cpu.since = now;
                cpu.sequence = sequence;
                cpu.transitions = 0;
                cpu.wait_entries = 0;
                cpu.wait_exits = 0;
                cpu.completed_wait = Duration::ZERO;
            }
            data.completed_all_waiting = Duration::ZERO;
        } else {
            let sequence = data.sequence;
            for cpu in &mut data.vcpus {
                if cpu.state == State::WaitingForEvent {
                    // End the observable interval, not necessarily the backend wait.
                    Self::finish_wait(cpu, now);
                    cpu.state = State::Unknown;
                    cpu.since = now;
                    cpu.sequence = sequence;
                    cpu.transitions = cpu.transitions.saturating_add(1);
                }
            }
        }
        self.enabled.store(enabled, Ordering::Release);
        Ok(())
    }

    fn finish_wait(cpu: &mut VcpuObservation, now: Duration) {
        cpu.wait_exits = cpu.wait_exits.saturating_add(1);
        cpu.completed_wait = cpu
            .completed_wait
            .saturating_add(now.saturating_sub(cpu.since));
    }

    fn close_window(data: &mut VcpuObservationSnapshot, now: Duration) {
        if let Some(since) = data.all_waiting_since.take() {
            data.completed_all_waiting = data
                .completed_all_waiting
                .saturating_add(now.saturating_sub(since));
        }
        data.all_waiting = false;
    }

    pub(crate) fn record(&self, id: u8, state: State) {
        if !self.enabled.load(Ordering::Acquire) && state != State::Stopped {
            return;
        }
        let Ok(mut data) = self.data.lock() else {
            return;
        };
        if !data.enabled && state != State::Stopped {
            return;
        }
        let index = usize::from(id);
        if index >= data.vcpus.len() {
            return;
        }
        // CPU teardown is permanent; stale updates must not resurrect a slot.
        if !data.vcpus[index].online || data.vcpus[index].state == state {
            return;
        }
        let now = self.origin.elapsed();
        data.sequence = data.sequence.saturating_add(1);
        let sequence = data.sequence;
        let observing = data.enabled;
        let cpu = &mut data.vcpus[index];
        if observing && cpu.state == State::WaitingForEvent {
            Self::finish_wait(cpu, now);
        }
        if state == State::WaitingForEvent {
            cpu.wait_entries = cpu.wait_entries.saturating_add(1);
        }
        cpu.state = state;
        cpu.since = now;
        cpu.sequence = sequence;
        cpu.transitions = cpu.transitions.saturating_add(1);
        if state == State::Stopped && cpu.online {
            cpu.online = false;
            data.topology_generation = data.topology_generation.saturating_add(1);
        }
        let all_waiting = !data.vcpus.is_empty()
            && data
                .vcpus
                .iter()
                .all(|cpu| cpu.online && cpu.state == State::WaitingForEvent);
        if all_waiting && !data.all_waiting {
            data.idle_epoch = data.idle_epoch.saturating_add(1);
            data.all_waiting_since = Some(now);
            data.all_waiting = true;
        } else if !all_waiting {
            Self::close_window(&mut data, now);
        }
    }

    pub(crate) fn snapshot(&self) -> Result<VcpuObservationSnapshot, String> {
        let mut locked = self.data.lock().map_err(|_| "observation lock poisoned")?;
        // Timestamp and records share one serialization boundary. A sampler
        // descheduled after unlocking must not extend an old waiting window.
        locked.sampled_at = self.origin.elapsed();
        let mut data = locked.clone();
        drop(locked);
        data.rejection = if !data.enabled {
            Rejection::Disabled
        } else if data.vcpus.is_empty() || data.vcpus.iter().any(|cpu| !cpu.online) {
            Rejection::TopologyIncomplete
        } else if data.vcpus.iter().any(|cpu| cpu.state == State::Unknown) {
            Rejection::Unknown
        } else if !data.all_waiting {
            Rejection::NotAllWaiting
        } else {
            Rejection::WakeDeadlineUnavailable
        };
        Ok(data)
    }
}

impl VcpuObservationControl for VmmHandle {
    fn set_vcpu_observation(&self, enabled: bool) -> Result<(), String> {
        let vm = self.vmm.upgrade().ok_or("VMM has stopped")?;
        let collector = vm
            .lock()
            .map_err(|_| "VMM lock poisoned")?
            .vcpu_observation
            .clone();
        collector.enable(enabled)
    }

    fn vcpu_observation(&self) -> Result<VcpuObservationSnapshot, String> {
        let vm = self.vmm.upgrade().ok_or("VMM has stopped")?;
        let collector = vm
            .lock()
            .map_err(|_| "VMM lock poisoned")?
            .vcpu_observation
            .clone();
        collector.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_timestamp_is_captured_in_the_locked_record() {
        let c = Collector::new(1);
        c.enable(true).unwrap();
        c.record(0, State::WaitingForEvent);
        let snapshot = c.snapshot().unwrap();
        let locked = c.data.lock().unwrap();
        assert_eq!(snapshot.sampled_at, locked.sampled_at);
        assert_eq!(snapshot.sequence, locked.sequence);
        assert!(snapshot.vcpus[0].since <= snapshot.sampled_at);
        drop(locked);
        c.record(0, State::HandlingExit);
        let exit = c.snapshot().unwrap();
        assert!(snapshot.sampled_at <= exit.vcpus[0].since);
        assert!(snapshot.all_waiting);
        assert!(!exit.all_waiting);
    }

    #[test]
    fn disable_truncates_each_wait_and_stopped_does_not_extend_it() {
        let c = Collector::new(2);
        c.enable(true).unwrap();
        c.record(0, State::WaitingForEvent);
        c.record(1, State::WaitingForEvent);
        let waiting = c.snapshot().unwrap();
        c.enable(false).unwrap();
        let disabled = c.snapshot().unwrap();
        assert_eq!(disabled.rejection, Rejection::Disabled);
        assert!(!disabled.all_waiting);
        assert!(disabled.all_waiting_since.is_none());
        assert_eq!(disabled.idle_epoch, waiting.idle_epoch);
        for (before, after) in waiting.vcpus.iter().zip(&disabled.vcpus) {
            assert_eq!(after.state, State::Unknown);
            assert_eq!(after.wait_entries, 1);
            assert_eq!(after.wait_exits, 1);
            assert_eq!(after.completed_wait, after.since - before.since);
            assert_eq!(after.sequence, disabled.sequence);
        }
        assert_eq!(disabled.vcpus[0].since, disabled.vcpus[1].since);
        assert_eq!(
            disabled.completed_all_waiting,
            disabled.vcpus[0].since - waiting.all_waiting_since.unwrap()
        );
        c.enable(false).unwrap();
        c.record(0, State::HandlingExit); // Invisible while disabled.
        c.record(1, State::Executing);
        c.record(0, State::Stopped);
        c.record(0, State::Stopped); // Duplicate teardown must be idempotent.
        c.record(1, State::Stopped);
        let stopped = c.snapshot().unwrap();
        assert_eq!(
            stopped.topology_generation,
            disabled.topology_generation + 2
        );
        assert_eq!(
            stopped.completed_all_waiting,
            disabled.completed_all_waiting
        );
        for (before, after) in disabled.vcpus.iter().zip(&stopped.vcpus) {
            assert_eq!(after.state, State::Stopped);
            assert!(!after.online);
            assert_eq!(after.completed_wait, before.completed_wait);
            assert_eq!(after.wait_exits, before.wait_exits);
            assert_eq!(after.wait_entries, before.wait_entries);
        }
    }

    #[test]
    fn disabled_stopped_never_accounts_an_unobservable_wait() {
        let c = Collector::new(1);
        // Defensive check for a stale wait record: disabled teardown must only
        // register lifecycle even if an old WaitingForEvent state is present.
        {
            let mut data = c.data.lock().unwrap();
            data.vcpus[0].state = State::WaitingForEvent;
            data.vcpus[0].wait_entries = 1;
            data.vcpus[0].completed_wait = Duration::from_secs(3);
        }
        c.record(0, State::Stopped);
        let snapshot = c.snapshot().unwrap();
        assert_eq!(snapshot.vcpus[0].completed_wait, Duration::from_secs(3));
        assert_eq!(snapshot.vcpus[0].wait_exits, 0);
        assert_eq!(snapshot.vcpus[0].state, State::Stopped);
        assert_eq!(snapshot.topology_generation, 2);
    }

    #[test]
    fn duplicate_and_invalid_updates_are_bounded_and_counters_saturate() {
        let c = Collector::new(1);
        c.enable(true).unwrap();
        c.record(0, State::WaitingForEvent);
        let first = c.snapshot().unwrap();
        c.record(0, State::WaitingForEvent);
        c.record(255, State::WaitingForEvent);
        let same = c.snapshot().unwrap();
        assert_eq!(same.sequence, first.sequence);
        assert_eq!(same.idle_epoch, first.idle_epoch);
        assert_eq!(same.vcpus.len(), 1);
        {
            let mut data = c.data.lock().unwrap();
            data.sequence = u64::MAX;
            data.vcpus[0].transitions = u64::MAX;
            data.vcpus[0].wait_exits = u64::MAX;
        }
        c.record(0, State::HandlingExit);
        let saturated = c.snapshot().unwrap();
        assert_eq!(saturated.sequence, u64::MAX);
        assert_eq!(saturated.vcpus[0].transitions, u64::MAX);
        assert_eq!(saturated.vcpus[0].wait_exits, u64::MAX);
        assert!(!saturated.all_waiting);
        assert!(saturated.all_waiting_since.is_none());
    }

    #[test]
    fn stopped_while_disabled_is_not_resurrected() {
        let c = Collector::new(1);
        c.record(0, State::Stopped);
        c.enable(true).unwrap();
        c.record(0, State::WaitingForEvent);
        let s = c.snapshot().unwrap();
        assert_eq!(s.topology_generation, 2);
        assert_eq!(s.rejection, Rejection::TopologyIncomplete);
        assert_eq!(s.vcpus[0].state, State::Stopped);
        assert!(!s.all_waiting);
    }

    #[test]
    fn concurrent_sampling_is_consistent_and_bounded() {
        let c = std::sync::Arc::new(Collector::new(2));
        c.enable(true).unwrap();
        let workers: Vec<_> = (0..2)
            .map(|id| {
                let c = c.clone();
                std::thread::spawn(move || {
                    for _ in 0..2000 {
                        c.record(id, State::WaitingForEvent);
                        c.record(id, State::HandlingExit);
                    }
                })
            })
            .collect();
        let mut previous = 0;
        for _ in 0..2000 {
            let s = c.snapshot().unwrap();
            assert_eq!(s.vcpus.len(), 2);
            assert!(s.sequence >= previous);
            previous = s.sequence;
            assert_eq!(
                s.all_waiting,
                s.vcpus
                    .iter()
                    .all(|cpu| cpu.state == State::WaitingForEvent)
            );
            assert_eq!(s.all_waiting, s.all_waiting_since.is_some());
            for cpu in &s.vcpus {
                assert!(cpu.sequence <= s.sequence);
                assert!(cpu.since <= s.sampled_at);
                assert!(cpu.wait_exits <= cpu.wait_entries);
            }
        }
        for worker in workers {
            worker.join().unwrap();
        }
        let s = c.snapshot().unwrap();
        assert_eq!(s.vcpus[0].wait_entries, 2000);
        assert_eq!(s.vcpus[1].wait_exits, 2000);
    }

    #[test]
    fn dead_handle_fails_without_control_changes() {
        let handle = VmmHandle {
            vmm: std::sync::Weak::new(),
            transition: std::sync::Arc::new(Mutex::new(())),
            cold_pager_started: std::sync::Arc::new(AtomicBool::new(false)),
        };
        assert!(handle.set_vcpu_observation(true).is_err());
        assert!(handle.vcpu_observation().is_err());
        assert!(!handle.cold_pager_started.load(Ordering::Acquire));
    }

    #[test]
    fn disabled_and_mid_wait_enable_are_unknown() {
        let c = Collector::new(1);
        c.record(0, State::WaitingForEvent);
        assert_eq!(c.snapshot().unwrap().rejection, Rejection::Disabled);
        c.enable(true).unwrap();
        let s = c.snapshot().unwrap();
        assert_eq!(s.rejection, Rejection::Unknown);
        assert_eq!(s.vcpus[0].wait_entries, 0);
        c.enable(true).unwrap();
        assert_eq!(c.snapshot().unwrap().session, s.session);
    }

    #[test]
    fn smp_windows_sequences_and_control_parks() {
        let c = Collector::new(2);
        c.enable(true).unwrap();
        c.record(0, State::WaitingForEvent);
        c.record(1, State::Executing);
        assert!(!c.snapshot().unwrap().all_waiting);
        c.record(1, State::WaitingForEvent);
        let first = c.snapshot().unwrap();
        assert!(first.all_waiting);
        assert_eq!(first.rejection, Rejection::WakeDeadlineUnavailable);
        assert_eq!(first.idle_epoch, 1);
        c.record(0, State::ManualPaused);
        let parked = c.snapshot().unwrap();
        assert!(!parked.all_waiting);
        assert!(parked.sequence > first.sequence);
        assert_eq!(parked.vcpus[0].wait_exits, 1);
        c.record(0, State::WaitingForEvent);
        assert_eq!(c.snapshot().unwrap().idle_epoch, 2);
        c.record(1, State::Stopped);
        let stopped = c.snapshot().unwrap();
        assert!(!stopped.all_waiting);
        assert_eq!(stopped.topology_generation, 2);
        assert_eq!(stopped.rejection, Rejection::TopologyIncomplete);
    }

    #[test]
    fn kvm_run_unknown_is_never_idle_and_sessions_do_not_reuse_windows() {
        let c = Collector::new(1);
        c.enable(true).unwrap();
        c.record(0, State::Executing);
        c.record(0, State::Unknown);
        for _ in 0..1000 {
            let s = c.snapshot().unwrap();
            assert_eq!(s.rejection, Rejection::Unknown);
            assert!(!s.all_waiting);
            assert_eq!(s.vcpus.len(), 1);
        }
        c.record(0, State::HandlingExit);
        assert_eq!(c.snapshot().unwrap().vcpus[0].state, State::HandlingExit);
        c.record(0, State::WaitingForEvent);
        let old = c.snapshot().unwrap();
        c.enable(false).unwrap();
        c.enable(true).unwrap();
        let new = c.snapshot().unwrap();
        assert!(new.session > old.session);
        assert!(new.sequence > old.sequence);
        assert!(!new.all_waiting);
        assert_eq!(new.vcpus[0].state, State::Unknown);
    }
}
