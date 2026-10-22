//! Bounded live event recording. Only the blocking writer touches the file.
//! Overflow/errors invalidate live evidence; the owner's final array is retained.
use anyhow::Context;
use serde::Serialize;
use std::io::Write;
use tokio::sync::{broadcast, mpsc, oneshot};

pub(super) struct EventLog {
    stop: oneshot::Sender<()>,
    pump: tokio::task::JoinHandle<anyhow::Result<()>>,
    writer: tokio::task::JoinHandle<anyhow::Result<()>>,
}
impl EventLog {
    pub(super) fn start<T, W, F>(mut source: broadcast::Receiver<T>, open: F) -> Self
    where
        T: Serialize + Clone + Send + Sync + 'static,
        W: Write + Send + 'static,
        F: FnOnce() -> std::io::Result<W> + Send + 'static,
    {
        // Backpressure suspends only this pump, never a Tokio worker thread or
        // the control producer. The source reports lag explicitly if exceeded.
        let (tx, mut rx) = mpsc::channel::<T>(4096);
        let (stop, mut stopped) = oneshot::channel();
        let writer = tokio::task::spawn_blocking(move || {
            let mut output = open().context("open runtime event log")?;
            while let Some(event) = rx.blocking_recv() {
                serde_json::to_writer(&mut output, &event).context("serialize runtime event")?;
                output.write_all(b"\n").context("write runtime event")?;
                output.flush().context("flush runtime event")?;
            }
            Ok(())
        });
        let pump = tokio::spawn(async move {
            loop {
                let event = tokio::select! {
                    _ = &mut stopped => break,
                    event = source.recv() => event.context("runtime event subscription (possible lag)")?,
                };
                tx.send(event)
                    .await
                    .context("runtime event writer stopped")?;
            }
            // Completion is a drain barrier, not an abort that loses the tail.
            loop {
                match source.try_recv() {
                    Ok(event) => tx
                        .send(event)
                        .await
                        .context("runtime event writer stopped")?,
                    Err(
                        broadcast::error::TryRecvError::Empty
                        | broadcast::error::TryRecvError::Closed,
                    ) => break,
                    Err(error) => return Err(error).context("runtime event drain lagged"),
                }
            }
            Ok(())
        });
        Self { stop, pump, writer }
    }
    pub(super) async fn finish(self) -> anyhow::Result<()> {
        let _ = self.stop.send(());
        // Always join both tasks, even if one failed. Dropping tx unblocks writer.
        let pump = self.pump.await.context("event pump task");
        let writer = self.writer.await.context("event writer task")?;
        writer?;
        pump??;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct SlowWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
        entered: Option<oneshot::Sender<()>>,
        release: std::sync::mpsc::Receiver<()>,
    }
    impl Write for SlowWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
                self.release.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    // One async worker makes accidental synchronous I/O a deterministic failure.
    #[tokio::test(flavor = "current_thread")]
    async fn blocked_writer_does_not_stop_control_ticks_and_shutdown_drains_in_order() {
        let (tx, rx) = broadcast::channel(8192);
        let (entered, started) = oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let output = bytes.clone();
        let log = EventLog::start(rx, move || {
            Ok(SlowWriter {
                bytes: output,
                entered: Some(entered),
                release: blocked,
            })
        });
        tx.send(0u64).unwrap();
        started.await.unwrap();
        // Fill the bounded writer queue while the disk is blocked.
        for value in 1..5000 {
            tx.send(value).unwrap();
        }
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        release.send(()).unwrap();
        log.finish().await.unwrap();
        let values: Vec<u64> = String::from_utf8(bytes.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(values, (0..5000).collect::<Vec<_>>());
    }
    #[tokio::test]
    async fn open_failure_is_not_silently_accepted() {
        let (tx, rx) = broadcast::channel(8);
        let log = EventLog::start(rx, || {
            Err::<Vec<u8>, _>(std::io::Error::other("disk unavailable"))
        });
        tx.send(1u64).unwrap();
        assert!(format!("{:#}", log.finish().await.unwrap_err()).contains("disk unavailable"));
    }
    #[tokio::test]
    async fn subscription_overflow_invalidates_live_recording() {
        let (tx, rx) = broadcast::channel(2);
        for n in 0..4 {
            tx.send(n).unwrap();
        }
        let log = EventLog::start(rx, || Ok(Vec::<u8>::new()));
        assert!(format!("{:#}", log.finish().await.unwrap_err()).contains("lag"));
    }
}
