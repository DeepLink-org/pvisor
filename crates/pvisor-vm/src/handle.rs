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
    #[cfg(target_os = "macos")]
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

    fn control(&self, paused: bool) -> Result<(), String> {
        let _transition = self
            .transition
            .lock()
            .map_err(|_| "VM transition lock poisoned")?;
        let vmm = self.vmm.upgrade().ok_or("VMM has stopped")?;
        let mut vmm = vmm.lock().map_err(|_| "VMM lock poisoned")?;
        let result = if paused { vmm.pause() } else { vmm.resume() };
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
        let _transition = self
            .transition
            .lock()
            .map_err(|_| "VM transition lock poisoned")?;
        if self
            .cold_pager_started
            .load(std::sync::atomic::Ordering::Acquire)
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
            locked.pause().map_err(|error| error.to_string())?;
            locked.device_memory_gate()
        };
        // Device I/O may need the VMM worker to finish; never drain under its lock.
        if let Err(error) = gate.close(std::time::Duration::from_secs(5)) {
            vmm.lock().map_err(|_| "VMM lock poisoned")?.fail_control();
            return Err(error.to_owned());
        }
        let mut locked = vmm.lock().map_err(|_| "VMM lock poisoned")?;
        let result = locked.offload_ram();
        if result.is_err() {
            locked.fail_control();
        }
        result.map_err(|error| error.to_string())
    }
    fn pause(&self) -> Result<(), String> {
        self.control(true)
    }

    fn resume(&self) -> Result<(), String> {
        self.control(false)
    }
}
