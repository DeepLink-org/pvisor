//! Private, independent parent/runner idle channel. No credentials or guest RAM.
use anyhow::Context;
use pvisor_vm::api::{GuestIdleControl, GuestIdleStatus, VmmHandle};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IdlePause {
    pub idle_pause_epoch: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IdleOffload {
    pub idle_offload_epoch: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct IdleResume {
    pub idle_resume_epoch: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
enum Request {
    Arm,
    Heartbeat,
    Retire { epoch: u64 },
}
#[derive(Clone)]
pub(crate) struct IdleClient {
    writer: Arc<Mutex<Option<tokio::net::unix::OwnedWriteHalf>>>,
    status: watch::Sender<Option<GuestIdleStatus>>,
    #[cfg(feature = "gateway")]
    lease: Arc<Mutex<()>>,
    failed: CancellationToken,
}
impl Default for IdleClient {
    fn default() -> Self {
        Self {
            writer: Arc::new(Mutex::new(None)),
            status: watch::channel(None).0,
            #[cfg(feature = "gateway")]
            lease: Arc::new(Mutex::new(())),
            failed: CancellationToken::new(),
        }
    }
}
impl IdleClient {
    #[cfg(feature = "gateway")]
    pub(crate) fn pending_host_reads(&self) -> u64 {
        self.status
            .borrow()
            .as_ref()
            .map_or(0, |s| s.pending_host_reads)
    }
    pub(super) async fn attach(
        &self,
        stream: tokio::net::UnixStream,
        cancel: CancellationToken,
        events: crate::runtime::event::RunEventPublisher,
    ) {
        let (read, write) = stream.into_split();
        *self.writer.lock().await = Some(write);
        let mut observations = self.status.subscribe();
        let observation_cancel = cancel.clone();
        let observation_events = events.clone();
        // Diagnostics may coalesce; journal latency must never block wake delivery.
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = observation_cancel.cancelled() => break,
                    changed = observations.changed() => if changed.is_err() { break; },
                }
                let value = observations.borrow_and_update().clone();
                if let Some(value) = value {
                    let _ = observation_events
                        .publish("vm.guest_idle", "runtime", serde_json::json!(value))
                        .await;
                } else {
                    break;
                }
            }
        });
        // Health traffic is independent of guest timers and lifecycle controls.
        {
            let client = self.clone();
            let stop = cancel.clone();
            let events = events.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = stop.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_millis(500)) => {},
                    }
                    if let Err(error) = client.send(&Request::Heartbeat).await {
                        stop.cancel();
                        host_failure(&events, "heartbeat_write", &error);
                        break;
                    }
                }
            });
        }
        let failed = self.failed.clone();
        let status = self.status.clone();
        tokio::spawn(async move {
            // Bounded length framing: each line is a small VM-owned DTO.
            let mut reader = BufReader::new(read);
            let result: anyhow::Result<()> = async {
                loop {
                    let mut bytes = Vec::new();
                    use tokio::io::AsyncReadExt;
                    let mut limited = (&mut reader).take(4097);
                    let n = tokio::select! {
                        _ = cancel.cancelled() => return Ok(()),
                        _ = failed.cancelled() => anyhow::bail!("idle lease cleanup failed"),
                        n = tokio::time::timeout(Duration::from_secs(3), limited.read_until(b'\n', &mut bytes)) => n.context("status read timed out (3s)")?.context("status read I/O")?,
                    };
                    if n == 0
                        && status
                            .borrow()
                            .as_ref()
                            .is_none_or(|s| !s.claimed && s.remaining_ns == 0)
                    {
                        return Ok(());
                    }
                    anyhow::ensure!(
                        n > 0 && n <= 4096 && bytes.last() == Some(&b'\n'),
                        "idle channel disconnected or oversized"
                    );
                    let next: GuestIdleStatus = serde_json::from_slice(&bytes).context("decode idle status")?;
                    if status.borrow().as_ref().is_none_or(|s| s.sequence != next.sequence) {
                        status.send_replace(Some(next));
                    }
                }
            }
            .await;
            status.send_replace(None);
            if let Err(error) = result {
                cancel.cancel();
                host_failure(&events, "status_read", &error);
            }
        });
    }
    async fn send(&self, request: &Request) -> anyhow::Result<()> {
        let mut writer = self.writer.lock().await;
        let writer = writer
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("idle channel not attached"))?;
        let mut bytes = serde_json::to_vec(request)?;
        bytes.push(b'\n');
        tokio::time::timeout(Duration::from_secs(1), writer.write_all(&bytes))
            .await
            .context("idle request write timed out (1s)")?
            .context("idle request write I/O")?;
        Ok(())
    }
    #[cfg(feature = "gateway")]
    pub(crate) async fn settled(&self) -> anyhow::Result<()> {
        let _guard = self.lease.lock().await;
        anyhow::ensure!(!self.failed.is_cancelled(), "idle retirement failed");
        Ok(())
    }
    #[cfg(feature = "gateway")]
    pub(crate) async fn resume_prepare<F>(
        &self,
        wake: CancellationToken,
        resume: F,
    ) -> anyhow::Result<tokio::sync::oneshot::Receiver<Option<u64>>>
    where
        F: std::future::Future<Output = anyhow::Result<u64>>,
    {
        let lease = self.lease.clone().lock_owned().await;
        anyhow::ensure!(!self.failed.is_cancelled(), "idle retirement failed");
        let states = self.status.subscribe();
        // The listener exists before native resume can release the vCPU. ACK
        // supplies the epoch even when watch notifications have coalesced.
        let expected = resume.await?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let client = self.clone();
        tokio::spawn(async move {
            let _lease = lease;
            client.monitor_epoch(states, expected, wake, tx).await;
        });
        Ok(rx)
    }
    #[cfg(feature = "gateway")]
    pub(crate) async fn prepare(&self, wake: CancellationToken) -> anyhow::Result<Option<u64>> {
        if self.writer.lock().await.is_none() {
            return Ok(None);
        }
        let client = self.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _lease = tokio::select! {
                guard = client.lease.lock() => guard,
                _ = wake.cancelled() => { let _ = tx.send(None); return; }
            };
            let states = client.status.subscribe();
            if wake.is_cancelled() || states.borrow().as_ref().is_some_and(|s| s.claimed) {
                let _ = tx.send(None);
                return;
            }
            let previous = states.borrow().as_ref().map_or(0, |s| s.epoch);
            let Some(expected) = previous.checked_add(1) else {
                client.failed.cancel();
                let _ = tx.send(None);
                return;
            };
            if let Err(error) = client.send(&Request::Arm).await {
                tracing::error!(error = %error, "idle arm request failed");
                client.failed.cancel();
                wake.cancel();
                let _ = tx.send(None);
                return;
            }
            client.monitor_epoch(states, expected, wake, tx).await;
        });
        Ok(rx.await?)
    }
    #[cfg(feature = "gateway")]
    async fn monitor_epoch(
        &self,
        mut states: watch::Receiver<Option<GuestIdleStatus>>,
        expected: u64,
        wake: CancellationToken,
        tx: tokio::sync::oneshot::Sender<Option<u64>>,
    ) {
        let client = self;
        let mut tx = Some(tx);
        let result: anyhow::Result<()> = async {
                let timeout = tokio::time::sleep(Duration::from_millis(1200));
                tokio::pin!(timeout);
                loop {
                    let state = states.borrow_and_update().clone();
                    if let Some(state) = state
                        && state.epoch == expected
                    {
                        if wake.is_cancelled()
                            || !state.enabled
                            || state.remaining_ns == 0
                            || state.wake_reason.is_some()
                        {
                            break;
                        }
                        if state.offered
                            && let Some(tx) = tx.take()
                            && tx.send(Some(state.epoch)).is_err()
                        {
                            break;
                        }
                    }
                    tokio::select! {
                        _ = wake.cancelled() => break,
                        _ = &mut timeout, if tx.is_some() => anyhow::bail!("idle grant acknowledgement timeout"),
                        result = states.changed() => { result?; },
                    }
                }
                wake.cancel();
                // Requests share one ordered stream. Even cancellation before
                // Arm ACK retires exactly that generation, never a newer lease.
                client.send(&Request::Retire { epoch: expected }).await?;
                tokio::time::timeout(Duration::from_secs(1), async {
                    loop {
                        let retired = states.borrow_and_update().as_ref().is_some_and(|s| {
                            s.epoch == expected && s.remaining_ns == 0 && !s.offered
                        });
                        if retired {
                            return Ok::<_, anyhow::Error>(());
                        }
                        states.changed().await?;
                    }
                })
                .await??;
                Ok(())

        }.await;
        wake.cancel();
        if let Err(error) = result {
            tracing::error!(epoch = expected, error = %error, "idle lease retirement failed");
            self.failed.cancel();
        }
        if let Some(tx) = tx {
            let _ = tx.send(None);
        }
    }
}

// Diagnostics never publish synchronously on the heartbeat/status read task.
fn host_failure(
    events: &crate::runtime::event::RunEventPublisher,
    phase: &str,
    error: &anyhow::Error,
) {
    let row = failure_row("host", phase, error, serde_json::Value::Null);
    let events = events.clone();
    tokio::spawn(async move {
        let stderr = row.clone();
        tokio::task::spawn_blocking(move || eprintln!("pvisor idle channel failure: {stderr}"));
        let _ = events
            .publish("vm.idle_channel_failure", "runtime", row)
            .await;
    });
}
fn failure_row(
    side: &str,
    phase: &str,
    error: &anyhow::Error,
    state: serde_json::Value,
) -> serde_json::Value {
    let errno = error
        .chain()
        .find_map(|e| e.downcast_ref::<std::io::Error>())
        .and_then(|e| e.raw_os_error());
    serde_json::json!({"side":side, "phase":phase, "pid":std::process::id(),
        "unix_ms":pvisor_core::unix_now_ms(), "error":format!("{error:#}"), "errno":errno, "state":state})
}
fn runner_failure(phase: &str, error: anyhow::Error, state: serde_json::Value) -> ! {
    let row = failure_row("runner", phase, &error, state);
    // A full stderr pipe must not defeat fail-closed termination. Usually this
    // completes immediately; a blocked diagnostic gets at most 20 ms to finish.
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let _ = std::thread::Builder::new()
        .name("idle-failure-log".into())
        .spawn(move || {
            eprintln!("pvisor idle channel failure: {row}");
            let _ = tx.send(());
        });
    let _ = rx.recv_timeout(Duration::from_millis(20));
    std::process::exit(1);
}
fn read_request(reader: &mut impl std::io::BufRead) -> anyhow::Result<Request> {
    use std::io::{BufRead, Read};
    let mut line = Vec::new();
    let n = reader
        .take(4097)
        .read_until(b'\n', &mut line)
        .context("controller read I/O or heartbeat timeout (3s)")?;
    anyhow::ensure!(n != 0, "controller EOF");
    anyhow::ensure!(
        n <= 4096 && line.last() == Some(&b'\n'),
        "controller frame oversized or incomplete"
    );
    serde_json::from_slice(&line).context("invalid controller request")
}
fn write_status(stream: &mut impl std::io::Write, status: &GuestIdleStatus) -> anyhow::Result<()> {
    let mut line = serde_json::to_vec(status).context("encode idle status")?;
    line.push(b'\n');
    stream
        .write_all(&line)
        .context("status write I/O or backpressure timeout (1s)")
}
pub(super) fn serve(
    mut stream: std::os::unix::net::UnixStream,
    handle: VmmHandle,
) -> anyhow::Result<()> {
    let read = stream.try_clone()?;
    read.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    let commands = handle.clone();
    std::thread::Builder::new()
        .name("pvisor-idle-commands".into())
        .spawn(move || {
            let mut reader = std::io::BufReader::new(read);
            let mut last_request = std::time::Instant::now();
            loop {
                let request = read_request(&mut reader).unwrap_or_else(|error| {
                    runner_failure(
                        "controller_read",
                        error,
                        serde_json::json!({
                        "since_last_request_ms":last_request.elapsed().as_millis()}),
                    )
                });
                last_request = std::time::Instant::now();
                match request {
                    Request::Arm => {
                        if let Err(error) = commands.arm_guest_idle(Duration::from_secs(1)) {
                            runner_failure("arm", anyhow::anyhow!(error), serde_json::Value::Null);
                        }
                    }
                    Request::Heartbeat => {}
                    Request::Retire { epoch } => commands.retire_guest_idle(epoch),
                }
            }
        })?;
    std::thread::Builder::new()
        .name("pvisor-idle-events".into())
        .spawn(move || {
            let mut sequence = u64::MAX;
            let mut heartbeat = std::time::Instant::now();
            loop {
                let status = handle.wait_guest_idle(sequence, Duration::from_millis(100));
                if status.sequence != sequence || heartbeat.elapsed() >= Duration::from_millis(500)
                {
                    heartbeat = std::time::Instant::now();
                    sequence = status.sequence;
                    let started = std::time::Instant::now();
                    if let Err(error) = write_status(&mut stream, &status) {
                        runner_failure(
                            "status_write",
                            error,
                            serde_json::json!({
                            "sequence":status.sequence, "epoch":status.epoch,
                            "claimed":status.claimed, "write_ms":started.elapsed().as_millis()}),
                        );
                    }
                }
            }
        })?;
    Ok(())
}

#[cfg(all(test, feature = "gateway"))]
mod tests {
    use super::*;
    #[test]
    fn controller_eof_invalid_and_oversized_frames_are_distinct() {
        for (input, expected) in [
            (Vec::new(), "EOF"),
            (b"not-json\n".to_vec(), "invalid controller request"),
            (vec![b'x'; 4097], "oversized"),
        ] {
            let error = read_request(&mut std::io::Cursor::new(input))
                .err()
                .unwrap();
            assert!(format!("{error:#}").contains(expected));
        }
        assert!(matches!(
            read_request(&mut std::io::Cursor::new(b"{\"op\":\"Heartbeat\"}\n")).unwrap(),
            Request::Heartbeat
        ));
    }
    #[test]
    fn stalled_controller_read_and_status_backpressure_remain_bounded() {
        use std::os::unix::net::UnixStream;
        let (mut runner, _controller) = UnixStream::pair().unwrap();
        runner
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let error = read_request(&mut std::io::BufReader::new(runner.try_clone().unwrap()))
            .err()
            .unwrap();
        let row = failure_row("runner", "controller_read", &error, serde_json::Value::Null);
        assert!(row["errno"].as_i64().is_some());
        runner
            .set_write_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        // An unread peer eventually backpressures the real Unix socket.
        let began = std::time::Instant::now();
        let error = loop {
            if let Err(error) = write_status(&mut runner, &status(1, true, 1_000_000_000)) {
                break error;
            }
            assert!(began.elapsed() < Duration::from_secs(3));
        };
        assert!(format!("{error:#}").contains("status write"));
        assert!(began.elapsed() < Duration::from_secs(3));
    }
    fn status(epoch: u64, offered: bool, remaining_ns: u64) -> GuestIdleStatus {
        GuestIdleStatus {
            offer_sequence: None,
            epoch,
            sequence: epoch * 10 + u64::from(offered),
            enabled: true,
            offered,
            claimed: false,
            remaining_ns,
            wake_reason: None,
            pending_host_reads: 0,
            offload_minimum_ns: 1_000_000_000,
            offload_deadline_skips: 0,
        }
    }
    async fn request(reader: &mut BufReader<tokio::net::UnixStream>) -> Request {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }
    async fn pair() -> (IdleClient, BufReader<tokio::net::UnixStream>) {
        let client = IdleClient::default();
        let (host, runner) = tokio::net::UnixStream::pair().unwrap();
        let (read, write) = host.into_split();
        // Retain the read half in this local fake so the socket remains connected.
        tokio::spawn(async move {
            let _read = read;
            std::future::pending::<()>().await;
        });
        *client.writer.lock().await = Some(write);
        (client, BufReader::new(runner))
    }
    #[tokio::test]
    async fn resume_listener_handles_offer_before_ack_and_retires_exact_epoch() {
        let (client, mut runner) = pair().await;
        let wake = CancellationToken::new();
        let rx = client
            .resume_prepare(wake.clone(), async {
                client
                    .status
                    .send_replace(Some(status(7, true, 10_000_000)));
                Ok(7)
            })
            .await
            .unwrap();
        assert_eq!(rx.await.unwrap(), Some(7));
        wake.cancel();
        assert!(matches!(
            request(&mut runner).await,
            Request::Retire { epoch: 7 }
        ));
        client.status.send_replace(Some(status(7, false, 0)));
        client.settled().await.unwrap();
    }
    #[tokio::test]
    async fn cancelled_resume_listener_still_retires_arm_created_before_ack() {
        let (client, mut runner) = pair().await;
        let wake = CancellationToken::new();
        let rx = client
            .resume_prepare(wake.clone(), async {
                wake.cancel();
                client
                    .status
                    .send_replace(Some(status(9, true, 10_000_000)));
                Ok(9)
            })
            .await
            .unwrap();
        assert!(matches!(
            request(&mut runner).await,
            Request::Retire { epoch: 9 }
        ));
        client.status.send_replace(Some(status(9, false, 0)));
        assert_eq!(rx.await.unwrap(), None);
        client.settled().await.unwrap();
    }
    #[tokio::test]
    async fn cancellation_before_arm_ack_retires_the_expected_epoch() {
        let (client, mut runner) = pair().await;
        let wake = CancellationToken::new();
        let c = client.clone();
        let w = wake.clone();
        let permit = tokio::spawn(async move { c.prepare(w).await.unwrap() });
        assert!(matches!(request(&mut runner).await, Request::Arm));
        wake.cancel();
        assert!(matches!(
            request(&mut runner).await,
            Request::Retire { epoch: 1 }
        ));
        client.status.send_replace(Some(status(1, false, 0)));
        assert_eq!(permit.await.unwrap(), None);
        assert!(!client.failed.is_cancelled());
        // Retirement ACK is a barrier before another arm can enter the channel.
        let c = client.clone();
        let wake2 = CancellationToken::new();
        let w = wake2.clone();
        let permit = tokio::spawn(async move { c.prepare(w).await.unwrap() });
        assert!(matches!(request(&mut runner).await, Request::Arm));
        client.status.send_replace(Some(status(1, true, 1_000_000)));
        tokio::task::yield_now().await;
        assert!(!permit.is_finished(), "stale offer authorized a new wait");
        client
            .status
            .send_replace(Some(status(2, true, 100_000_000)));
        assert_eq!(permit.await.unwrap(), Some(2));
        wake2.cancel();
        assert!(matches!(
            request(&mut runner).await,
            Request::Retire { epoch: 2 }
        ));
        client.status.send_replace(Some(status(2, false, 0)));
    }
    #[tokio::test(start_paused = true)]
    async fn accepted_offer_outlives_acquisition_ack_timeout() {
        let (client, mut runner) = pair().await;
        let wake = CancellationToken::new();
        let c = client.clone();
        let w = wake.clone();
        let permit = tokio::spawn(async move { c.prepare(w).await.unwrap() });
        assert!(matches!(request(&mut runner).await, Request::Arm));
        client
            .status
            .send_replace(Some(status(1, true, 30_000_000_000)));
        assert_eq!(permit.await.unwrap(), Some(1));
        tokio::time::advance(Duration::from_secs(5)).await;
        tokio::task::yield_now().await;
        assert!(!wake.is_cancelled());
        assert!(!client.failed.is_cancelled());
        wake.cancel();
        assert!(matches!(
            request(&mut runner).await,
            Request::Retire { epoch: 1 }
        ));
        client.status.send_replace(Some(status(1, false, 0)));
    }
}
