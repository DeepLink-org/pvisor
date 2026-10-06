//! Local, persistent rootless Podman containers for the OpenSandbox daemon.
//!
//! Prepared-image contract (not an execd replacement): the image's ENTRYPOINT
//! must supervise the real OpenSandbox execd, real egress service and workload.
//! `RuntimeSpec::entrypoint` replaces image CMD and is passed to that ENTRYPOINT
//! as separate arguments; an empty vector preserves image CMD. The supervisor
//! must keep both services alive, forward signals and reap children. Execd must
//! listen on container TCP 44772 and egress on TCP 18080 (not container loopback).
//! Images must be provisioned locally; this backend never pulls them. Image
//! volumes are ignored and no host executable, directory or socket is mounted.
//!
//! Upstream inspected at b1a29cf93a823a95913f7943010febb3f29de05c in
//! <https://github.com/opensandbox-group/OpenSandbox>: server service
//! `server/opensandbox_server/services/docker/runtime.py` distributes `/execd`
//! and `/bootstrap.sh` to `/opt/opensandbox`. `components/execd/bootstrap.sh`
//! starts execd without arguments in ordinary mode; EXECD_INIT enables
//! `execd --init -- <workload argv>`. Lifecycle mode additionally passes
//! `--lifecycle-startup-status-file`. `components/execd/pkg/flag/parser.go`
//! defaults to port 44772. Bootstrap does NOT start egress. This module neither
//! injects bootstrap nor joins workload argv into a shell command. Images must
//! complete execd initialization themselves: no /internal/init is sent here.
//!
//! IMPORTANT: at that SHA the default egress sidecar unconditionally installs
//! iptables redirects, even in DNS-only mode (`components/egress/main.go`). It
//! cannot run under this backend's cap-drop=ALL/no-new-privileges contract.
//! A prepared image must provide a genuine capability-free egress deployment;
//! an unmodified default sidecar is unsupported and creation fails closed.
//! Health checks do not prove transparent egress enforcement. This backend
//! provides neither network-policy enforcement nor a VM-grade security boundary.
//! Rootless slirp4netns disables host-loopback access, but outbound network/LAN
//! access is not a deny-all policy. Do not claim stock sidecar compatibility.
//!
//! The daemon must generate fresh canonical `sb-<uuid>` IDs (not client names),
//! persist its stable owner string AND sandbox intent before calling create,
//! and retain that intent on unknown failures/cancellation until reconciliation
//! confirms Missing. CreateError distinguishes confirmed attempt cleanup from
//! unknown state; errors before create begins never authorize deleting existing
//! sandbox records. Labels/Podman state survive restarts; there is no volatile
//! container registry to discard. Never run another manager using the same IDs.
//! Podman, its config/hooks, the image and host account are trusted; labels are
//! not a boundary against other processes running as the same host user.
//!
//! Running returned by inspect means Podman running state plus current service
//! readiness. endpoint only verifies running state, ownership and loopback port
//! mapping; it is not a health check and services may fail after resolution.
//! Unready-but-running is an error, not Stopped or Missing. Pause is cgroup
//! freeze, resume is unfreeze (not restart), and delete removes the container's
//! writable layer. No checkpoint, TTL, proxy, SSE handling or command execution
//! is implemented here: the upper layer must proxy the official execd API via
//! endpoint(44772), preserving its authentication, streams and command IDs.
//! Endpoints are loopback HTTP base URLs, only for ports 44772 and 18080. Local
//! untrusted users may reach published ports; configure real service tokens in
//! the prepared image/spec and enforce daemon API authentication separately.
//!
//! Locks serialize operations only within a sandbox, with weak entries reclaimed
//! on completion/cancellation. The daemon also needs its own per-sandbox gate;
//! runtime locks do not cover reservation/records or fencing across processes.
//! Preflight caches success for 30s with no shared lock across await. A backend
//! capability change can be detected up to 30s later; subprocess failures are
//! still errors and resource checks are not skipped during create.
//!
//! --env receives KEY only. Values enter only the create child's environment,
//! never process-global env or subsequent control invocations. Host-sensitive
//! keys (PATH/HOME, loader, Podman/config/helper variables) are rejected by
//! validate_spec; configure those workload settings in the prepared image.
//! Same-UID/root processes and Podman metadata can still expose container env.
//! Host configs/hooks remain trusted, including their environment handling.
//! Backend stderr and untrusted JSON values are withheld from returned errors.
//!
//! Integration requires `pub mod runtime`, `async-trait`, and Tokio `process`
//! and `io-util` features in the owning crate. This file has no pvisor dependency.

use std::{
    collections::BTreeMap,
    fmt,
    io::Read,
    path::PathBuf,
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex as StdMutex, Weak},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::{Mutex, OwnedMutexGuard},
    time::{sleep, timeout},
};

const OWNER_LABEL: &str = "io.pvisor.daemon.owner";
const SANDBOX_LABEL: &str = "io.pvisor.daemon.sandbox";
const ATTEMPT_LABEL: &str = "io.pvisor.daemon.create-attempt";
const EXECD_PORT: u16 = 44772;
const EGRESS_PORT: u16 = 18080;
const PIDS_LIMIT: u64 = 512;
const OUTPUT_LIMIT: usize = 256 * 1024;
const HTTP_LIMIT: usize = 16 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
const CREATE_TIMEOUT: Duration = Duration::from_secs(90);
const READY_TIMEOUT: Duration = Duration::from_secs(60);
const PREFLIGHT_TTL: Duration = Duration::from_secs(30);

#[async_trait::async_trait]
pub trait Runtime: Send + Sync {
    async fn preflight(&self) -> Result<()> {
        Ok(())
    }

    async fn create(&self, spec: &RuntimeSpec) -> Result<()>;
    async fn inspect(&self, id: &str) -> Result<RuntimeState>;
    async fn pause(&self, id: &str) -> Result<()>;
    async fn resume(&self, id: &str) -> Result<()>;
    async fn delete(&self, id: &str) -> Result<()>;
    async fn endpoint(&self, id: &str, port: u16) -> Result<String>;
}

#[derive(Clone)]
pub struct RuntimeSpec {
    pub id: String,
    pub image: String,
    pub entrypoint: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// CPU quota in thousandths of one CPU, not CPU shares.
    pub cpu_millis: u64,
    /// Hard memory limit; memory+swap equals this, disabling additional swap.
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeState {
    Running,
    Paused,
    Stopped,
    Missing,
}

/// Failure after a create invocation began. `false` means this attempt's
/// container was confirmed absent after cleanup; `true` requires reconciliation
/// (including possible late persistence after interruption). Cancellation has
/// no returned error and must always be reconciled by the daemon.
#[derive(Debug)]
pub struct CreateError {
    requires_reconciliation: bool,
    message: String,
}

impl CreateError {
    pub fn requires_reconciliation(&self) -> bool {
        self.requires_reconciliation
    }

    fn after_cleanup(error: anyhow::Error, cleanup: Result<()>, interrupted: bool) -> Self {
        let requires_reconciliation = interrupted || cleanup.is_err();
        let message = match cleanup {
            Ok(()) if interrupted => format!(
                "creation failed: {error:#}; container currently absent, but interrupted create may persist late; retain intent and reconcile"
            ),
            Ok(()) => format!(
                "creation failed: {error:#}; this attempt's container confirmed Missing after cleanup"
            ),
            Err(cleanup) => format!(
                "creation failed: {error:#}; cleanup failed: {cleanup:#}; retain intent and reconcile"
            ),
        };
        Self {
            requires_reconciliation,
            message,
        }
    }
}

impl fmt::Display for CreateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CreateError {}

#[derive(Debug)]
struct CommandInterrupted(String);

impl fmt::Display for CommandInterrupted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for CommandInterrupted {}

type SandboxLockEntries = StdMutex<BTreeMap<String, Weak<SandboxKey>>>;

#[derive(Default)]
struct SandboxLocks {
    entries: Arc<SandboxLockEntries>,
}

impl SandboxLocks {
    fn lease(&self, id: &str) -> Result<SandboxLease> {
        container_name(id)?;
        let key = {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            match entries.get(id).and_then(Weak::upgrade) {
                Some(key) => key,
                None => {
                    let key = Arc::new(SandboxKey {
                        id: id.to_owned(),
                        lock: Arc::new(Mutex::new(())),
                        entries: Arc::downgrade(&self.entries),
                    });
                    entries.insert(id.to_owned(), Arc::downgrade(&key));
                    key
                }
            }
        };
        Ok(SandboxLease { key, guard: None })
    }

    async fn lock(&self, id: &str) -> Result<SandboxLease> {
        let mut lease = self.lease(id)?;
        // A registration exists before awaiting. It is reclaimed even if
        // cancellation drops the lock future and registration in either order.
        lease.guard = Some(Arc::clone(&lease.key.lock).lock_owned().await);
        Ok(lease)
    }
}

struct SandboxKey {
    id: String,
    lock: Arc<Mutex<()>>,
    entries: Weak<SandboxLockEntries>,
}

impl Drop for SandboxKey {
    fn drop(&mut self) {
        if let Some(registry) = self.entries.upgrade() {
            let mut entries = registry.lock().unwrap_or_else(|e| e.into_inner());
            // A new registration may have replaced our expired weak entry
            // while this destructor waited. Never remove that replacement.
            if entries
                .get(&self.id)
                .is_some_and(|entry| std::ptr::eq(entry.as_ptr(), self))
            {
                entries.remove(&self.id);
            }
        }
    }
}

struct SandboxLease {
    key: Arc<SandboxKey>,
    guard: Option<OwnedMutexGuard<()>>,
}

impl Drop for SandboxLease {
    fn drop(&mut self) {
        drop(self.guard.take());
        // The registration drops after the operation's guard is released.
    }
}

pub struct PodmanRuntime {
    binary: PathBuf,
    owner: String,
    // One runtime instance per daemon. Cross-process fencing belongs to daemon.
    operations: SandboxLocks,
    preflight_success: StdMutex<Option<Instant>>,
}

impl PodmanRuntime {
    /// `binary` must be an absolute path to trusted local Podman, and `owner`
    /// must be a nonempty stable value persisted by the daemon, not a PID.
    /// No PATH lookup or default binary is provided. The caller must require an
    /// explicit absolute --podman option and call preflight at startup.
    pub fn new(binary: PathBuf, owner: String) -> Result<Self> {
        ensure!(
            binary.is_absolute(),
            "Podman binary must be an absolute trusted path"
        );
        validate_owner(&owner)?;
        Ok(Self {
            binary,
            owner,
            operations: SandboxLocks::default(),
            preflight_success: StdMutex::new(None),
        })
    }

    async fn command(&self, args: &[String], limit: Duration) -> Result<CommandOutput> {
        self.command_with_env(args, None, limit).await
    }

    fn subprocess(
        &self,
        args: &[String],
        create_env: Option<&BTreeMap<String, String>>,
    ) -> Result<Command> {
        ensure!(
            self.binary.is_absolute(),
            "Podman binary must be an absolute trusted path"
        );
        validate_owner(&self.owner)?;
        let operation = args.first().map(String::as_str).unwrap_or("unknown");
        let mut command = Command::new(&self.binary);
        if let Some(env) = create_env {
            ensure!(
                operation == "create",
                "sandbox environment is restricted to create invocation"
            );
            command.envs(env);
        }
        command
            .arg("--remote=false")
            .args(args)
            .env_remove("CONTAINER_HOST")
            .env_remove("CONTAINER_CONNECTION")
            .env_remove("DOCKER_HOST")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        Ok(command)
    }

    async fn command_with_env(
        &self,
        args: &[String],
        create_env: Option<&BTreeMap<String, String>>,
        limit: Duration,
    ) -> Result<CommandOutput> {
        let operation = args.first().map(String::as_str).unwrap_or("unknown");
        let mut command = self.subprocess(args, create_env)?;
        let mut child = command
            .spawn()
            .with_context(|| format!("spawn local Podman {operation}"))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            CommandInterrupted(
                "missing Podman stdout pipe after spawn; persisted state may remain".into(),
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            CommandInterrupted(
                "missing Podman stderr pipe after spawn; persisted state may remain".into(),
            )
        })?;
        let result = timeout(limit, async {
            let (status, stdout, stderr) =
                tokio::try_join!(child.wait(), read_bounded(stdout), read_bounded(stderr))?;
            Ok::<_, std::io::Error>(CommandOutput {
                status,
                stdout,
                _stderr: stderr,
            })
        })
        .await;
        match result {
            Ok(Ok(output)) => Ok(output),
            failed => {
                // Killing the CLI is not rollback: Podman/conmon may already
                // have persisted a container. The caller must reconcile labels.
                let _ = child.start_kill();
                let reaped = timeout(Duration::from_secs(2), child.wait()).await;
                let detail = match failed {
                    Ok(Err(_)) => "subprocess I/O failed or output exceeded limit".to_owned(),
                    Err(_) => format!("timed out after {}s", limit.as_secs()),
                    Ok(Ok(_)) => unreachable!(),
                };
                return Err(CommandInterrupted(format!(
                    "Podman {operation}: {detail}; kill/reap completed={}; persisted state may remain",
                    matches!(reaped, Ok(Ok(_)))
                )).into());
            }
        }
    }

    async fn checked(&self, args: Vec<String>, limit: Duration) -> Result<Vec<u8>> {
        let output = self.command(&args, limit).await?;
        output.require_success(args.first().map(String::as_str).unwrap_or("unknown"))?;
        Ok(output.stdout)
    }

    /// Validate local rootless/resource support, caching only successful probes
    /// for 30 seconds. No shared lock is held across a subprocess or await.
    /// Concurrent cold/expired calls may issue duplicate info probes; startup
    /// preflight warms the cache without serializing unrelated sandbox work.
    pub async fn preflight(&self) -> Result<()> {
        let started = Instant::now();
        {
            let cached = self
                .preflight_success
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if cached.is_some_and(|checked| checked.elapsed() < PREFLIGHT_TTL) {
                return Ok(());
            }
        }
        let bytes = self
            .checked(strings(&["info", "--format=json"]), COMMAND_TIMEOUT)
            .await?;
        let info =
            serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid Podman info JSON"))?;
        validate_info(&info)?;
        let mut cached = self
            .preflight_success
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // An older concurrent probe cannot extend or overwrite a newer success.
        if cached.is_none_or(|checked| checked < started) {
            *cached = Some(started);
        }
        Ok(())
    }

    async fn exists(&self, name: &str) -> Result<bool> {
        let output = self
            .command(
                &strings(&["container", "exists", "--", name]),
                COMMAND_TIMEOUT,
            )
            .await?;
        match output.status.code() {
            Some(0) => Ok(true),
            // This is Podman's documented absence code; arbitrary inspect
            // failures / backend failures are NEVER translated to Missing.
            Some(1) => Ok(false),
            _ => {
                output.require_success("container exists")?;
                bail!("unexpected existence status")
            }
        }
    }

    async fn lookup(&self, id: &str) -> Result<Option<Container>> {
        let name = container_name(id)?;
        if !self.exists(&name).await? {
            return Ok(None);
        }
        let output = self
            .command(
                &strings(&["container", "inspect", "--", &name]),
                COMMAND_TIMEOUT,
            )
            .await?;
        if !output.status.success() {
            if !self.exists(&name).await? {
                return Ok(None);
            }
            output.require_success("container inspect")?;
        }
        let container = parse_container(&output.stdout)?;
        container.check_owner(&self.owner, id)?;
        Ok(Some(container))
    }

    async fn required(&self, id: &str) -> Result<Container> {
        self.lookup(id)
            .await?
            .with_context(|| format!("sandbox {id} is Missing"))
    }

    async fn probe_services(&self, container: &Container) -> Result<()> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(1))
            .timeout(Duration::from_secs(3))
            .build()
            .context("build readiness HTTP client")?;
        let execd = container.endpoint(EXECD_PORT)?;
        let egress = container.endpoint(EGRESS_PORT)?;
        // The upstream /ping really returns an empty HTTP 200, not JSON.
        let ping = probe(&client, &format!("{execd}/ping")).await?;
        ensure!(ping.is_empty(), "unexpected execd /ping body");
        let ready = probe(&client, &format!("{execd}/ready")).await?;
        validate_ready(&ready)?;
        let health = probe(&client, &format!("{egress}/healthz")).await?;
        ensure!(health == b"ok", "unexpected egress /healthz body");
        Ok(())
    }

    async fn wait_ready(&self, id: &str, cid: &str) -> Result<()> {
        let mut last_health_error = None;
        let readiness = timeout(READY_TIMEOUT, async {
            loop {
                let container = self.required(id).await?;
                ensure!(
                    container.cid == cid,
                    "container identity changed during startup"
                );
                ensure!(
                    container.state == RuntimeState::Running,
                    "sandbox {id} stopped/paused during startup ({:?})",
                    container.state
                );
                match self.probe_services(&container).await {
                    Ok(()) => return Ok(()),
                    Err(error) => {
                        last_health_error = Some(error);
                        sleep(Duration::from_millis(250)).await;
                    }
                }
            }
        })
        .await;
        match readiness {
            Ok(result) => result,
            Err(_) => {
                Err(last_health_error.unwrap_or_else(|| anyhow!("no service probe completed")))
                    .context("execd/egress readiness deadline expired")
            }
        }
    }

    async fn remove(&self, container: &Container) -> Result<()> {
        let result = self
            .checked(
                strings(&["rm", "--force", "--time=2", "--", &container.cid]),
                COMMAND_TIMEOUT,
            )
            .await;
        // An external deletion is harmless, but any other failure is retained.
        if let Err(error) = result {
            if self.exists(&container.cid).await? {
                return Err(error).context(
                    "container retained; retry delete/reconcile before forgetting sandbox",
                );
            }
        }
        ensure!(
            !self.exists(&container.cid).await?,
            "Podman rm returned success but container remains"
        );
        Ok(())
    }

    async fn cleanup_attempt(&self, id: &str, attempt: &str) -> Result<()> {
        if let Some(container) = self.lookup(id).await? {
            ensure!(
                container.labels.get(ATTEMPT_LABEL).map(String::as_str) == Some(attempt),
                "refusing cleanup of a different creation attempt; container retained"
            );
            self.remove(&container).await?;
        }
        ensure!(
            self.lookup(id).await?.is_none(),
            "sandbox name still present after attempt cleanup; reconciliation required"
        );
        Ok(())
    }
}

#[async_trait::async_trait]
impl Runtime for PodmanRuntime {
    async fn preflight(&self) -> Result<()> {
        PodmanRuntime::preflight(self).await
    }

    async fn create(&self, spec: &RuntimeSpec) -> Result<()> {
        validate_spec(spec)?;
        let _guard = self.operations.lock(&spec.id).await?;
        validate_owner(&self.owner)?;
        self.preflight().await?;
        ensure!(
            self.lookup(&spec.id).await?.is_none(),
            "sandbox already exists; reconcile instead of replacing it"
        );
        let attempt = new_attempt()?;
        let mut create_interrupted = false;
        let result = async {
            // Create/start split provides an immutable identity before launch.
            // No --rm/--replace: exited or failed containers remain recoverable.
            let args = create_args(spec, &self.owner, &attempt)?;
            let output = self
                .command_with_env(&args, Some(&spec.env), CREATE_TIMEOUT)
                .await
                .map_err(|error| {
                    create_interrupted = error.downcast_ref::<CommandInterrupted>().is_some();
                    error
                })?;
            output.require_success("create")?;
            let bytes = output.stdout;
            let cid = std::str::from_utf8(&bytes)
                .context("Podman create output is not UTF-8")?
                .trim();
            validate_cid(cid)?;
            let container = self.required(&spec.id).await?;
            ensure!(
                container.cid == cid,
                "Podman create/inspect identity mismatch"
            );
            ensure!(
                container.labels.get(ATTEMPT_LABEL).map(String::as_str) == Some(attempt.as_str()),
                "Podman creation attempt mismatch"
            );
            validate_resources(&container.document, spec)?;
            self.checked(strings(&["start", "--", cid]), COMMAND_TIMEOUT)
                .await?;
            self.wait_ready(&spec.id, cid).await
        }
        .await;
        if let Err(error) = result {
            let cleanup = self.cleanup_attempt(&spec.id, &attempt).await;
            return Err(CreateError::after_cleanup(error, cleanup, create_interrupted).into());
        }
        Ok(())
    }

    async fn inspect(&self, id: &str) -> Result<RuntimeState> {
        let _guard = self.operations.lock(id).await?;
        self.preflight().await?;
        let Some(container) = self.lookup(id).await? else {
            return Ok(RuntimeState::Missing);
        };
        if container.state == RuntimeState::Running {
            self.probe_services(&container)
                .await
                .context("container is running but execd/egress is not ready")?;
        }
        Ok(container.state)
    }

    async fn pause(&self, id: &str) -> Result<()> {
        let _guard = self.operations.lock(id).await?;
        self.preflight().await?;
        let container = self.required(id).await?;
        match container.state {
            RuntimeState::Paused => return Ok(()),
            RuntimeState::Running => {}
            _ => bail!("only a running sandbox can be paused"),
        }
        self.checked(strings(&["pause", "--", &container.cid]), COMMAND_TIMEOUT)
            .await?;
        let current = self.required(id).await?;
        ensure!(
            current.cid == container.cid && current.state == RuntimeState::Paused,
            "pause not confirmed"
        );
        Ok(())
    }

    async fn resume(&self, id: &str) -> Result<()> {
        let _guard = self.operations.lock(id).await?;
        self.preflight().await?;
        let container = self.required(id).await?;
        match container.state {
            RuntimeState::Paused => {
                self.checked(strings(&["unpause", "--", &container.cid]), COMMAND_TIMEOUT)
                    .await?;
            }
            RuntimeState::Running => {}
            _ => bail!("resume only unfreezes a paused sandbox; it does not restart a stopped one"),
        }
        // A failed readiness check must not delete a successfully unfrozen container.
        self.wait_ready(id, &container.cid)
            .await
            .context("resume readiness failed; container retained")
    }

    async fn delete(&self, id: &str) -> Result<()> {
        let _guard = self.operations.lock(id).await?;
        self.preflight().await?;
        if let Some(container) = self.lookup(id).await? {
            self.remove(&container).await?;
        }
        // Check the name too: a replacement must never be deleted by stale CID.
        ensure!(
            self.lookup(id).await?.is_none(),
            "sandbox name is still present; retain daemon state"
        );
        Ok(())
    }

    async fn endpoint(&self, id: &str, port: u16) -> Result<String> {
        let _guard = self.operations.lock(id).await?;
        ensure!(
            matches!(port, EXECD_PORT | EGRESS_PORT),
            "only execd/egress service endpoints are supported"
        );
        self.preflight().await?;
        let container = self.required(id).await?;
        ensure!(
            container.state == RuntimeState::Running,
            "sandbox is not running"
        );
        // Resolve current identity/ownership and mapping, not HTTP health on
        // every proxied request. Actual readiness is checked by create/inspect.
        container.endpoint(port)
    }
}

struct CommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    // Drain/bound stderr but never place it (or parsed values) in API errors.
    _stderr: Vec<u8>,
}

impl CommandOutput {
    fn require_success(&self, operation: &str) -> Result<()> {
        ensure!(
            self.status.success(),
            "Podman {operation} failed ({}); backend stderr withheld",
            self.status
        );
        Ok(())
    }
}

async fn read_bounded(mut stream: impl AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            return Ok(bytes);
        }
        if bytes.len() + count > OUTPUT_LIMIT {
            return Err(std::io::Error::other(
                "Podman output exceeded per-stream limit",
            ));
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

async fn probe(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let mut response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("probe {url}"))?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "probe {url}: HTTP {}",
        response.status()
    );
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            body.len() + chunk.len() <= HTTP_LIMIT,
            "health response exceeds limit"
        );
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn validate_ready(bytes: &[u8]) -> Result<()> {
    let ready: Value =
        serde_json::from_slice(bytes).map_err(|_| anyhow!("invalid execd readiness JSON"))?;
    ensure!(
        ready.get("initialized").and_then(Value::as_bool) == Some(true),
        "execd initialization is incomplete"
    );
    Ok(())
}

fn validate_owner(owner: &str) -> Result<()> {
    ensure!(
        !owner.is_empty() && owner.len() <= 256 && !owner.chars().any(char::is_control),
        "owner must be a stable nonempty label value without control characters (max 256 bytes)"
    );
    Ok(())
}

fn container_name(id: &str) -> Result<String> {
    let uuid = id
        .strip_prefix("sb-")
        .context("sandbox ID must start with sb-")?;
    ensure!(
        uuid.len() == 36
            && uuid.bytes().enumerate().all(|(i, b)| {
                if matches!(i, 8 | 13 | 18 | 23) {
                    b == b'-'
                } else {
                    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
                }
            }),
        "sandbox ID must contain a canonical lowercase UUID"
    );
    ensure!(
        uuid.bytes().any(|b| b != b'0' && b != b'-'),
        "nil sandbox UUID is forbidden"
    );
    Ok(format!("pvisor-{id}"))
}

fn validate_cid(cid: &str) -> Result<()> {
    ensure!(
        cid.len() == 64
            && cid
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "expected full immutable Podman container ID"
    );
    Ok(())
}

/// Pure validation for use before daemon admission/reservation. Does not run
/// Podman or inspect the host. Environment keys that can configure the host
/// Podman/loader/helpers are reserved because KEY-only passing uses child env.
pub fn validate_spec(spec: &RuntimeSpec) -> Result<()> {
    container_name(&spec.id)?;
    ensure!(
        !spec.image.is_empty()
            && !spec.image.starts_with('-')
            && !spec
                .image
                .chars()
                .any(|c| c.is_whitespace() || c.is_control()),
        "invalid image reference"
    );
    ensure!(
        spec.cpu_millis > 0 && spec.cpu_millis <= i64::MAX as u64 / 100,
        "CPU quota must be positive and representable"
    );
    ensure!(
        spec.memory_bytes >= 6 * 1024 * 1024 && spec.memory_bytes <= i64::MAX as u64,
        "memory limit must be between 6 MiB and i64::MAX bytes"
    );
    if let Some(first) = spec.entrypoint.first() {
        ensure!(!first.is_empty(), "empty workload executable");
    }
    let mut size = spec.image.len();
    for arg in &spec.entrypoint {
        ensure!(!arg.contains('\0'), "workload argv contains NUL");
        size = size
            .checked_add(arg.len() + 1)
            .context("argv size overflow")?;
    }
    for (key, value) in &spec.env {
        ensure!(
            !key.is_empty()
                && key.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                }),
            "invalid environment key"
        );
        ensure!(
            !reserved_child_env(key),
            "environment key is reserved for the host backend"
        );
        ensure!(!value.contains('\0'), "environment value contains NUL");
        size = size
            .checked_add(key.len() + value.len() + 2)
            .context("environment size overflow")?;
    }
    ensure!(
        size <= 64 * 1024,
        "workload argv/environment exceeds 64 KiB"
    );
    Ok(())
}

fn reserved_child_env(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    // Values are visible to Podman before --env imports them. Do not let a
    // sandbox request change host loading, storage, connections or helpers.
    matches!(
        key.as_str(),
        "PATH"
            | "HOME"
            | "USER"
            | "LOGNAME"
            | "SHELL"
            | "ENV"
            | "SHELLOPTS"
            | "TMPDIR"
            | "TMP"
            | "TEMP"
            | "PWD"
            | "OLDPWD"
            | "IFS"
            | "GCONV_PATH"
            | "LOCPATH"
            | "LANG"
            | "LANGUAGE"
            | "TZ"
            | "TZDIR"
            | "HTTP_PROXY"
            | "HTTPS_PROXY"
            | "ALL_PROXY"
            | "NO_PROXY"
            | "FTP_PROXY"
            | "SSL_CERT_FILE"
            | "SSL_CERT_DIR"
            | "REGISTRY_AUTH_FILE"
            | "STORAGE_DRIVER"
            | "STORAGE_OPTS"
            | "GODEBUG"
            | "GOMAXPROCS"
            | "GOGC"
            | "GOTRACEBACK"
            | "GOEXPERIMENT"
            | "LOCALDOMAIN"
            | "RES_OPTIONS"
            | "HOSTALIASES"
    ) || [
        "LD_",
        "DYLD_",
        "GLIBC_",
        "MALLOC_",
        "LC_",
        "CONTAINER",
        "_CONTAINER",
        "PODMAN_",
        "REGISTRY_",
        "REGISTRIES_",
        "STORAGE_",
        "SECCOMP_",
        "OCI_",
        "CNI_",
        "NETAVARK_",
        "AARDVARK_",
        "DOCKER_",
        "XDG_",
        "DBUS_",
        "SSH_",
        "SLIRP4NETNS_",
        "RUNC_",
        "CRUN_",
        "NOTIFY_",
        "LISTEN_",
        "PYTHON",
        "PERL",
        "RUBY",
        "NODE_",
        "BASH_",
        "ZSH_",
        "JAVA_",
        "_JAVA_",
        "JDK_",
        "GNUTLS_",
        "OPENSSL_",
    ]
    .iter()
    .any(|prefix| key.starts_with(*prefix))
}

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

fn create_args(spec: &RuntimeSpec, owner: &str, attempt: &str) -> Result<Vec<String>> {
    validate_spec(spec)?;
    validate_owner(owner)?;
    let mut args = strings(&[
        "create",
        "--name",
        &container_name(&spec.id)?,
        "--pull=never",
        "--image-volume=ignore",
        "--http-proxy=false",
        "--cgroups=enabled",
        "--cgroupns=private",
        "--pid=private",
        "--ipc=private",
        "--userns=private",
        "--network=slirp4netns:allow_host_loopback=false",
        "--privileged=false",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--restart=no",
        "--pids-limit",
        &PIDS_LIMIT.to_string(),
        "--cpus",
        &format!("{}.{:03}", spec.cpu_millis / 1000, spec.cpu_millis % 1000),
        "--memory",
        &spec.memory_bytes.to_string(),
        "--memory-swap",
        &spec.memory_bytes.to_string(),
        "--publish",
        "127.0.0.1::44772/tcp",
        "--publish",
        "127.0.0.1::18080/tcp",
        "--label",
        &format!("{OWNER_LABEL}={owner}"),
        "--label",
        &format!("{SANDBOX_LABEL}={}", spec.id),
        "--label",
        &format!("{ATTEMPT_LABEL}={attempt}"),
    ]);
    for key in spec.env.keys() {
        args.push("--env".to_owned());
        args.push(key.clone());
    }
    args.push("--".to_owned());
    args.push(spec.image.clone());
    args.extend(spec.entrypoint.iter().cloned());
    Ok(args)
}

fn new_attempt() -> Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .context("open creation nonce source")?
        .read_exact(&mut bytes)
        .context("read creation nonce")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn validate_info(info: &Value) -> Result<()> {
    let host = info.get("host").context("Podman info missing host")?;
    ensure!(
        host.pointer("/security/rootless").and_then(Value::as_bool) == Some(true),
        "local rootless Podman is required; no rootful/host fallback"
    );
    ensure!(
        host.get("cgroupVersion").and_then(Value::as_str) == Some("v2"),
        "cgroup v2 is required"
    );
    let manager = host.get("cgroupManager").and_then(Value::as_str);
    ensure!(
        matches!(manager, Some("systemd" | "cgroupfs")),
        "resource-capable cgroup manager is required"
    );
    let controllers = host
        .get("cgroupControllers")
        .and_then(Value::as_array)
        .context("Podman did not report delegated cgroup controllers")?;
    for required in ["cpu", "memory", "pids"] {
        ensure!(
            controllers
                .iter()
                .any(|value| value.as_str() == Some(required)),
            "rootless cgroup controller {required} is unavailable"
        );
    }
    Ok(())
}

fn validate_resources(document: &Value, spec: &RuntimeSpec) -> Result<()> {
    let config = document
        .get("HostConfig")
        .context("inspect missing HostConfig")?;
    ensure!(
        config.get("Memory").and_then(Value::as_u64) == Some(spec.memory_bytes)
            && config.get("MemorySwap").and_then(Value::as_u64) == Some(spec.memory_bytes)
            && config.get("PidsLimit").and_then(Value::as_u64) == Some(PIDS_LIMIT),
        "Podman did not retain requested memory/swap/PID limits"
    );
    let quota = config
        .get("CpuQuota")
        .and_then(Value::as_u64)
        .context("inspect missing CPU quota")?;
    let period = config
        .get("CpuPeriod")
        .and_then(Value::as_u64)
        .context("inspect missing CPU period")?;
    ensure!(
        quota > 0
            && period > 0
            && u128::from(quota) * 1000 == u128::from(period) * u128::from(spec.cpu_millis),
        "Podman did not retain requested CPU limit"
    );
    Ok(())
}

struct Container {
    cid: String,
    state: RuntimeState,
    labels: BTreeMap<String, String>,
    document: Value,
}

impl Container {
    fn check_owner(&self, owner: &str, id: &str) -> Result<()> {
        ensure!(
            self.labels.get(OWNER_LABEL).map(String::as_str) == Some(owner),
            "sandbox owner label mismatch; refusing access"
        );
        ensure!(
            self.labels.get(SANDBOX_LABEL).map(String::as_str) == Some(id),
            "sandbox identity label mismatch; refusing access"
        );
        Ok(())
    }

    fn endpoint(&self, port: u16) -> Result<String> {
        ensure!(
            matches!(port, EXECD_PORT | EGRESS_PORT),
            "unsupported service port"
        );
        let ports = self
            .document
            .pointer("/NetworkSettings/Ports")
            .context("inspect missing port mappings")?;
        let bindings = ports
            .get(format!("{port}/tcp"))
            .and_then(Value::as_array)
            .context("service port is not published")?;
        ensure!(
            bindings.len() == 1,
            "expected exactly one loopback service binding"
        );
        let binding = &bindings[0];
        ensure!(
            binding.get("HostIp").and_then(Value::as_str) == Some("127.0.0.1"),
            "refusing non-loopback published endpoint"
        );
        let host_port: u16 = binding
            .get("HostPort")
            .and_then(Value::as_str)
            .context("missing host port")?
            .parse()
            .context("invalid host port")?;
        ensure!(host_port != 0, "host port has not been allocated");
        Ok(format!("http://127.0.0.1:{host_port}"))
    }
}

fn parse_container(bytes: &[u8]) -> Result<Container> {
    let mut documents: Vec<Value> = serde_json::from_slice(bytes)
        .map_err(|_| anyhow!("invalid Podman container inspect JSON"))?;
    ensure!(
        documents.len() == 1,
        "expected exactly one inspected container"
    );
    let document = documents.remove(0);
    let cid = document
        .get("Id")
        .and_then(Value::as_str)
        .context("inspect missing container ID")?
        .to_owned();
    validate_cid(&cid)?;
    let labels = serde_json::from_value(
        document
            .pointer("/Config/Labels")
            .context("inspect missing labels")?
            .clone(),
    )
    .map_err(|_| anyhow!("invalid container labels"))?;
    let status = document
        .pointer("/State/Status")
        .and_then(Value::as_str)
        .context("inspect missing state")?;
    let state = match status {
        "paused" => RuntimeState::Paused,
        "running" => RuntimeState::Running,
        "created" | "configured" | "exited" | "stopped" => RuntimeState::Stopped,
        // Restarting/removing/dead/unknown states require reconciliation,
        // rather than a fabricated stable state.
        _ => bail!("Podman state is transitional or unsupported"),
    };
    Ok(Container {
        cid,
        state,
        labels,
        document,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "sb-6a069a75-82c2-4e05-a55b-07826d63a9e1";

    fn spec() -> RuntimeSpec {
        RuntimeSpec {
            id: ID.into(),
            image: "localhost/prepared:1.1.0".into(),
            entrypoint: vec![
                "/usr/bin/workload".into(),
                "a b; $(touch /host)".into(),
                "--privileged".into(),
            ],
            env: BTreeMap::from([("TEXT".into(), "x=y\n$(id)".into())]),
            cpu_millis: 1250,
            memory_bytes: 128 * 1024 * 1024,
        }
    }

    fn document(status: &str) -> Value {
        json!({
            "Id": "a".repeat(64), "State": {"Status": status},
            "Config": {"Labels": {(OWNER_LABEL): "owner", (SANDBOX_LABEL): ID, (ATTEMPT_LABEL): "attempt"}},
            "HostConfig": {"Memory": 134217728, "MemorySwap": 134217728,
                "PidsLimit": 512, "CpuQuota": 125000, "CpuPeriod": 100000},
            "NetworkSettings": {"Ports": {
                "44772/tcp": [{"HostIp": "127.0.0.1", "HostPort": "32100"}],
                "18080/tcp": [{"HostIp": "127.0.0.1", "HostPort": "32101"}]
            }}
        })
    }

    fn parse(value: &Value) -> Result<Container> {
        parse_container(&serde_json::to_vec(&vec![value]).unwrap())
    }

    #[test]
    fn weak_key_locks_share_only_same_sandbox_and_reclaim_waiters() {
        let locks = SandboxLocks::default();
        let mut first = locks.lease(ID).unwrap();
        let waiter = locks.lease(ID).unwrap();
        let other = locks
            .lease("sb-6a069a75-82c2-4e05-a55b-07826d63a9e2")
            .unwrap();
        assert!(Arc::ptr_eq(&first.key.lock, &waiter.key.lock));
        assert!(!Arc::ptr_eq(&first.key.lock, &other.key.lock));
        first.guard = Some(Arc::clone(&first.key.lock).try_lock_owned().unwrap());
        assert!(Arc::clone(&waiter.key.lock).try_lock_owned().is_err());
        assert!(Arc::clone(&other.key.lock).try_lock_owned().is_ok());
        drop(first);
        assert_eq!(locks.entries.lock().unwrap().len(), 2);
        assert!(Arc::clone(&waiter.key.lock).try_lock_owned().is_ok());
        // Dropping an unacquired lease exercises waiter-cancellation cleanup.
        drop(waiter);
        assert_eq!(locks.entries.lock().unwrap().len(), 1);
        drop(other);
        assert!(locks.entries.lock().unwrap().is_empty());
        let replacement = locks.lease(ID).unwrap();
        assert_eq!(locks.entries.lock().unwrap().len(), 1);
        // A cancelled lock future can retain a mutex Arc briefly after its
        // registration drops; that must not keep a dead weak key in the table.
        let cancelled_future_lock = Arc::clone(&replacement.key.lock);
        drop(replacement);
        assert!(locks.entries.lock().unwrap().is_empty());
        let next = locks.lease(ID).unwrap();
        assert!(!Arc::ptr_eq(&next.key.lock, &cancelled_future_lock));
        drop(next);
        drop(cancelled_future_lock);
        assert!(locks.entries.lock().unwrap().is_empty());
        assert!(locks.lease("invalid").is_err());
        assert!(locks.entries.lock().unwrap().is_empty());
    }

    #[test]
    fn only_unknown_create_failures_require_reconciliation() {
        let confirmed: anyhow::Error =
            CreateError::after_cleanup(anyhow!("services did not become ready"), Ok(()), false)
                .into();
        assert!(
            !confirmed
                .downcast_ref::<CreateError>()
                .unwrap()
                .requires_reconciliation()
        );
        let late_create = CreateError::after_cleanup(anyhow!("create timed out"), Ok(()), true);
        assert!(late_create.requires_reconciliation());
        let retained = CreateError::after_cleanup(
            anyhow!("startup failed"),
            Err(anyhow!("owned container could not be removed")),
            false,
        );
        assert!(retained.requires_reconciliation());
        let unreadable = CreateError::after_cleanup(
            anyhow!("startup failed"),
            Err(anyhow!("cleanup ownership/state unknown")),
            true,
        );
        assert!(unreadable.requires_reconciliation());
    }

    #[test]
    fn constructor_and_create_child_environment_are_explicit() {
        assert!(PodmanRuntime::new(PathBuf::from("podman"), "owner".into()).is_err());
        assert!(PodmanRuntime::new(PathBuf::from("/usr/bin/podman"), "".into()).is_err());
        let runtime = PodmanRuntime::new(PathBuf::from("/usr/bin/podman"), "owner".into()).unwrap();
        let value = spec();
        let args = create_args(&value, "owner", "attempt").unwrap();
        let create = runtime.subprocess(&args, Some(&value.env)).unwrap();
        assert!(
            create
                .as_std()
                .get_envs()
                .any(|(key, value)| key == std::ffi::OsStr::new("TEXT")
                    && value == Some(std::ffi::OsStr::new("x=y\n$(id)")))
        );
        assert!(
            !create
                .as_std()
                .get_args()
                .any(|arg| arg == std::ffi::OsStr::new("TEXT=x=y\n$(id)"))
        );
        let inspect = runtime
            .subprocess(&strings(&["container", "inspect", "--", ID]), None)
            .unwrap();
        assert!(
            !inspect
                .as_std()
                .get_envs()
                .any(|(key, _)| key == std::ffi::OsStr::new("TEXT"))
        );
        assert!(
            runtime
                .subprocess(&strings(&["start", "--", ID]), Some(&value.env))
                .is_err()
        );
    }

    #[test]
    fn host_backend_environment_keys_are_not_sandbox_inputs() {
        for key in [
            "LD_PRELOAD",
            "ld_library_path",
            "PATH",
            "HOME",
            "CONTAINERS_CONF",
            "CONTAINER_HOST",
            "DOCKER_HOST",
            "XDG_CONFIG_HOME",
            "REGISTRY_AUTH_FILE",
            "BASH_ENV",
            "PYTHONPATH",
            "GODEBUG",
            "HTTPS_PROXY",
        ] {
            let mut invalid = spec();
            invalid.env.insert(key.into(), "private-value".into());
            let error = validate_spec(&invalid).unwrap_err();
            assert!(!format!("{error:#}").contains("private-value"));
            assert!(create_args(&invalid, "owner", "attempt").is_err());
        }
        let mut valid = spec();
        valid
            .env
            .insert("EXECD_ACCESS_TOKEN".into(), "private-token".into());
        assert!(validate_spec(&valid).is_ok());
        let args = create_args(&valid, "owner", "attempt").unwrap();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--env", "EXECD_ACCESS_TOKEN"])
        );
        assert!(!args.iter().any(|arg| arg.contains("private-token")));
    }

    #[cfg(unix)]
    #[test]
    fn backend_errors_never_include_captured_stderr_or_json_values() {
        use std::os::unix::process::ExitStatusExt;
        let output = CommandOutput {
            status: ExitStatus::from_raw(125 << 8),
            stdout: Vec::new(),
            _stderr: b"backend diagnostic: private-value".to_vec(),
        };
        let error = output.require_success("create").unwrap_err();
        assert!(!format!("{error:#}").contains("private-value"));
        let error = parse_container(br#""private-value""#).err().unwrap();
        assert!(!format!("{error:#}").contains("private-value"));
        let mut invalid = document("running");
        invalid["Config"]["Labels"] = json!({"label": ["private-value"]});
        let error = parse(&invalid).err().unwrap();
        assert!(!format!("{error:#}").contains("private-value"));
    }

    #[test]
    fn strict_names_and_inputs() {
        assert_eq!(container_name(ID).unwrap(), format!("pvisor-{ID}"));
        for id in [
            "--all",
            "sb-x",
            "sb-00000000-0000-0000-0000-000000000000",
            "sb-6A069a75-82c2-4e05-a55b-07826d63a9e1",
            "../escape",
        ] {
            assert!(container_name(id).is_err());
        }
        let mut value = spec();
        value.env.insert("A=B".into(), "secret".into());
        assert!(validate_spec(&value).is_err());
        value = spec();
        value.entrypoint.push("nul\0byte".into());
        assert!(validate_spec(&value).is_err());
        value = spec();
        value.cpu_millis = 0;
        assert!(validate_spec(&value).is_err());
        value = spec();
        value.image = "--privileged".into();
        assert!(validate_spec(&value).is_err());
        assert!(validate_owner("").is_err());
        assert!(validate_owner("owner\nother").is_err());
    }

    #[test]
    fn assembly_preserves_argv_and_security_limits() {
        let value = spec();
        let args = create_args(&value, "owner", "attempt").unwrap();
        for required in [
            "--privileged=false",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--pull=never",
            "--image-volume=ignore",
            "--http-proxy=false",
            "--pid=private",
            "--ipc=private",
            "--cgroups=enabled",
            "--network=slirp4netns:allow_host_loopback=false",
        ] {
            assert!(args.iter().any(|arg| arg == required));
        }
        for (flag, expected) in [
            ("--cpus", "1.250"),
            ("--memory", "134217728"),
            ("--memory-swap", "134217728"),
            ("--pids-limit", "512"),
        ] {
            let index = args.iter().position(|arg| arg == flag).unwrap();
            assert_eq!(args[index + 1], expected);
        }
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--publish", "127.0.0.1::44772/tcp"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--publish", "127.0.0.1::18080/tcp"])
        );
        assert!(args.windows(2).any(|pair| pair == ["--env", "TEXT"]));
        let separator = args.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(args[separator + 1], value.image);
        assert_eq!(&args[separator + 2..], value.entrypoint.as_slice());
        assert!(!args[..separator].iter().any(|arg| matches!(
            arg.as_str(),
            "--privileged"
                | "--rm"
                | "--replace"
                | "--volume"
                | "--mount"
                | "--network=host"
                | "--pid=host"
        )));
    }

    #[test]
    fn preflight_requires_rootless_v2_and_every_controller() {
        let info = json!({"host": {"security": {"rootless": true}, "cgroupVersion": "v2",
            "cgroupManager": "systemd", "cgroupControllers": ["cpu", "memory", "pids"]}});
        assert!(validate_info(&info).is_ok());
        for (pointer, replacement) in [
            ("/host/security/rootless", json!(false)),
            ("/host/cgroupVersion", json!("v1")),
            ("/host/cgroupManager", json!("none")),
            ("/host/cgroupControllers", json!(["cpu", "pids"])),
            ("/host/cgroupControllers", json!(["memory", "pids"])),
            ("/host/cgroupControllers", json!(["cpu", "memory"])),
        ] {
            let mut invalid = info.clone();
            *invalid.pointer_mut(pointer).unwrap() = replacement;
            assert!(validate_info(&invalid).is_err());
        }
        assert!(validate_info(&json!({})).is_err());
    }

    #[test]
    fn ownership_and_real_states_are_not_inferred_from_errors() {
        for (status, expected) in [
            ("running", RuntimeState::Running),
            ("paused", RuntimeState::Paused),
            ("exited", RuntimeState::Stopped),
            ("created", RuntimeState::Stopped),
        ] {
            let container = parse(&document(status)).unwrap();
            assert_eq!(container.state, expected);
            assert!(container.check_owner("owner", ID).is_ok());
            assert!(container.check_owner("another-owner", ID).is_err());
            assert!(container.check_owner("owner", "another-id").is_err());
        }
        for status in ["restarting", "removing", "dead", "invented"] {
            assert!(parse(&document(status)).is_err());
        }
        assert!(parse_container(b"[]").is_err());
        assert!(parse_container(b"backend failure").is_err());
        let mut value = document("running");
        value["Config"]["Labels"] = json!({});
        assert!(parse(&value).unwrap().check_owner("owner", ID).is_err());
    }

    #[test]
    fn endpoints_must_be_single_loopback_tcp_bindings() {
        let value = document("running");
        assert_eq!(
            parse(&value).unwrap().endpoint(EXECD_PORT).unwrap(),
            "http://127.0.0.1:32100"
        );
        assert_eq!(
            parse(&value).unwrap().endpoint(EGRESS_PORT).unwrap(),
            "http://127.0.0.1:32101"
        );
        assert!(parse(&value).unwrap().endpoint(22).is_err());
        for bindings in [
            json!([{"HostIp": "0.0.0.0", "HostPort": "32100"}]),
            json!([{"HostIp": "127.0.0.1", "HostPort": "0"}]),
            json!([{"HostIp": "127.0.0.1", "HostPort": "65536"}]),
            json!([]),
            json!(null),
        ] {
            let mut invalid = value.clone();
            invalid["NetworkSettings"]["Ports"]["44772/tcp"] = bindings;
            assert!(parse(&invalid).unwrap().endpoint(EXECD_PORT).is_err());
        }
    }

    #[test]
    fn resources_and_initialization_fail_closed() {
        assert!(validate_resources(&document("created"), &spec()).is_ok());
        for key in ["Memory", "MemorySwap", "PidsLimit", "CpuQuota", "CpuPeriod"] {
            let mut invalid = document("created");
            invalid["HostConfig"][key] = json!(0);
            assert!(validate_resources(&invalid, &spec()).is_err());
        }
        assert!(validate_ready(br#"{"initialized":true}"#).is_ok());
        for body in [
            b"".as_slice(),
            br#"{"initialized":false}"#,
            br#"{"status":"ok"}"#,
        ] {
            assert!(validate_ready(body).is_err());
        }
    }
}
