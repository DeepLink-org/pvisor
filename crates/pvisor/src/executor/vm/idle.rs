//! Private, independent parent/runner idle channel. No credentials or guest RAM.
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
        // Diagnostics may coalesce; journal latency must never block wake delivery.
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = observation_cancel.cancelled() => break,
                    changed = observations.changed() => if changed.is_err() { break; },
                }
                let value = observations.borrow_and_update().clone();
                if let Some(value) = value {
                    let _ = events
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
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = stop.cancelled() => break,
                        _ = tokio::time::sleep(Duration::from_millis(500)) => {},
                    }
                    if client.send(&Request::Heartbeat).await.is_err() {
                        stop.cancel();
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
                        n = tokio::time::timeout(Duration::from_secs(3), limited.read_until(b'\n', &mut bytes)) => n??,
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
                    let next: GuestIdleStatus = serde_json::from_slice(&bytes)?;
                    if status.borrow().as_ref().is_none_or(|s| s.sequence != next.sequence) {
                        status.send_replace(Some(next));
                    }
                }
            }
            .await;
            status.send_replace(None);
            if result.is_err() {
                cancel.cancel();
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
        tokio::time::timeout(Duration::from_secs(1), writer.write_all(&bytes)).await??;
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
            if client.send(&Request::Arm).await.is_err() {
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
        if result.is_err() {
            self.failed.cancel();
        }
        if let Some(tx) = tx {
            let _ = tx.send(None);
        }
    }
}

pub(super) fn serve(
    stream: std::os::unix::net::UnixStream,
    handle: VmmHandle,
) -> anyhow::Result<()> {
    use std::io::{BufRead, Read, Write};
    let read = stream.try_clone()?;
    read.set_read_timeout(Some(Duration::from_secs(3)))?;
    let commands = handle.clone();
    std::thread::Builder::new()
        .name("pvisor-idle-commands".into())
        .spawn(move || {
            let mut reader = std::io::BufReader::new(read);
            loop {
                let mut line = Vec::new();
                let n = match (&mut reader).take(4097).read_until(b'\n', &mut line) {
                    Ok(n) => n,
                    Err(error) => {
                        eprintln!("pvisor idle controller heartbeat/channel failure: {error}");
                        std::process::exit(1);
                    }
                };
                if n == 0 || n > 4096 || line.last() != Some(&b'\n') {
                    std::process::exit(1);
                }
                match serde_json::from_slice(&line) {
                    Ok(Request::Arm) => {
                        if commands.arm_guest_idle(Duration::from_secs(1)).is_err() {
                            std::process::exit(1);
                        }
                    }
                    Ok(Request::Heartbeat) => {}
                    Ok(Request::Retire { epoch }) => commands.retire_guest_idle(epoch),
                    Err(_) => std::process::exit(1),
                }
            }
        })?;
    std::thread::Builder::new()
        .name("pvisor-idle-events".into())
        .spawn(move || {
            let mut stream = stream;
            // A stopped controller cannot leave a single-vCPU kernel in idle forever.
            let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
            let mut sequence = u64::MAX;
            let mut heartbeat = std::time::Instant::now();
            loop {
                let status = handle.wait_guest_idle(sequence, Duration::from_millis(100));
                if status.sequence != sequence || heartbeat.elapsed() >= Duration::from_millis(500)
                {
                    heartbeat = std::time::Instant::now();
                    sequence = status.sequence;
                    let mut line = serde_json::to_vec(&status).unwrap();
                    line.push(b'\n');
                    if stream.write_all(&line).is_err() {
                        std::process::exit(1);
                    }
                }
            }
        })?;
    Ok(())
}

#[cfg(all(test, feature = "gateway"))]
mod tests {
    use super::*;
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
