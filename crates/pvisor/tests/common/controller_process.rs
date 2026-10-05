//! Test-owned CLI controller and a stable streaming proxy for artifact gates.
//! Only the controller process is killed; the proxy, Worker and VMs stay live.
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use pvisor_cluster::{
    ArtifactStorageLimits, InferenceWaitIntent, InferenceWaitKey, InferenceWaitReceipt,
    InferenceWaitRequest,
};
use std::{
    fs,
    net::SocketAddr,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

pub struct ReadyFailure {
    committed: tokio::sync::watch::Receiver<Option<InferenceWaitKey>>,
    release: tokio::sync::watch::Sender<bool>,
    failures: Arc<AtomicUsize>,
}

impl ReadyFailure {
    pub async fn committed(&mut self) -> InferenceWaitKey {
        tokio::time::timeout(
            Duration::from_secs(1),
            self.committed.wait_for(Option::is_some),
        )
        .await
        .unwrap()
        .unwrap()
        .as_ref()
        .unwrap()
        .clone()
    }

    pub async fn release_as_failure(&self) {
        self.release.send_replace(true);
        tokio::time::timeout(Duration::from_secs(1), async {
            while self.failures.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(self.failures.load(Ordering::SeqCst), 1);
    }
}

pub struct Controller {
    child: Child,
    root: PathBuf,
    binary: PathBuf,
    log: PathBuf,
    address: SocketAddr,
    lease_ms: u64,
    admin: String,
    worker: String,
}

impl Drop for Controller {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Controller {
    pub async fn start(
        root: &Path,
        worker_binary: &Path,
        lease_ms: u64,
        limits: &ArtifactStorageLimits,
        admin: &str,
        worker: &str,
    ) -> Self {
        let source = std::env::var_os("PVISOR_TEST_CONTROLLER_BINARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| worker_binary.parent().unwrap().join("pvisor-cluster"));
        assert!(
            source.is_file(),
            "build the controller with just test-cluster-vm-gateway"
        );
        let binary = root.join("pvisor-cluster");
        fs::copy(source, &binary).unwrap();
        fs::write(
            root.join("artifact-limits.json"),
            serde_json::to_vec(limits).unwrap(),
        )
        .unwrap();
        let log = root.join("controller-first.log");
        let child = Self::spawn(root, &binary, &log, "127.0.0.1:0", lease_ms, admin, worker);
        let mut controller = Self {
            child,
            root: root.into(),
            binary,
            log,
            address: "127.0.0.1:0".parse().unwrap(),
            lease_ms,
            admin: admin.into(),
            worker: worker.into(),
        };
        controller.address = controller.ready().await;
        controller
    }

    fn spawn(
        root: &Path,
        binary: &Path,
        log: &Path,
        listen: &str,
        lease_ms: u64,
        admin: &str,
        worker: &str,
    ) -> Child {
        Command::new(binary)
            .args(["serve", "--listen", listen, "--journal"])
            .arg(root.join("journal"))
            .arg("--lease-ms")
            .arg(lease_ms.to_string())
            .arg("--artifact-limits")
            .arg(root.join("artifact-limits.json"))
            .env("PVISOR_CLUSTER_TOKEN", admin)
            .env("PVISOR_CLUSTER_WORKER_TOKEN", worker)
            .stdout(Stdio::null())
            .stderr(Stdio::from(fs::File::create(log).unwrap()))
            .spawn()
            .unwrap()
    }

    async fn ready(&mut self) -> SocketAddr {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                assert!(
                    self.child.try_wait().unwrap().is_none(),
                    "{}",
                    fs::read_to_string(&self.log).unwrap()
                );
                if let Some(address) = fs::read_to_string(&self.log)
                    .unwrap()
                    .lines()
                    .find_map(|line| line.strip_prefix("pVisor controller listening on "))
                {
                    break address.parse().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("controller starts within the existing Worker watchdog")
    }

    pub fn kill(&mut self) {
        self.child.kill().unwrap();
        assert_eq!(self.child.wait().unwrap().signal(), Some(libc::SIGKILL));
    }

    pub async fn restart(&mut self) {
        assert!(self.child.try_wait().unwrap().is_some());
        self.log = self.root.join("controller-restarted.log");
        self.child = Self::spawn(
            &self.root,
            &self.binary,
            &self.log,
            &self.address.to_string(),
            self.lease_ms,
            &self.admin,
            &self.worker,
        );
        assert_eq!(self.ready().await, self.address);
    }

    pub fn proxy(&self) -> (Router, ReadyFailure) {
        #[derive(Clone)]
        struct Proxy {
            base: String,
            http: reqwest::Client,
            armed: Arc<AtomicBool>,
            committed: tokio::sync::watch::Sender<Option<InferenceWaitKey>>,
            release: tokio::sync::watch::Receiver<bool>,
            failures: Arc<AtomicUsize>,
        }
        async fn forward(State(proxy): State<Proxy>, request: Request) -> Response {
            let (parts, body) = request.into_parts();
            let path = parts.uri.path_and_query().unwrap().as_str();
            let (body, ready) = if parts.uri.path() == "/v1/workers/inference-wait" {
                let bytes = axum::body::to_bytes(body, 64 * 1024).await.unwrap();
                let request: InferenceWaitRequest = serde_json::from_slice(&bytes).unwrap();
                (
                    reqwest::Body::from(bytes),
                    request.intent == InferenceWaitIntent::Ready,
                )
            } else {
                (reqwest::Body::wrap_stream(body.into_data_stream()), false)
            };
            let result = proxy
                .http
                .request(parts.method, format!("{}{path}", proxy.base))
                .headers(parts.headers)
                .body(body)
                .send()
                .await;
            match result {
                Ok(reply) => {
                    if ready
                        && reply.status().is_success()
                        && proxy.armed.swap(false, Ordering::SeqCst)
                    {
                        // Receipt bytes can only arrive after the real CLI's
                        // durability barrier. Keep them away from the Worker,
                        // then discard them after that CLI has been SIGKILLed.
                        let receipt: InferenceWaitReceipt =
                            serde_json::from_slice(&reply.bytes().await.unwrap()).unwrap();
                        assert!(receipt.record.ready && !receipt.delivery_ready);
                        proxy.committed.send_replace(Some(receipt.record.key));
                        let mut release = proxy.release.clone();
                        let _ = release.wait_for(|released| *released).await;
                        proxy.failures.fetch_add(1, Ordering::SeqCst);
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            "injected lost Ready response",
                        )
                            .into_response();
                    }
                    let status = reply.status();
                    let headers = reply.headers().clone();
                    let mut response = Response::new(Body::from_stream(reply.bytes_stream()));
                    *response.status_mut() = status;
                    *response.headers_mut() = headers;
                    response
                }
                Err(error) => (StatusCode::SERVICE_UNAVAILABLE, error.to_string()).into_response(),
            }
        }
        let (committed, observed) = tokio::sync::watch::channel(None);
        let (release, gate) = tokio::sync::watch::channel(false);
        let failures = Arc::new(AtomicUsize::new(0));
        let router = Router::new().fallback(forward).with_state(Proxy {
            base: format!("http://{}", self.address),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            armed: Arc::new(AtomicBool::new(true)),
            committed,
            release: gate,
            failures: failures.clone(),
        });
        (
            router,
            ReadyFailure {
                committed: observed,
                release,
                failures,
            },
        )
    }
}
