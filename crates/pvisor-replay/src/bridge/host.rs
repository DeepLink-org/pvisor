//! Loopback transport ownership shared by the protocol-specific bridges.

use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use axum::Router;
use tokio::sync::Notify;

use crate::error::{ReplayError, ReplayErrorKind, ResultExt};

const START_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug)]
struct DrainTimeout;

impl std::fmt::Display for DrainTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bridge graceful shutdown timed out")
    }
}

impl std::error::Error for DrainTimeout {}

#[derive(Default)]
pub(super) struct Cancellation {
    cancelled: AtomicBool,
    notify: Notify,
}

impl Cancellation {
    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(super) async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

/// Preserve each bridge's existing executor choice without duplicating servers.
#[derive(Clone, Copy)]
pub(super) enum WorkerRuntime {
    CurrentThread,
    TwoThreads,
}

pub(super) fn bind_loopback(agent: &str) -> Result<(TcpListener, SocketAddr), ReplayError> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).replay_context(
        ReplayErrorKind::Continuation,
        format!("allocate {agent} SandboxReplay bridge port"),
    )?;
    let address = listener.local_addr().replay_context(
        ReplayErrorKind::Continuation,
        format!("read {agent} SandboxReplay bridge address"),
    )?;
    listener.set_nonblocking(true).replay_context(
        ReplayErrorKind::Continuation,
        format!("configure {agent} SandboxReplay bridge listener"),
    )?;
    Ok((listener, address))
}

pub(super) fn bridge_api_key() -> String {
    format!("pvisor-sandbox-replay-{}", uuid::Uuid::new_v4().simple())
}

pub(super) fn upstream_client(agent: &str) -> Result<reqwest::Client, ReplayError> {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .replay_context(
            ReplayErrorKind::Continuation,
            format!("build {agent} SandboxReplay bridge client"),
        )
}

/// Owns only transport resources; protocol validation stays in each handle.
pub(super) struct BridgeHost {
    agent: &'static str,
    cancellation: Arc<Cancellation>,
    worker_done: Option<mpsc::Receiver<anyhow::Result<()>>>,
    worker: Option<JoinHandle<()>>,
    stop_timeout: Duration,
    stop_error_kind: ReplayErrorKind,
}

impl BridgeHost {
    pub(super) fn start(
        agent: &'static str,
        listener: TcpListener,
        router: Router,
        cancellation: Arc<Cancellation>,
        runtime: WorkerRuntime,
        stop_error_kind: ReplayErrorKind,
    ) -> Result<Self, ReplayError> {
        let worker_cancel = cancellation.clone();
        Self::spawn(
            agent,
            cancellation,
            stop_error_kind,
            START_TIMEOUT,
            move |ready| run_worker(listener, router, worker_cancel, runtime, ready),
        )
    }

    fn spawn(
        agent: &'static str,
        cancellation: Arc<Cancellation>,
        stop_error_kind: ReplayErrorKind,
        start_timeout: Duration,
        run: impl FnOnce(mpsc::SyncSender<Result<(), String>>) -> anyhow::Result<()> + Send + 'static,
    ) -> Result<Self, ReplayError> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name(format!(
                "pvisor-{}-replay-bridge",
                agent.to_ascii_lowercase()
            ))
            .spawn(move || {
                let _ = done_tx.send(run(ready_tx));
            })
            .replay_context(
                ReplayErrorKind::Continuation,
                format!("start {agent} SandboxReplay bridge thread"),
            )?;
        let mut host = Self {
            agent,
            cancellation,
            worker_done: Some(done_rx),
            worker: Some(worker),
            stop_timeout: STOP_TIMEOUT,
            stop_error_kind,
        };
        let error = match ready_rx.recv_timeout(start_timeout) {
            Ok(Ok(())) => return Ok(host),
            Ok(Err(message)) => message,
            Err(mpsc::RecvTimeoutError::Timeout) => format!(
                "{agent} SandboxReplay bridge did not become ready within {} seconds",
                start_timeout.as_secs_f64()
            ),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                format!("{agent} SandboxReplay bridge exited before reporting readiness")
            }
        };
        let cleanup = host.stop().err();
        Err(ReplayError::continuation(match cleanup {
            Some(cleanup) => format!("{error}; cleanup also failed: {cleanup}"),
            None => error,
        }))
    }

    pub(super) fn stop(&mut self) -> Result<(), ReplayError> {
        self.cancellation.cancel();
        let result = match self.worker_done.take() {
            Some(done) => match done.recv_timeout(self.stop_timeout) {
                Ok(result) => Some(result),
                Err(mpsc::RecvTimeoutError::Disconnected) => None,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.worker.take();
                    return Err(ReplayError::continuation(format!(
                        "{} SandboxReplay bridge did not stop within {} seconds; detached worker",
                        self.agent,
                        self.stop_timeout.as_secs_f64(),
                    )));
                }
            },
            None => None,
        };
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            return Err(ReplayError::continuation(format!(
                "{} SandboxReplay bridge thread panicked",
                self.agent
            )));
        }
        if let Some(result) = result {
            let kind = if result
                .as_ref()
                .err()
                .is_some_and(|error| error.is::<DrainTimeout>())
            {
                ReplayErrorKind::Continuation
            } else {
                self.stop_error_kind
            };
            result.replay_context(kind, format!("stop {} SandboxReplay bridge", self.agent))?;
        }
        Ok(())
    }
}

impl Drop for BridgeHost {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn run_worker(
    listener: TcpListener,
    router: Router,
    cancellation: Arc<Cancellation>,
    kind: WorkerRuntime,
    ready: mpsc::SyncSender<Result<(), String>>,
) -> anyhow::Result<()> {
    let mut builder = match kind {
        WorkerRuntime::CurrentThread => tokio::runtime::Builder::new_current_thread(),
        WorkerRuntime::TwoThreads => {
            let mut builder = tokio::runtime::Builder::new_multi_thread();
            builder.worker_threads(2);
            builder
        }
    };
    let runtime = match builder.enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(format!("build bridge runtime: {error}")));
            return Err(error.into());
        }
    };
    let result = runtime.block_on(async move {
        let listener = tokio::net::TcpListener::from_std(listener).inspect_err(|error| {
            let _ = ready.send(Err(format!("adopt bridge listener: {error}")));
        })?;
        ready
            .send(Ok(()))
            .map_err(|_| anyhow::anyhow!("bridge startup receiver was dropped"))?;
        let shutdown = cancellation.clone();
        let server = axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown.cancelled().await })
            .into_future();
        tokio::pin!(server);
        tokio::select! {
            result = &mut server => result?,
            _ = cancellation.cancelled() => {
                // Bound partial uploads/non-cooperative handlers too. On timeout
                // the runtime is stopped below and finish must report failure.
                tokio::time::timeout(DRAIN_TIMEOUT, &mut server)
                    .await
                    .map_err(|_| DrainTimeout)??;
            }
        }
        Ok(())
    });
    // Do not report a clean stop while runtime-owned work still exists. If
    // runtime teardown itself cannot finish, BridgeHost's outer stop deadline
    // reports an unconfirmed/detached worker instead of success.
    drop(runtime);
    result
}

pub(super) fn merged_no_proxy_environment() -> String {
    let configured: Vec<_> = ["NO_PROXY", "no_proxy"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .collect();
    merge_no_proxy_values(configured.iter().map(String::as_str))
}

fn merge_no_proxy_values<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    let mut entries = Vec::<String>::new();
    for entry in values
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .chain(["127.0.0.1", "localhost", "::1"])
    {
        if !entries.iter().any(|existing| existing == entry) {
            entries.push(entry.to_owned());
        }
    }
    entries.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_proxy_merge_preserves_values_and_guarantees_loopback() {
        assert_eq!(
            merge_no_proxy_values(["example.internal, localhost", "10.0.0.0/8,example.internal"]),
            "example.internal,localhost,10.0.0.0/8,127.0.0.1,::1"
        );
        assert_eq!(merge_no_proxy_values([" , "]), "127.0.0.1,localhost,::1");
        assert_ne!(bridge_api_key(), bridge_api_key());
    }

    #[tokio::test]
    async fn cancellation_is_latched_and_wakes_all_waiters() {
        let cancellation = Arc::new(Cancellation::default());
        let mut waiters = Vec::new();
        let mut registrations = Vec::new();
        for _ in 0..8 {
            let cancellation = cancellation.clone();
            let (registered, ready) = tokio::sync::oneshot::channel();
            registrations.push(ready);
            waiters.push(tokio::spawn(async move {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {},
                    _ = async {
                        // The first branch has already registered its waiter.
                        registered.send(()).unwrap();
                        std::future::pending::<()>().await;
                    } => unreachable!(),
                }
            }));
        }
        for ready in registrations {
            tokio::time::timeout(Duration::from_secs(2), ready)
                .await
                .unwrap()
                .unwrap();
        }
        cancellation.cancel();
        for waiter in waiters {
            tokio::time::timeout(Duration::from_secs(2), waiter)
                .await
                .unwrap()
                .unwrap();
        }
        cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(2), cancellation.cancelled())
            .await
            .unwrap();
    }

    #[test]
    fn both_runtime_styles_serve_and_drop_releases_the_listener() {
        for kind in [WorkerRuntime::CurrentThread, WorkerRuntime::TwoThreads] {
            let (listener, address) = bind_loopback("test").unwrap();
            assert!(address.ip().is_loopback());
            let router = Router::new().route("/health", axum::routing::get(|| async { "ready" }));
            let mut host = BridgeHost::start(
                "test",
                listener,
                router,
                Arc::new(Cancellation::default()),
                kind,
                ReplayErrorKind::Continuation,
            )
            .unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                assert_eq!(
                    upstream_client("test")
                        .unwrap()
                        .get(format!("http://{address}/health"))
                        .send()
                        .await
                        .unwrap()
                        .text()
                        .await
                        .unwrap(),
                    "ready"
                );
            });
            host.stop().unwrap();
            host.stop().unwrap();
            drop(host);
            assert!(TcpListener::bind(address).is_ok());
        }
        let (listener, address) = bind_loopback("test").unwrap();
        let host = BridgeHost::start(
            "test",
            listener,
            Router::new(),
            Arc::new(Cancellation::default()),
            WorkerRuntime::CurrentThread,
            ReplayErrorKind::Executor,
        )
        .unwrap();
        drop(host);
        assert!(TcpListener::bind(address).is_ok());
    }

    #[test]
    fn startup_error_and_panic_release_owned_resources() {
        for panic in [false, true] {
            let (listener, address) = bind_loopback("test").unwrap();
            let result = BridgeHost::spawn(
                "test",
                Arc::new(Cancellation::default()),
                ReplayErrorKind::Executor,
                START_TIMEOUT,
                move |ready| {
                    let _listener = listener;
                    if panic {
                        panic!("injected bridge worker panic");
                    }
                    ready.send(Err("injected startup failure".into())).unwrap();
                    anyhow::bail!("injected worker failure")
                },
            );
            let error = result.err().expect("startup must fail");
            assert_eq!(error.kind, ReplayErrorKind::Continuation);
            assert!(error.message.contains(if panic {
                "panicked"
            } else {
                "injected startup failure"
            }));
            assert!(TcpListener::bind(address).is_ok());
        }
    }

    #[test]
    fn startup_timeout_cancels_and_joins_the_worker() {
        let (listener, address) = bind_loopback("test").unwrap();
        let cancellation = Arc::new(Cancellation::default());
        let worker_cancel = cancellation.clone();
        let result = BridgeHost::spawn(
            "test",
            cancellation.clone(),
            ReplayErrorKind::Executor,
            Duration::from_millis(20),
            move |_ready| {
                let _listener = listener;
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?
                    .block_on(worker_cancel.cancelled());
                Ok(())
            },
        );
        assert!(
            result
                .err()
                .unwrap()
                .message
                .contains("did not become ready")
        );
        assert!(cancellation.is_cancelled());
        assert!(TcpListener::bind(address).is_ok());
    }

    #[test]
    fn worker_error_is_reported_and_noncooperative_stop_is_not_success() {
        let mut host = BridgeHost::spawn(
            "test",
            Arc::new(Cancellation::default()),
            ReplayErrorKind::Executor,
            START_TIMEOUT,
            |ready| {
                ready.send(Ok(())).unwrap();
                anyhow::bail!("injected serve failure")
            },
        )
        .unwrap();
        assert_eq!(host.stop().unwrap_err().kind, ReplayErrorKind::Executor);

        let (release, released) = mpsc::channel();
        let (finished, completion) = mpsc::channel();
        let mut host = BridgeHost::spawn(
            "test",
            Arc::new(Cancellation::default()),
            ReplayErrorKind::Executor,
            START_TIMEOUT,
            move |ready| {
                ready.send(Ok(())).unwrap();
                released.recv().unwrap();
                finished.send(()).unwrap();
                Ok(())
            },
        )
        .unwrap();
        host.stop_timeout = Duration::from_millis(20);
        let error = host.stop().unwrap_err();
        release.send(()).unwrap();
        completion.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(error.message.contains("did not stop"));
        assert!(host.worker.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stalled_handlers_are_closed_and_drain_timeout_is_reported() {
        let (listener, address) = bind_loopback("test").unwrap();
        let (entered, started) = tokio::sync::oneshot::channel();
        let entered = Arc::new(std::sync::Mutex::new(Some(entered)));
        let router = Router::new().route(
            "/pending",
            axum::routing::get(move || {
                let entered = entered.clone();
                async move {
                    entered.lock().unwrap().take().unwrap().send(()).unwrap();
                    std::future::pending::<String>().await
                }
            }),
        );
        let mut host = BridgeHost::start(
            "test",
            listener,
            router,
            Arc::new(Cancellation::default()),
            WorkerRuntime::TwoThreads,
            ReplayErrorKind::Executor,
        )
        .unwrap();
        let request = tokio::spawn(async move {
            upstream_client("test")
                .unwrap()
                .get(format!("http://{address}/pending"))
                .send()
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), started)
            .await
            .unwrap()
            .unwrap();
        let error = host.stop().unwrap_err();
        assert_eq!(error.kind, ReplayErrorKind::Continuation);
        assert!(error.message.contains("graceful shutdown timed out"));
        assert!(host.worker.is_none());
        assert!(TcpListener::bind(address).is_ok());
        assert!(
            tokio::time::timeout(Duration::from_secs(2), request)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }
}
