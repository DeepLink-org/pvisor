//! Parent-side resident VM control. One request owns the transport until its
//! completion acknowledgement; status and cancellation never wait for its lock.
use super::vm_memory::VmMemory;
use anyhow::{Context, bail};
use persisting_control::overlay::{
    VmMemoryStatus, VmOffloadReport, VmOffloadState, VmRuntimeState, VmRuntimeStatus,
};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};
use tokio::sync::watch;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum VmCommand {
    Pause,
    Resume,
    Status,
}

#[derive(Serialize)]
struct Request {
    request_id: u64,
    command: VmCommand,
}

#[derive(Deserialize)]
struct Response {
    request_id: u64,
    ok: bool,
    state: VmRuntimeState,
    error: Option<String>,
}

/// Accounts all wall time except intervals acknowledged as fully paused.
struct ActiveClock {
    started: Instant,
    paused_since: Option<Instant>,
    paused_total: Duration,
}

impl ActiveClock {
    fn new(now: Instant) -> Self {
        Self {
            started: now,
            paused_since: None,
            paused_total: Duration::ZERO,
        }
    }

    fn transition(&mut self, paused: bool, now: Instant) {
        match (self.paused_since, paused) {
            (None, true) => self.paused_since = Some(now),
            (Some(since), false) => {
                self.paused_total += now.saturating_duration_since(since);
                self.paused_since = None;
            }
            _ => {}
        }
    }

    fn elapsed(&self, now: Instant) -> Duration {
        self.paused_since
            .unwrap_or(now)
            .saturating_duration_since(self.started)
            .saturating_sub(self.paused_total)
    }
}

pub(crate) struct VmControl {
    transport: Mutex<Option<BufReader<UnixStream>>>,
    request_id: AtomicU64,
    status: watch::Sender<VmRuntimeStatus>,
    clock: Mutex<ActiveClock>,
    memory: Mutex<Option<Arc<VmMemory>>>,
    reclaim_task: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl VmControl {
    pub(crate) fn new(supported: bool) -> Self {
        let (status, _) = watch::channel(VmRuntimeStatus {
            supported,
            state: if supported {
                VmRuntimeState::Starting
            } else {
                VmRuntimeState::Unsupported
            },
            error: None,
            memory: None,
        });
        Self {
            transport: Mutex::new(None),
            request_id: AtomicU64::new(1),
            status,
            clock: Mutex::new(ActiveClock::new(Instant::now())),
            memory: Mutex::new(None),
            reclaim_task: Mutex::new(None),
        }
    }

    pub(crate) fn status(&self) -> VmRuntimeStatus {
        let mut status = self.status.borrow().clone();
        if let Some(memory) = self.memory.lock().unwrap().as_ref()
            && let Some(detail) = status.memory.as_mut()
        {
            match memory.sample() {
                Ok(sample) => {
                    detail.sample = Some(sample);
                    detail.sample_error = None;
                }
                Err(error) => {
                    detail.sample = None;
                    detail.sample_error = Some(error.to_string());
                }
            }
        }
        status
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn set_memory(&self, memory: Arc<VmMemory>) -> anyhow::Result<()> {
        let sample = memory.sample()?;
        let detail = VmMemoryStatus {
            cgroup: memory.path().to_path_buf(),
            sample: Some(sample),
            sample_error: None,
            last_offload: None,
        };
        *self.memory.lock().unwrap() = Some(memory);
        self.status
            .send_modify(|status| status.memory = Some(detail));
        Ok(())
    }

    /// Called on a blocking worker AFTER stop + child.wait(). Killing the VM
    /// does not wait for reclaim; orderly supervisor exit must join it so its
    /// cgroup owner is not abandoned when the whole process exits.
    pub(crate) fn release_memory(&self) {
        let worker = {
            // Serialize with an offload that was accepted just before stop.
            let _transport = self.transport.lock().unwrap();
            self.reclaim_task.lock().unwrap().take()
        };
        if let Some(worker) = worker
            && worker.join().is_err()
        {
            tracing::warn!("memory reclaim worker panicked during teardown");
        }
        self.memory.lock().unwrap().take();
    }

    /// Accept a bounded background reclaim job. The execution state remains
    /// Paused; status queries and killing the runner do not wait for reclaim.
    pub(crate) fn offload(self: &Arc<Self>, bytes: u64) -> anyhow::Result<VmRuntimeStatus> {
        anyhow::ensure!(bytes > 0, "offload bytes must be positive");
        let _transport = self
            .transport
            .try_lock()
            .map_err(|_| anyhow::anyhow!("VM control request in progress"))?;
        let status = self.status();
        anyhow::ensure!(
            status.state == VmRuntimeState::Paused,
            "offload requires a confirmed paused VM; run pause first"
        );
        anyhow::ensure!(!reclaiming(&status), "memory reclaim already in progress");
        if let Some(previous) = self.reclaim_task.lock().unwrap().take() {
            // The completion report was published, so only the worker tail
            // remains. Retain structured ownership even across many cycles.
            previous
                .join()
                .map_err(|_| anyhow::anyhow!("previous reclaim worker panicked"))?;
        }
        let memory = self.memory.lock().unwrap().clone()
            .context("offload unavailable: configure vm.cgroup_parent or --vm-cgroup-parent before starting the VM")?;
        memory.ensure_swap_enabled()?;
        let before = memory.sample()?;
        anyhow::ensure!(
            bytes <= before.current_bytes,
            "requested offload exceeds current cgroup resident memory"
        );
        let report = VmOffloadReport {
            operation_id: self.request_id.fetch_add(1, Ordering::Relaxed),
            state: VmOffloadState::Reclaiming,
            requested_bytes: bytes,
            before,
            after: None,
            elapsed_ms: 0,
            error: None,
        };
        self.status.send_modify(|status| {
            status.memory.as_mut().unwrap().last_offload = Some(report.clone());
        });
        let control = self.clone();
        let failure = report.clone();
        match std::thread::Builder::new()
            .name("pvisor-reclaim".into())
            .spawn(move || control.run_offload(memory, report))
        {
            Ok(worker) => *self.reclaim_task.lock().unwrap() = Some(worker),
            Err(error) => {
                let mut report = failure;
                report.state = VmOffloadState::Failed;
                report.error = Some(error.to_string());
                self.finish_offload(report);
                return Err(error.into());
            }
        }
        Ok(self.status())
    }

    fn run_offload(&self, memory: Arc<VmMemory>, mut report: VmOffloadReport) {
        let started = Instant::now();
        let mut idle_rounds = 0;
        let result = (|| -> anyhow::Result<()> {
            // The deadline is checked BETWEEN syscalls. A blocked kernel write
            // is not cancellable by abandoning a future or timing out a CLI.
            loop {
                let after = memory.sample()?;
                let remaining = remaining_reclaim(&report, &after);
                report.after = Some(after);
                if self.status.borrow().state == VmRuntimeState::Stopped {
                    report.state = VmOffloadState::Cancelled;
                    return Ok(());
                }
                if remaining == 0 {
                    report.state = VmOffloadState::Completed;
                    return Ok(());
                }
                if started.elapsed() >= Duration::from_secs(30) || idle_rounds >= 3 {
                    report.state = VmOffloadState::Partial;
                    return Ok(());
                }
                let previous = report.after.as_ref().unwrap().current_bytes;
                match memory.reclaim(remaining.min(64 * 1024 * 1024)) {
                    Ok(()) => {}
                    Err(error) if error.raw_os_error() == Some(libc::EAGAIN) => {}
                    Err(error) => return Err(error.into()),
                }
                let after = memory.sample()?;
                if after.current_bytes >= previous {
                    idle_rounds += 1;
                } else {
                    idle_rounds = 0;
                }
                report.after = Some(after);
            }
        })();
        if let Err(error) = result {
            report.state = if self.status.borrow().state == VmRuntimeState::Stopped {
                VmOffloadState::Cancelled
            } else {
                VmOffloadState::Failed
            };
            report.error = Some(format!("{error:#}"));
        }
        report.elapsed_ms = started.elapsed().as_millis() as u64;
        self.finish_offload(report);
    }

    fn finish_offload(&self, report: VmOffloadReport) {
        self.status.send_modify(|status| {
            if let Some(memory) = status.memory.as_mut() {
                memory.sample = report.after.clone();
                memory.last_offload = Some(report);
            }
        });
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<VmRuntimeStatus> {
        self.status.subscribe()
    }

    pub(crate) fn attach(&self, stream: UnixStream) -> anyhow::Result<()> {
        stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
        stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
        let mut transport = self
            .transport
            .lock()
            .map_err(|_| anyhow::anyhow!("VM control lock poisoned"))?;
        anyhow::ensure!(transport.is_none(), "VM control transport already attached");
        *transport = Some(BufReader::new(stream));
        *self.clock.lock().unwrap() = ActiveClock::new(Instant::now());
        Ok(())
    }

    fn publish(&self, state: VmRuntimeState, error: Option<String>) {
        // Stop wins a race with an in-flight acknowledgement during teardown.
        self.status.send_modify(|current| {
            if current.state != VmRuntimeState::Stopped {
                self.clock
                    .lock()
                    .unwrap()
                    .transition(state == VmRuntimeState::Paused, Instant::now());
                current.state = state;
                current.error = error;
            }
        });
    }

    pub(crate) fn stop(&self) {
        self.publish(VmRuntimeState::Stopped, None);
    }

    pub(crate) fn request(&self, command: VmCommand) -> anyhow::Result<VmRuntimeStatus> {
        let status = self.status();
        anyhow::ensure!(
            status.supported,
            "pause/resume is supported only by the Linux libkrun executor"
        );
        anyhow::ensure!(status.state != VmRuntimeState::Stopped, "VM has stopped");
        anyhow::ensure!(
            status.state != VmRuntimeState::Faulted || matches!(command, VmCommand::Status),
            "VM control is faulted; terminate the Job"
        );
        // A concurrent request is explicitly rejected instead of blocking the
        // run control listener or queuing an unbounded number of operations.
        let mut transport = match self.transport.try_lock() {
            Ok(transport) => transport,
            Err(TryLockError::WouldBlock) => {
                bail!("VM control request in progress; retry after it completes")
            }
            Err(TryLockError::Poisoned(_)) => bail!("VM control lock poisoned"),
        };
        // Another request may have completed between the first snapshot and
        // acquiring the transport. Phase publication must use the latest ACK.
        let status = self.status();
        anyhow::ensure!(status.state != VmRuntimeState::Stopped, "VM has stopped");
        anyhow::ensure!(
            status.state != VmRuntimeState::Faulted || matches!(command, VmCommand::Status),
            "VM control is faulted; terminate the Job"
        );
        let reader = transport
            .as_mut()
            .context("VM runner control is not ready")?;
        anyhow::ensure!(
            !reclaiming(&status) || matches!(command, VmCommand::Status),
            "memory reclaim in progress; query status and retry after it finishes"
        );
        match command {
            VmCommand::Pause if status.state != VmRuntimeState::Paused => {
                self.publish(VmRuntimeState::Pausing, None)
            }
            VmCommand::Resume if status.state == VmRuntimeState::Paused => {
                self.publish(VmRuntimeState::Resuming, None)
            }
            _ => {}
        }
        let request_id = self.request_id.fetch_add(1, Ordering::Relaxed);
        let result = (|| -> anyhow::Result<Response> {
            serde_json::to_writer(
                reader.get_mut(),
                &Request {
                    request_id,
                    command,
                },
            )?;
            reader.get_mut().write_all(b"\n")?;
            let mut line = Vec::new();
            reader
                .take(MAX_RESPONSE_BYTES + 1)
                .read_until(b'\n', &mut line)?;
            anyhow::ensure!(
                line.last() == Some(&b'\n') && line.len() as u64 <= MAX_RESPONSE_BYTES,
                "VM control returned a truncated, oversized or empty response"
            );
            let response: Response = serde_json::from_slice(&line)?;
            anyhow::ensure!(
                response.request_id == request_id,
                "VM control response request_id mismatch"
            );
            anyhow::ensure!(
                matches!(
                    response.state,
                    VmRuntimeState::Running | VmRuntimeState::Paused | VmRuntimeState::Faulted
                ),
                "VM control returned an invalid lifecycle state"
            );
            if response.ok {
                let expected = match command {
                    VmCommand::Pause => VmRuntimeState::Paused,
                    VmCommand::Resume => VmRuntimeState::Running,
                    VmCommand::Status => response.state,
                };
                anyhow::ensure!(
                    response.state == expected
                        && (response.state != VmRuntimeState::Faulted
                            || matches!(command, VmCommand::Status)),
                    "VM control acknowledgement contradicts the requested operation"
                );
            }
            Ok(response)
        })();
        match result {
            Ok(response) => {
                self.publish(response.state, response.error.clone());
                if !response.ok {
                    bail!(
                        "{}",
                        response
                            .error
                            .unwrap_or_else(|| "VM control operation failed".into())
                    );
                }
                let status = self.status();
                anyhow::ensure!(
                    status.state != VmRuntimeState::Stopped,
                    "VM stopped while processing control request"
                );
                Ok(status)
            }
            Err(error) => {
                // Never reuse a stream after a timeout: a delayed ACK must not
                // be mistaken for the next request's completion.
                if let Some(reader) = transport.take() {
                    let _ = reader.into_inner().shutdown(std::net::Shutdown::Both);
                }
                self.publish(VmRuntimeState::Faulted, Some(error.to_string()));
                Err(error.context("VM control failed; state is uncertain, terminate the Job"))
            }
        }
    }

    pub(crate) async fn wait_active_timeout(&self, limit: Duration) {
        let mut changes = self.subscribe();
        loop {
            let paused = changes.borrow_and_update().state == VmRuntimeState::Paused;
            let elapsed = self.clock.lock().unwrap().elapsed(Instant::now());
            let Some(remaining) = limit.checked_sub(elapsed) else {
                return;
            };
            if remaining.is_zero() {
                return;
            }
            if paused {
                if changes.changed().await.is_err() {
                    return;
                }
            } else {
                tokio::select! {
                    _ = tokio::time::sleep(remaining) => {},
                    result = changes.changed() => if result.is_err() { return; },
                }
            }
        }
    }
}

fn reclaiming(status: &VmRuntimeStatus) -> bool {
    status
        .memory
        .as_ref()
        .and_then(|m| m.last_offload.as_ref())
        .is_some_and(|r| r.state == VmOffloadState::Reclaiming)
}

fn remaining_reclaim(
    report: &VmOffloadReport,
    after: &persisting_control::overlay::VmMemorySample,
) -> u64 {
    report.requested_bytes.saturating_sub(
        report
            .before
            .current_bytes
            .saturating_sub(after.current_bytes),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teardown_joins_reclaim_after_stop_without_blocking_status() {
        let control = Arc::new(VmControl::new(true));
        let (release, waiting) = std::sync::mpsc::channel();
        *control.reclaim_task.lock().unwrap() =
            Some(std::thread::spawn(move || waiting.recv().unwrap()));
        control.stop();
        let other = control.clone();
        let (done, completed) = std::sync::mpsc::channel();
        let cleanup = std::thread::spawn(move || {
            other.release_memory();
            done.send(()).unwrap();
        });
        assert_eq!(control.status().state, VmRuntimeState::Stopped);
        assert!(matches!(
            completed.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        release.send(()).unwrap();
        completed.recv_timeout(Duration::from_secs(2)).unwrap();
        cleanup.join().unwrap();
        assert!(control.reclaim_task.lock().unwrap().is_none());
    }

    #[test]
    fn offload_requires_pause_and_a_dedicated_memory_cgroup() {
        let control = Arc::new(VmControl::new(true));
        control.publish(VmRuntimeState::Running, None);
        assert!(
            control
                .offload(4096)
                .unwrap_err()
                .to_string()
                .contains("confirmed paused")
        );
        control.publish(VmRuntimeState::Paused, None);
        assert!(
            control
                .offload(4096)
                .unwrap_err()
                .to_string()
                .contains("cgroup_parent")
        );
        assert_eq!(control.status().state, VmRuntimeState::Paused);
        assert!(control.offload(0).is_err());
    }

    #[test]
    fn resume_during_reclaim_is_rejected_without_touching_runner_transport() {
        let control = Arc::new(VmControl::new(true));
        let (client, mut server) = UnixStream::pair().unwrap();
        control.attach(client).unwrap();
        control.publish(VmRuntimeState::Paused, None);
        let sample = persisting_control::overlay::VmMemorySample {
            current_bytes: 8192,
            swap_bytes: 0,
            anon_bytes: 8192,
            file_bytes: 0,
        };
        control.status.send_modify(|status| {
            status.memory = Some(VmMemoryStatus {
                cgroup: "/unused-test-path".into(),
                sample: Some(sample.clone()),
                sample_error: None,
                last_offload: Some(VmOffloadReport {
                    operation_id: 1,
                    state: VmOffloadState::Reclaiming,
                    requested_bytes: 4096,
                    before: sample,
                    after: None,
                    elapsed_ms: 0,
                    error: None,
                }),
            })
        });
        assert!(
            control
                .request(VmCommand::Resume)
                .unwrap_err()
                .to_string()
                .contains("reclaim in progress")
        );
        assert!(
            control
                .offload(4096)
                .unwrap_err()
                .to_string()
                .contains("already in progress")
        );
        assert_eq!(control.status().state, VmRuntimeState::Paused);
        server.set_nonblocking(true).unwrap();
        assert_eq!(
            server.read(&mut [0]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        // Finishing reclaim must never resurrect a stopped VM.
        control.stop();
        let mut report = control.status().memory.unwrap().last_offload.unwrap();
        report.state = VmOffloadState::Cancelled;
        control.finish_offload(report);
        assert_eq!(control.status().state, VmRuntimeState::Stopped);
    }

    #[test]
    fn clock_excludes_only_confirmed_paused_intervals() {
        let now = Instant::now();
        let mut clock = ActiveClock::new(now);
        clock.transition(true, now + Duration::from_secs(2));
        clock.transition(true, now + Duration::from_secs(5));
        assert_eq!(
            clock.elapsed(now + Duration::from_secs(20)),
            Duration::from_secs(2)
        );
        clock.transition(false, now + Duration::from_secs(22));
        assert_eq!(
            clock.elapsed(now + Duration::from_secs(25)),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn completion_ack_controls_state_and_duplicate_pause_is_idempotent() {
        let (client, server) = UnixStream::pair().unwrap();
        let control = VmControl::new(true);
        control.attach(client).unwrap();
        let thread = std::thread::spawn(move || {
            let mut server = BufReader::new(server);
            for state in ["paused", "paused", "running"] {
                let mut line = String::new();
                server.read_line(&mut line).unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                writeln!(server.get_mut(), "{}", serde_json::json!({
                    "request_id": request["request_id"], "ok": true, "state": state, "error": null
                })).unwrap();
            }
        });
        assert_eq!(
            control.request(VmCommand::Pause).unwrap().state,
            VmRuntimeState::Paused
        );
        assert_eq!(
            control.request(VmCommand::Pause).unwrap().state,
            VmRuntimeState::Paused
        );
        assert_eq!(
            control.request(VmCommand::Resume).unwrap().state,
            VmRuntimeState::Running
        );
        thread.join().unwrap();
    }

    #[test]
    fn disconnected_or_mismatched_response_never_reports_paused() {
        for mismatch in [false, true] {
            let (client, server) = UnixStream::pair().unwrap();
            let control = VmControl::new(true);
            control.attach(client).unwrap();
            let thread = std::thread::spawn(move || {
                let mut server = BufReader::new(server);
                let mut line = String::new();
                server.read_line(&mut line).unwrap();
                if mismatch {
                    writeln!(
                        server.get_mut(),
                        "{{\"request_id\":999,\"ok\":true,\"state\":\"paused\"}}"
                    )
                    .unwrap();
                }
            });
            assert!(control.request(VmCommand::Pause).is_err());
            assert_eq!(control.status().state, VmRuntimeState::Faulted);
            thread.join().unwrap();
        }
    }

    #[test]
    fn timeout_faults_and_discards_the_transport_before_a_late_ack() {
        use std::sync::mpsc;
        let (client, server) = UnixStream::pair().unwrap();
        let control = VmControl::new(true);
        control.attach(client).unwrap();
        control
            .transport
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .get_mut()
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let (release_tx, release_rx) = mpsc::channel();
        let peer = std::thread::spawn(move || {
            let mut server = BufReader::new(server);
            let mut line = String::new();
            server.read_line(&mut line).unwrap();
            release_rx.recv().unwrap();
            // A delayed successful response cannot repair an uncertain state.
            let _ = writeln!(
                server.get_mut(),
                "{{\"request_id\":1,\"ok\":true,\"state\":\"paused\"}}"
            );
        });
        let started = Instant::now();
        assert!(control.request(VmCommand::Pause).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(control.status().state, VmRuntimeState::Faulted);
        assert!(control.transport.lock().unwrap().is_none());
        release_tx.send(()).unwrap();
        peer.join().unwrap();
        assert!(control.request(VmCommand::Resume).is_err());
        assert_eq!(control.status().state, VmRuntimeState::Faulted);
    }

    #[test]
    fn status_and_stop_remain_available_during_a_pending_pause() {
        use std::sync::{Arc, mpsc};
        let (client, server) = UnixStream::pair().unwrap();
        let control = Arc::new(VmControl::new(true));
        control.attach(client).unwrap();
        let (received_tx, received_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let peer = std::thread::spawn(move || {
            let mut server = BufReader::new(server);
            let mut line = String::new();
            server.read_line(&mut line).unwrap();
            received_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            writeln!(
                server.get_mut(),
                "{}",
                serde_json::json!({
                    "request_id": request["request_id"], "ok": true, "state": "paused"
                })
            )
            .unwrap();
        });
        let requester = {
            let control = control.clone();
            std::thread::spawn(move || control.request(VmCommand::Pause))
        };
        received_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(control.status().state, VmRuntimeState::Pausing);
        assert!(
            control
                .request(VmCommand::Resume)
                .unwrap_err()
                .to_string()
                .contains("in progress")
        );
        control.stop();
        release_tx.send(()).unwrap();
        assert!(requester.join().unwrap().is_err());
        assert_eq!(control.status().state, VmRuntimeState::Stopped);
        peer.join().unwrap();
    }

    #[test]
    fn faulted_status_is_a_valid_observation_without_recovering_the_vm() {
        let (client, server) = UnixStream::pair().unwrap();
        let control = VmControl::new(true);
        control.attach(client).unwrap();
        let peer = std::thread::spawn(move || {
            let mut server = BufReader::new(server);
            for ok in [false, true] {
                let mut line = String::new();
                server.read_line(&mut line).unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                writeln!(
                    server.get_mut(),
                    "{}",
                    serde_json::json!({
                        "request_id": request["request_id"], "ok": ok, "state": "faulted",
                        "error": "device operation timed out"
                    })
                )
                .unwrap();
            }
        });
        assert!(control.request(VmCommand::Pause).is_err());
        let observed = control.request(VmCommand::Status).unwrap();
        assert_eq!(observed.state, VmRuntimeState::Faulted);
        assert_eq!(
            observed.error.as_deref(),
            Some("device operation timed out")
        );
        assert!(control.request(VmCommand::Resume).is_err());
        peer.join().unwrap();
    }

    #[test]
    fn unsupported_backend_does_not_send_a_request() {
        let control = VmControl::new(false);
        assert!(
            control
                .request(VmCommand::Pause)
                .unwrap_err()
                .to_string()
                .contains("Linux libkrun")
        );
        assert_eq!(control.status().state, VmRuntimeState::Unsupported);
    }
}
