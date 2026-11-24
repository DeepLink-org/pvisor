//! Real API process faults on Linux/KVM, without launching a guest.
#![cfg(all(target_os = "linux", target_arch = "x86_64", feature = "daemon-bin"))]

use anyhow::{Context, Result, ensure};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use std::{
    fs::{self, File, Permissions},
    net::{Ipv4Addr, SocketAddrV4},
    os::unix::{fs::PermissionsExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    process::{Child, Command},
    time::{Instant, sleep, timeout},
};

const CONTROL_BODY_SLOTS: usize = 32;
const PROCESS_DEADLINE: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(20);

struct ApiProcess {
    child: Child,
    log: PathBuf,
}

impl ApiProcess {
    fn stderr(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    async fn listening(&mut self) -> Result<SocketAddrV4> {
        let deadline = Instant::now() + PROCESS_DEADLINE;
        loop {
            if let Some(status) = self.child.try_wait()? {
                anyhow::bail!("daemon exited with {status}: {}", self.stderr());
            }
            if let Some(authority) = self
                .stderr()
                .lines()
                .find_map(|line| line.strip_prefix("pVisor sandbox service listening on "))
            {
                let authority: SocketAddrV4 = authority.parse()?;
                ensure!(*authority.ip() == Ipv4Addr::LOCALHOST, "not loopback");
                return Ok(authority);
            }
            ensure!(Instant::now() < deadline, "no listener: {}", self.stderr());
            sleep(POLL).await;
        }
    }

    async fn exited(&mut self) -> Result<ExitStatus> {
        timeout(PROCESS_DEADLINE, self.child.wait())
            .await
            .with_context(|| format!("daemon stayed alive: {}", self.stderr()))?
            .map_err(Into::into)
    }

    async fn stop(&mut self) -> Result<()> {
        let pid = self.child.id().context("daemon already exited")?;
        // The direct child has not been reaped, so its PID cannot be reused.
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) }, 0);
        let status = self.exited().await?;
        assert!(status.success(), "{status}: {}", self.stderr());
        Ok(())
    }
}

struct ReadOnly<'a>(&'a Path);

impl<'a> ReadOnly<'a> {
    fn apply(path: &'a Path) -> Result<Self> {
        fs::set_permissions(path, Permissions::from_mode(0o500))?;
        Ok(Self(path))
    }
}

impl Drop for ReadOnly<'_> {
    fn drop(&mut self) {
        let _ = fs::set_permissions(self.0, Permissions::from_mode(0o700));
    }
}

struct Campaign {
    root: TempDir,
    cgroup: PathBuf,
    key: String,
    http: Client,
}

impl Campaign {
    fn new() -> Result<Self> {
        ensure!(
            unsafe { libc::geteuid() } != 0,
            "run as an unprivileged user"
        );
        let cgroup = PathBuf::from(
            std::env::var_os("PVISOR_DAEMON_CHAOS_CGROUP")
                .context("set PVISOR_DAEMON_CHAOS_CGROUP to a fresh delegated cgroup v2 root")?,
        )
        .canonicalize()?;
        let root = tempfile::tempdir()?;
        fs::create_dir(root.path().join("images"))?;
        Ok(Self {
            root,
            cgroup,
            key: format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4()),
            http: Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(3))
                .build()?,
        })
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("state")
    }

    fn start(&self, phase: &str) -> Result<ApiProcess> {
        let log = self.root.path().join(format!("{phase}.stderr"));
        let child = Command::new(env!("CARGO_BIN_EXE_pvisor-daemon"))
            .args(["serve", "--sandbox", "--listen", "127.0.0.1:0"])
            .args(["--max-sandboxes", "1"])
            .arg("--images-dir")
            .arg(self.root.path().join("images"))
            .arg("--cgroup-root")
            .arg(&self.cgroup)
            .arg("--state")
            .arg(self.state())
            .env("OPEN_SANDBOX_API_KEY", &self.key)
            .env("RUST_LIB_BACKTRACE", "0")
            .stdout(Stdio::null())
            .stderr(File::create(&log)?)
            .kill_on_drop(true)
            .spawn()?;
        Ok(ApiProcess { child, log })
    }

    fn url(authority: SocketAddrV4) -> String {
        format!("http://{authority}/v1/sandboxes")
    }

    async fn request(
        &self,
        authority: SocketAddrV4,
        method: Method,
        body: Option<Value>,
    ) -> Result<(StatusCode, Value)> {
        let mut request = self
            .http
            .request(method, Self::url(authority))
            .header("OPEN-SANDBOX-API-KEY", &self.key);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        Ok((response.status(), response.json().await?))
    }

    async fn assert_empty(&self, authority: SocketAddrV4) -> Result<()> {
        let (status, body) = self.request(authority, Method::GET, None).await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["items"], json!([]));
        Ok(())
    }

    async fn invalid_create(
        &self,
        authority: SocketAddrV4,
        status: StatusCode,
        code: &str,
    ) -> Result<()> {
        let request = json!({
            "image": {"uri": "missing-fixture"},
            "entrypoint": ["/bin/true"],
            "resourceLimits": {"cpu": "1", "memory": "64Mi"},
        });
        let (actual, body) = self.request(authority, Method::POST, Some(request)).await?;
        assert_eq!(actual, status, "{body}");
        assert_eq!(body["code"], code);
        Ok(())
    }

    async fn begin_incomplete_create(&self, authority: SocketAddrV4) -> Result<TcpStream> {
        timeout(Duration::from_secs(3), async {
            let mut stream = BufReader::new(TcpStream::connect(authority).await?);
            let headers = format!(
                "POST /v1/sandboxes HTTP/1.1\r\nHost: {authority}\r\nOPEN-SANDBOX-API-KEY: {}\r\nContent-Type: application/json\r\nContent-Length: 64\r\nExpect: 100-continue\r\n\r\n",
                self.key,
            );
            stream.write_all(headers.as_bytes()).await?;
            // The interim response proves the server is polling this request body.
            let mut line = String::new();
            stream.read_line(&mut line).await?;
            assert_eq!(line, "HTTP/1.1 100 Continue\r\n");
            stream.write_all(b"{").await?;
            Ok(stream.into_inner())
        })
        .await
        .context("partial create did not reach body buffering")?
    }
}

fn durable_empty(state: &Path) -> Result<[Vec<u8>; 3]> {
    let records = state.join("records");
    let entries = fs::read_dir(&records)?.collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(entries.len(), 1, "registry leaked records: {entries:?}");
    assert_eq!(entries[0].file_name(), "meta.json");
    Ok([
        fs::read(state.join("owner.json"))?,
        fs::read(state.join("sandboxes.json"))?,
        fs::read(records.join("meta.json"))?,
    ])
}

#[tokio::test]
#[ignore = "requires Linux x86-64 KVM and an unprivileged delegated cgroup v2 root"]
async fn api_restarts_after_storage_failure_and_sigkill_and_refuses_corruption() -> Result<()> {
    let campaign = Campaign::new()?;
    let state = campaign.state();
    let mut daemon = campaign.start("before-write-failure")?;
    let address = daemon.listening().await?;
    campaign.assert_empty(address).await?;
    let initial = durable_empty(&state)?;

    {
        let records = state.join("records");
        let _read_only = ReadOnly::apply(&records)?;
        campaign
            .invalid_create(
                address,
                StatusCode::SERVICE_UNAVAILABLE,
                "STORAGE_UNAVAILABLE",
            )
            .await?;
        let status = daemon.exited().await?;
        assert!(status.code().is_some_and(|code| code != 0));
        assert!(
            daemon.stderr().contains("sandbox maintenance stopped:"),
            "{}",
            daemon.stderr()
        );
        assert_eq!(durable_empty(&state)?, initial);
    }

    let mut daemon = campaign.start("after-write-failure")?;
    let address = daemon.listening().await?;
    campaign.assert_empty(address).await?;
    for _ in 0..3 {
        campaign
            .invalid_create(
                address,
                StatusCode::INTERNAL_SERVER_ERROR,
                "SANDBOX_CREATE_FAILED",
            )
            .await?;
    }
    assert_eq!(durable_empty(&state)?, initial);
    let incomplete = futures_util::future::try_join_all(
        (0..CONTROL_BODY_SLOTS).map(|_| campaign.begin_incomplete_create(address)),
    )
    .await?;
    campaign
        .invalid_create(
            address,
            StatusCode::TOO_MANY_REQUESTS,
            "CONTROL_BODY_CAPACITY_EXCEEDED",
        )
        .await?;
    campaign.assert_empty(address).await?;
    daemon.child.start_kill()?;
    assert_eq!(daemon.exited().await?.signal(), Some(libc::SIGKILL));
    drop(incomplete);

    let mut daemon = campaign.start("after-sigkill")?;
    let address = daemon.listening().await?;
    campaign.assert_empty(address).await?;
    campaign
        .invalid_create(
            address,
            StatusCode::INTERNAL_SERVER_ERROR,
            "SANDBOX_CREATE_FAILED",
        )
        .await?;
    assert_eq!(durable_empty(&state)?, initial);
    daemon.stop().await?;

    // Damage the existing header while the owner is down; it must not reset state.
    let header = state.join("sandboxes.json");
    let corrupt = &initial[1][..initial[1].len() / 2];
    fs::write(&header, corrupt)?;
    let mut rejected = campaign.start("corrupt-header")?;
    let status = rejected.exited().await?;
    assert!(status.code().is_some_and(|code| code != 0));
    let stderr = rejected.stderr();
    assert!(
        stderr.contains("invalid registry; refusing to forget native ownership"),
        "{stderr}"
    );
    assert!(!stderr.contains("sandbox service listening on"));
    assert_eq!(
        durable_empty(&state)?,
        [initial[0].clone(), corrupt.to_vec(), initial[2].clone()]
    );

    fs::write(&header, &initial[1])?;
    let mut daemon = campaign.start("after-header-repair")?;
    let address = daemon.listening().await?;
    campaign.assert_empty(address).await?;
    daemon.stop().await?;
    assert_eq!(durable_empty(&state)?, initial);
    Ok(())
}
