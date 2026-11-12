use crate::{
    api::{RamReclaim, VmControl, VmmHandle},
    vmm,
};
#[cfg(target_os = "macos")]
use std::sync::Arc;

impl VmmHandle {
    /// Full-device quiescence for snapshot capture. This does not publish a
    /// snapshot or implement cold restore. Caller persists state inside action.
    /// Deadline failure leaves CPU parked: a worker still stopping cannot safely
    /// be abandoned or resumed. The caller must terminate that failed runner.
    #[cfg(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    pub(crate) fn snapshot_quiesced<T>(
        &self,
        timeout: std::time::Duration,
        action: impl FnOnce(&mut vmm::Vmm) -> Result<T, String>,
    ) -> Result<T, String> {
        self.snapshot_transaction(timeout, true, action)
    }

    #[cfg(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    pub(crate) fn snapshot_frozen<T>(
        &self,
        timeout: std::time::Duration,
        action: impl FnOnce(&mut vmm::Vmm) -> Result<T, String>,
    ) -> Result<T, String> {
        self.snapshot_transaction(timeout, false, action)
    }

    #[cfg(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    fn snapshot_transaction<T>(
        &self,
        timeout: std::time::Duration,
        resume_source: bool,
        action: impl FnOnce(&mut vmm::Vmm) -> Result<T, String>,
    ) -> Result<T, String> {
        if self.idle.status().enabled {
            return Err("guest idle M1 cannot snapshot".into());
        }
        let _transition = self
            .transition
            .lock()
            .map_err(|_| "VM transition lock poisoned")?;
        let vmm = self.vmm.upgrade().ok_or("VMM has stopped")?;
        let deadline = std::time::Instant::now()
            .checked_add(timeout)
            .ok_or("invalid snapshot timeout")?;
        {
            let locked = vmm.lock().map_err(|_| "VMM lock poisoned")?;
            if (resume_source && locked.is_paused())
                || locked.device_memory_gate().has_prepare()
                || self
                    .cold_pager_started
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(
                    "resumable snapshot requires a running VM; active RAM pagers are unsupported"
                        .into(),
                );
            }
        }
        if resume_source {
            wait_snapshot_channels(deadline, || {
                let locked = vmm.lock().map_err(|_| "VMM lock poisoned")?;
                locked.snapshot_channel_blocker()
            })?;
        }
        loop {
            let result = vmm
                .lock()
                .map_err(|_| "VMM lock poisoned")?
                .freeze_for_snapshot();
            match result {
                Ok(true) => break,
                Ok(false) if std::time::Instant::now() < deadline => {
                    // Release VMM lock so worker completion can make progress.
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                result => {
                    vmm.lock().map_err(|_| "VMM lock poisoned")?.fail_control();
                    return Err(match result {
                        Err(error) => error,
                        _ => "snapshot worker freeze timed out; terminate runner".into(),
                    });
                }
            }
        }
        let mut locked = vmm.lock().map_err(|_| "VMM lock poisoned")?;
        let result = action(&mut locked);
        if resume_source {
            if let Err(error) = locked.resume() {
                locked.fail_control();
                return Err(format!("snapshot source resume failed: {error}"));
            }
        } else {
            locked.fail_control();
        }
        result
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn ram_residency(&self) -> Result<Option<u64>, String> {
        let vmm = self.vmm.upgrade().ok_or("VMM has stopped")?;
        let locked = vmm.lock().map_err(|_| "VMM lock poisoned")?;
        Ok(locked.experimental_ram_residency())
    }

    /// Experimental optional mapping transaction. None skips a paused VM or
    /// busy devices without invoking action. Mapping errors park the VM.
    #[cfg(any(target_os = "macos", all(target_os = "linux", target_arch = "x86_64")))]
    pub(crate) fn ram_quiesced<T>(
        &self,
        action: impl FnOnce(&mut vmm::Vmm) -> Result<T, String>,
    ) -> Result<Option<T>, String> {
        let started = std::time::Instant::now();
        let _transition = self
            .transition
            .lock()
            .map_err(|_| "VM transition lock poisoned")?;
        let transition_us = started.elapsed().as_micros();
        let vmm = self.vmm.upgrade().ok_or("VMM has stopped")?;
        let pause_started = std::time::Instant::now();
        let gate = {
            let mut locked = vmm.lock().map_err(|_| "VMM lock poisoned")?;
            if locked.is_paused() {
                return Ok(None);
            }
            #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
            {
                let faults = locked.device_memory_gate().cold_faults();
                let generation = match &faults {
                    Some(faults) => match faults.snapshot() {
                        Some(generation) => Some(generation),
                        None => return Ok(None),
                    },
                    None => None,
                };
                if faults.is_some() {
                    locked
                        .pause_for_ram_maintenance()
                        .map_err(|error| error.to_string())?;
                } else {
                    locked.pause().map_err(|error| error.to_string())?;
                }
                // A fault can begin after the preflight check, while KVM is
                // entering the kernel. Never abandon outstanding pause acks.
                // Once parked, defer if a fault raced or is still resolving.
                if faults
                    .as_ref()
                    .is_some_and(|faults| faults.snapshot() != generation)
                {
                    if let Err(error) = locked.resume() {
                        locked.fail_control();
                        return Err(error.to_string());
                    }
                    return Ok(None);
                }
            }
            #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
            locked.pause().map_err(|error| error.to_string())?;
            locked.device_memory_gate()
        };
        let pause_us = pause_started.elapsed().as_micros();
        let gate_started = std::time::Instant::now();
        match gate.try_close() {
            Ok(true) => {}
            Ok(false) => {
                let mut locked = vmm.lock().map_err(|_| "VMM lock poisoned")?;
                if let Err(error) = locked.resume() {
                    locked.fail_control();
                    return Err(error.to_string());
                }
                return Ok(None);
            }
            Err(error) => {
                vmm.lock().map_err(|_| "VMM lock poisoned")?.fail_control();
                return Err(error.into());
            }
        }
        let gate_us = gate_started.elapsed().as_micros();
        let action_started = std::time::Instant::now();
        let mut locked = vmm.lock().map_err(|_| "VMM lock poisoned")?;
        let result = action(&mut locked);
        let action_us = action_started.elapsed().as_micros();
        let resume_started = std::time::Instant::now();
        let result = result.and_then(|value| {
            locked
                .resume()
                .map(|_| value)
                .map_err(|error| error.to_string())
        });
        if std::env::var_os("PVISOR_EXPERIMENTAL_MEMORY_METRICS").is_some() {
            eprintln!(
                "pvisor-cold-quiesce transition_us={transition_us} pause_us={pause_us} gate_us={gate_us} action_us={action_us} resume_us={} elapsed_us={} pid={}",
                resume_started.elapsed().as_micros(),
                started.elapsed().as_micros(),
                std::process::id()
            );
        }
        if result.is_err() {
            locked.fail_control();
        }
        result.map(Some)
    }

    /// Experimental macOS RAM fault interface. This only installs a resolver;
    /// it does not enable sampling, deduplication or change guest RAM mappings.
    /// Requires a running VM; preserves paused/offloaded VMs by rejecting them.
    #[cfg(target_os = "macos")]
    pub(crate) fn register_ram_fault_handler(
        &self,
        handler: Arc<vmm::ram::MemoryFaultHandler>,
    ) -> Result<(), String> {
        let _transition = self
            .transition
            .lock()
            .map_err(|_| "VM transition lock poisoned")?;
        let vmm = self.vmm.upgrade().ok_or("VMM has stopped")?;
        let gate = {
            let mut locked = vmm.lock().map_err(|_| "VMM lock poisoned")?;
            if locked.is_paused() {
                return Err("RAM fault registration requires a running VM".into());
            }
            locked.pause().map_err(|e| e.to_string())?;
            locked.device_memory_gate()
        };
        // Device completion may need the VMM worker: drain without the VMM lock.
        if let Err(error) = gate.close(std::time::Duration::from_secs(5)) {
            vmm.lock().map_err(|_| "VMM lock poisoned")?.fail_control();
            return Err(error.into());
        }
        let mut locked = vmm.lock().map_err(|_| "VMM lock poisoned")?;
        let result = locked
            .install_memory_fault_handler(handler)
            .and_then(|_| locked.resume());
        if result.is_err() {
            locked.fail_control();
        }
        result.map_err(|e| e.to_string())
    }

    pub(crate) fn offload_ram_with_idle(
        &self,
        idle_epoch: Option<u64>,
    ) -> Result<Option<RamReclaim>, String> {
        // In this profile automatic reclaim must carry its original idle epoch.
        // Manual offload remains unsupported; a manual pause/resume is unchanged.
        if idle_epoch.is_none() && self.idle.status().enabled {
            return Err("guest idle offload requires a claimed idle epoch".into());
        }
        let _transition = self
            .transition
            .lock()
            .map_err(|_| "VM transition lock poisoned")?;
        let reattach = self
            .shared_backing_reclaim
            .lock()
            .map_err(|_| "shared backing hook poisoned")?;
        if self
            .cold_pager_started
            .load(std::sync::atomic::Ordering::Acquire)
            && reattach.is_none()
        {
            return Err("whole-VM offload is incompatible with the experimental cold pager".into());
        }
        let vmm = self.vmm.upgrade().ok_or("VMM has stopped")?;
        let gate = {
            let mut locked = vmm.lock().map_err(|_| "VMM lock poisoned")?;
            if locked.device_memory_gate().has_prepare() {
                return Err(
                    "whole-VM offload is incompatible with experimental RAM preparation".into(),
                );
            }
            if idle_epoch.is_some() {
                if !locked.is_paused() {
                    return Err("guest idle offload requires an acknowledged pause".into());
                }
                if locked.device_memory_gate().is_idle_closed() {
                    // Never reopen a gate owned by a prior offload/maintenance.
                    return Err("guest idle offload requires an open device RAM gate".into());
                }
            } else {
                locked.pause().map_err(|error| error.to_string())?;
            }
            locked.device_memory_gate()
        };
        // Device I/O may need the VMM worker to finish; never drain under its lock.
        if let Some(epoch) = idle_epoch {
            // A device lease retained across a slow host request is a reason to
            // resume, not to spend the entire guest timer window draining it.
            if !gate.try_close()? {
                self.idle.trace_offload(epoch, "device_gate_busy", None);
                return Ok(None);
            }
        } else if let Err(error) = gate.close(std::time::Duration::from_secs(5)) {
            vmm.lock().map_err(|_| "VMM lock poisoned")?.fail_control();
            return Err(error.to_owned());
        }
        let mut locked = vmm.lock().map_err(|_| "VMM lock poisoned")?;
        if let Some(epoch) = idle_epoch {
            if !self.idle.begin_offload(epoch) {
                gate.open();
                return Ok(None);
            }
        }
        // A wake after admission is latched; never interrupt writeback or reopen
        // RAM writers until native reclaim and host publication have completed.
        let reclaim_started = std::time::Instant::now();
        if let Some(Err(error)) = reattach.as_ref().map(|reattach| reattach()) {
            locked.fail_control();
            return Err(error);
        }
        let result = locked.offload_ram();
        if let Some(epoch) = idle_epoch {
            self.idle.trace_offload(
                epoch,
                if result.is_ok() {
                    "reclaim_completed"
                } else {
                    "reclaim_failed"
                },
                Some(reclaim_started.elapsed().as_nanos()),
            );
        }
        if result.is_err() {
            locked.fail_control();
        }
        result.map(Some).map_err(|error| error.to_string())
    }

    fn control(&self, paused: bool) -> Result<(), String> {
        let _transition = self
            .transition
            .lock()
            .map_err(|_| "VM transition lock poisoned")?;
        let vmm = self.vmm.upgrade().ok_or("VMM has stopped")?;
        let mut vmm = vmm.lock().map_err(|_| "VMM lock poisoned")?;
        let result = if paused {
            vmm.pause()
        } else {
            self.idle.resumed();
            vmm.resume()
        };
        result.map_err(|error| error.to_string())
    }
}

impl VmControl for VmmHandle {
    fn is_paused(&self) -> Result<bool, String> {
        let _transition = self
            .transition
            .lock()
            .map_err(|_| "VM transition lock poisoned")?;
        let vmm = self.vmm.upgrade().ok_or("VMM has stopped")?;
        let locked = vmm.lock().map_err(|_| "VMM lock poisoned")?;
        Ok(locked.is_paused())
    }

    fn offload_ram(&self) -> Result<RamReclaim, String> {
        self.offload_ram_with_idle(None)?
            .ok_or_else(|| "unconditional offload was not admitted".into())
    }
    fn pause(&self) -> Result<(), String> {
        self.control(true)
    }

    fn resume(&self) -> Result<(), String> {
        self.control(false)
    }
}

#[cfg(test)]
mod shared_backing_tests {
    use super::*;
    use std::sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    fn stopped_handle() -> VmmHandle {
        VmmHandle {
            idle: Arc::new(crate::guest_idle::IdleGate::default()),
            vmm: Weak::new(),
            transition: Arc::new(Mutex::new(())),
            cold_pager_started: Arc::new(AtomicBool::new(true)),
            shared_backing_reclaim: Default::default(),
        }
    }

    #[test]
    fn other_cold_pagers_remain_incompatible_with_whole_vm_offload() {
        let handle = stopped_handle();
        assert!(handle.offload_ram().unwrap_err().contains("incompatible"));
    }

    #[test]
    fn shared_reclaim_never_runs_after_vm_exit() {
        let handle = stopped_handle();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        *handle.shared_backing_reclaim.lock().unwrap() = Some(Box::new(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        assert_eq!(handle.offload_ram().unwrap_err(), "VMM has stopped");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

/// Wait with CPUs/workers running and no VMM lock held across sleeps. A timeout
/// here has not frozen anything; the capture action must never run afterwards.
#[cfg(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
))]
fn wait_snapshot_channels(
    deadline: std::time::Instant,
    mut probe: impl FnMut() -> Result<Option<String>, String>,
) -> Result<(), String> {
    let started = std::time::Instant::now();
    let mut previous = None;
    loop {
        let blocker = probe()?;
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "snapshot channel drain timed out before freeze: {}",
                blocker.as_deref().unwrap_or("deadline exhausted")
            ));
        }
        if blocker != previous || blocker.is_none() {
            eprintln!(
                "pvisor-snapshot-channel elapsed_ms={} blocker={:?}",
                started.elapsed().as_millis(),
                blocker
            );
            previous = blocker.clone();
        }
        if blocker.is_none() {
            return Ok(());
        }
        std::thread::sleep(
            std::time::Duration::from_millis(10)
                .min(deadline.saturating_duration_since(std::time::Instant::now())),
        );
    }
}

#[cfg(all(
    test,
    any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64")
    )
))]
mod snapshot_channel_tests {
    use super::wait_snapshot_channels;
    use std::time::{Duration, Instant};
    #[test]
    fn snapshot_channel_drains_before_capture() {
        let mut polls = 0;
        wait_snapshot_channels(Instant::now() + Duration::from_secs(1), || {
            polls += 1;
            Ok(if polls < 3 {
                Some("closed=1".into())
            } else {
                None
            })
        })
        .unwrap();
        assert_eq!(polls, 3);
    }
    #[test]
    fn snapshot_channel_timeout_reports_blocker_and_does_not_run_action() {
        let mut called = false;
        let result = wait_snapshot_channels(Instant::now() + Duration::from_millis(20), || {
            Ok(Some("responses=2".into()))
        })
        .map(|()| called = true);
        assert!(result.unwrap_err().contains("responses=2"));
        assert!(!called);
    }
    #[test]
    fn snapshot_channel_fault_and_exhausted_budget_are_not_retried() {
        assert_eq!(
            wait_snapshot_channels(Instant::now() + Duration::from_secs(1), || Err(
                "poisoned".into()
            ))
            .unwrap_err(),
            "poisoned"
        );
        assert!(wait_snapshot_channels(Instant::now(), || Ok(None)).is_err());
    }
}
