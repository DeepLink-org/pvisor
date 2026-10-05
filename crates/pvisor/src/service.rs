//! One deployment entry with independently managed process/failure boundaries.
use anyhow::{Context, ensure};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
    process::{Child, Command},
};

#[derive(Debug, Args)]
pub struct ServiceArgs {
    #[command(subcommand)]
    command: ServiceCommand,
}
#[derive(Debug, Subcommand)]
enum ServiceCommand {
    /// Launch configured roles; a failed Controller does not stop node data owners.
    Run {
        #[arg(long)]
        config: PathBuf,
    },
    /// Inspect process status without exposing tokens.
    Status {
        #[arg(long)]
        config: PathBuf,
    },
    /// Restart a role. Active node owners and dependent pools are protected.
    Restart {
        #[arg(long)]
        config: PathBuf,
        role: String,
    },
    /// Stop one role, or drain Workers before stopping all roles.
    Stop {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        role: Option<String>,
    },
    /// Submit/query cluster tasks or serve the Controller.
    #[command(disable_help_flag = true, disable_help_subcommand = true)]
    Cluster(ToolArgs),
    /// Execute cluster tasks on this node.
    #[command(disable_help_flag = true, disable_help_subcommand = true)]
    Worker(ToolArgs),
    /// Prepare, publish or serve immutable image caches.
    #[command(disable_help_flag = true, disable_help_subcommand = true)]
    Cache(ToolArgs),
    /// Serve the experimental shared VM cold-page pool.
    #[command(disable_help_flag = true, disable_help_subcommand = true)]
    MemoryPool(ToolArgs),
    #[command(hide = true)]
    Node {
        #[arg(long)]
        config: PathBuf,
    },
}
#[derive(Debug, Args)]
struct ToolArgs {
    #[arg(num_args = 0.., trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<std::ffi::OsString>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    state: PathBuf,
    controller: Option<Controller>,
    node: Option<crate::node::Config>,
    workers: Vec<Worker>,
    pool: Option<Pool>,
    cgroup_root: Option<PathBuf>,
    limits: BTreeMap<String, Limits>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            state: ".pvisor/services".into(),
            controller: None,
            node: Some(crate::node::Config::default()),
            workers: Vec::new(),
            pool: None,
            cgroup_root: None,
            limits: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Controller {
    listen: std::net::SocketAddr,
    journal: PathBuf,
    admin_token_env: String,
    worker_token_env: String,
    lease_ms: u64,
    max_journal_bytes: u64,
    max_artifact_bytes: u64,
}
impl Default for Controller {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:19800".parse().unwrap(),
            journal: "controller/journal".into(),
            admin_token_env: "PVISOR_CLUSTER_TOKEN".into(),
            worker_token_env: "PVISOR_CLUSTER_WORKER_TOKEN".into(),
            lease_ms: 30_000,
            max_journal_bytes: 1024 * 1024 * 1024,
            max_artifact_bytes: 8 * 1024 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Worker {
    id: String,
    url: Option<String>,
    token_env: String,
    backend: String,
    profile: Option<PathBuf>,
    slots: u32,
    memory_bytes: u64,
    cpu_millis: u64,
    poll_ms: u64,
}
impl Default for Worker {
    fn default() -> Self {
        Self {
            id: "worker".into(),
            url: None,
            token_env: "PVISOR_CLUSTER_WORKER_TOKEN".into(),
            backend: "rootless".into(),
            profile: None,
            slots: 4,
            memory_bytes: 512 * 1024 * 1024,
            cpu_millis: 1000,
            poll_ms: 200,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Pool {
    max_bytes: usize,
    max_objects: usize,
    max_connections: usize,
    max_references: usize,
}
impl Default for Pool {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_objects: 8192,
            max_connections: 16,
            max_references: 32768,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Limits {
    memory_bytes: u64,
    cpu_millis: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_bytes: 512 * 1024 * 1024,
            cpu_millis: 500,
        }
    }
}
fn absolute(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.into()
    } else {
        base.join(path)
    }
}
impl Config {
    fn load(path: &Path) -> anyhow::Result<Self> {
        let path = path.canonicalize()?;
        let base = path.parent().unwrap();
        let mut config: Self = toml::from_str(&fs::read_to_string(&path)?)?;
        if config.node.as_ref().is_some_and(|node| !node.enabled) {
            config.node = None;
        }
        config.state = absolute(base, &config.state);
        if let Some(controller) = &mut config.controller {
            controller.journal = absolute(&config.state, &controller.journal);
        }
        let mut ids = std::collections::BTreeSet::new();
        for worker in &mut config.workers {
            ensure!(
                !worker.id.is_empty()
                    && worker.id.len() <= 64
                    && worker
                        .id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
                    && ids.insert(worker.id.clone()),
                "worker IDs must be unique safe path components"
            );
            ensure!(
                ["host", "rootless", "container", "vm"].contains(&worker.backend.as_str())
                    && worker.slots > 0
                    && worker.memory_bytes > 0
                    && worker.cpu_millis > 0
                    && worker.poll_ms > 0,
                "invalid worker resources/backend"
            );
            worker.profile = worker.profile.as_ref().map(|path| absolute(base, path));
        }
        if let Some(node) = &mut config.node {
            node.state = absolute(&config.state, &node.state);
            node.socket = absolute(&node.state, &node.socket);
            node.snapshot_roots = node
                .snapshot_roots
                .iter()
                .map(|path| absolute(base, path))
                .collect();
            node.snapshot_roots.extend(
                config
                    .workers
                    .iter()
                    .map(|worker| config.state.join("workers").join(&worker.id)),
            );
            if node.cache_backend == "filesystem" {
                node.cache_location = node.cache_location.as_ref().map(|path| {
                    let path = path.strip_prefix("file://").unwrap_or(path);
                    absolute(base, Path::new(path))
                        .to_string_lossy()
                        .into_owned()
                });
            }
        }
        config.cgroup_root = config
            .cgroup_root
            .as_ref()
            .map(|path| -> anyhow::Result<PathBuf> {
                if path == Path::new(":self:") {
                    ensure!(
                        cfg!(target_os = "linux"),
                        "self cgroup delegation requires Linux"
                    );
                    pvisor_cluster::admission::current_cgroup_v2()
                } else {
                    Ok(absolute(base, path))
                }
            })
            .transpose()?;
        ensure!(
            config.pool.is_none()
                || (cfg!(all(target_os = "macos", target_arch = "aarch64"))
                    && config.node.is_some()),
            "experimental cold pool deployment requires Apple Silicon and a node role"
        );
        for limits in config.limits.values() {
            ensure!(
                limits.memory_bytes > 0 && limits.cpu_millis > 0,
                "service limits must be positive"
            );
        }
        Ok(config)
    }
    fn management_socket(&self) -> PathBuf {
        self.state.join("service.sock")
    }
    fn pool_socket(&self) -> PathBuf {
        self.node.as_ref().unwrap().state.join("pool.sock")
    }
    fn controller_url(&self) -> Option<String> {
        self.controller
            .as_ref()
            .map(|c| format!("http://{}", c.listen))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Status,
    Restart { role: String },
    Stop { role: Option<String> },
}
struct Role {
    command: std::process::Command,
    child: Option<Child>,
    exit: Option<String>,
}
struct Manager {
    config: Config,
    roles: BTreeMap<String, Role>,
}
fn token(name: &str) -> anyhow::Result<String> {
    let value = std::env::var(name).with_context(|| format!("set {name}"))?;
    ensure!(!value.is_empty(), "empty service token: {name}");
    Ok(value)
}
fn companion(name: &str) -> anyhow::Result<PathBuf> {
    crate::cli::extensions::find(
        name.strip_prefix("pvisor-")
            .context("invalid companion name")?,
    )?
    .map(|(path, _)| path)
    .with_context(|| format!("missing companion {name}; build/install the service component set"))
}
impl Manager {
    fn new(config: Config, config_path: &Path) -> anyhow::Result<Self> {
        let mut roles = BTreeMap::new();
        let mut add = |name: String, command| {
            roles.insert(
                name,
                Role {
                    command,
                    child: None,
                    exit: None,
                },
            );
        };
        if let Some(controller) = &config.controller {
            let mut command = std::process::Command::new(companion("pvisor-cluster")?);
            command
                .env("PVISOR_CLUSTER_TOKEN", token(&controller.admin_token_env)?)
                .env(
                    "PVISOR_CLUSTER_WORKER_TOKEN",
                    token(&controller.worker_token_env)?,
                );
            command
                .args([
                    "serve",
                    "--listen",
                    &controller.listen.to_string(),
                    "--lease-ms",
                    &controller.lease_ms.to_string(),
                    "--max-journal-bytes",
                    &controller.max_journal_bytes.to_string(),
                    "--max-artifact-bytes",
                    &controller.max_artifact_bytes.to_string(),
                ])
                .arg("--journal")
                .arg(&controller.journal);
            add("controller".into(), command);
        }
        if config.node.is_some() {
            let mut command = std::process::Command::new(std::env::current_exe()?);
            command
                .args(["service", "node", "--config"])
                .arg(config_path);
            add("node".into(), command);
        }
        if let Some(pool) = &config.pool {
            let mut command = std::process::Command::new(companion("pvisor-memory-pool")?);
            command.arg(config.pool_socket()).args([
                "--max-bytes",
                &pool.max_bytes.to_string(),
                "--max-objects",
                &pool.max_objects.to_string(),
                "--max-connections",
                &pool.max_connections.to_string(),
                "--max-references",
                &pool.max_references.to_string(),
            ]);
            add("pool".into(), command);
        }
        for worker in &config.workers {
            crate::node::private_directory(&config.state.join("workers").join(&worker.id))?;
            let mut command = std::process::Command::new(companion("pvisor-worker")?);
            command
                .env_remove("PVISOR_CLUSTER_TOKEN")
                .env("PVISOR_CLUSTER_WORKER_TOKEN", token(&worker.token_env)?);
            let url = worker
                .url
                .clone()
                .or_else(|| config.controller_url())
                .context("worker needs a URL or local Controller")?;
            command
                .args([
                    "--id",
                    &worker.id,
                    "--url",
                    &url,
                    "--backend",
                    &worker.backend,
                    "--slots",
                    &worker.slots.to_string(),
                    "--memory-bytes",
                    &worker.memory_bytes.to_string(),
                    "--cpu-millis",
                    &worker.cpu_millis.to_string(),
                    "--poll-ms",
                    &worker.poll_ms.to_string(),
                ])
                .arg("--state")
                .arg(config.state.join("workers").join(&worker.id));
            if let Some(profile) = &worker.profile {
                command.arg("--config").arg(profile);
            }
            if let Some(node) = &config.node {
                command.arg("--node-socket").arg(&node.socket);
            }
            if config.pool.is_some() && worker.backend == "vm" {
                command.arg("--memory-pool").arg(config.pool_socket());
            }
            add(format!("worker:{}", worker.id), command);
        }
        ensure!(!roles.is_empty(), "service configuration has no roles");
        ensure!(
            config.limits.keys().all(|name| roles.contains_key(name)),
            "limits name an unconfigured role"
        );
        for (name, role) in &mut roles {
            role.command.env("TOKIO_WORKER_THREADS", "2");
            if name != "controller" {
                role.command.env_remove("PVISOR_CLUSTER_TOKEN");
                if let Some(controller) = &config.controller {
                    role.command.env_remove(&controller.admin_token_env);
                }
            }
        }
        Ok(Self { config, roles })
    }
    fn refresh(&mut self) -> anyhow::Result<()> {
        for role in self.roles.values_mut() {
            if let Some(child) = &mut role.child
                && let Some(status) = child.try_wait()?
            {
                role.exit = Some(status.to_string());
                role.child = None;
            }
        }
        Ok(())
    }
    fn status(&mut self) -> anyhow::Result<serde_json::Value> {
        self.refresh()?;
        Ok(
            serde_json::json!({"kernel_limits": self.config.cgroup_root.is_some(), "roles": self.roles.iter().map(|(name, role)| (name, serde_json::json!({"pid": role.child.as_ref().and_then(Child::id), "state": if role.child.is_some() { "running" } else { "stopped" }, "exit": role.exit, "readiness": if name.starts_with("worker:") { "process_started" } else { "endpoint_checked_at_start" }}))).collect::<BTreeMap<_, _>>() }),
        )
    }
    async fn status_with_resources(&mut self) -> anyhow::Result<serde_json::Value> {
        let mut value = self.status()?;
        if self
            .roles
            .get("node")
            .is_some_and(|role| role.child.is_some())
        {
            let socket = self.config.node.as_ref().unwrap().socket.clone();
            let resources =
                tokio::task::spawn_blocking(move || crate::node::stats(&socket)).await?;
            value["node_resources"] = match resources {
                Ok(stats) => stats,
                Err(error) => serde_json::json!({"error": format!("{error:#}")}),
            };
        }
        Ok(value)
    }
    fn spawn(&mut self, name: &str) -> anyhow::Result<()> {
        let role = self.roles.get_mut(name).context("unknown service role")?;
        ensure!(role.child.is_none(), "role already running");
        crate::node::private_directory(&self.config.state.join("logs"))?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(
                self.config
                    .state
                    .join("logs")
                    .join(format!("{}.log", name.replace(':', "-"))),
            )?;
        let mut command = Command::new(role.command.get_program());
        command.args(role.command.get_args());
        for (key, value) in role.command.get_envs() {
            match value {
                Some(value) => {
                    command.env(key, value);
                }
                None => {
                    command.env_remove(key);
                }
            }
        }
        command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        command.as_std_mut().process_group(0);
        install_limits(
            command.as_std_mut(),
            self.config.cgroup_root.as_deref(),
            name,
            self.config.limits.get(name).cloned().unwrap_or_default(),
        )?;
        role.child = Some(command.kill_on_drop(false).spawn()?);
        role.exit = None;
        Ok(())
    }
    async fn ready(&mut self, name: &str) -> anyhow::Result<()> {
        if name.starts_with("worker:") {
            return Ok(());
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            self.refresh()?;
            ensure!(
                self.roles[name].child.is_some(),
                "service role {name} exited during startup; inspect its log"
            );
            let ready = match name {
                "controller" => {
                    let controller = self.config.controller.as_ref().unwrap();
                    reqwest::Client::new()
                        .get(format!("{}/health", self.config.controller_url().unwrap()))
                        .bearer_auth(token(&controller.admin_token_env)?)
                        .timeout(Duration::from_secs(1))
                        .send()
                        .await
                        .is_ok_and(|reply| reply.status().is_success())
                }
                "node" => {
                    let socket = self.config.node.as_ref().unwrap().socket.clone();
                    tokio::task::spawn_blocking(move || crate::node::stats(&socket))
                        .await?
                        .is_ok()
                }
                "pool" => {
                    let socket = self.config.pool_socket();
                    tokio::task::spawn_blocking(move || {
                        std::os::unix::net::UnixStream::connect(socket)
                    })
                    .await?
                    .is_ok()
                }
                _ => false,
            };
            if ready {
                return Ok(());
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "service role {name} readiness timed out"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    async fn protect(&mut self, name: &str) -> anyhow::Result<()> {
        self.refresh()?;
        if name == "node"
            && self
                .roles
                .get(name)
                .is_some_and(|role| role.child.is_some())
        {
            let socket = self.config.node.as_ref().unwrap().socket.clone();
            let stats = tokio::task::spawn_blocking(move || crate::node::stats(&socket)).await??;
            ensure!(
                stats["active_pins"].as_u64() == Some(0),
                "node has active pins; drain dependent tasks first"
            );
        }
        if name == "pool" {
            ensure!(
                !self
                    .roles
                    .iter()
                    .any(|(name, role)| name.starts_with("worker:") && role.child.is_some()),
                "drain and stop Workers before stopping the cold pool"
            );
        }
        Ok(())
    }
    async fn stop_role(&mut self, name: &str) -> anyhow::Result<()> {
        self.protect(name).await?;
        let role = self.roles.get_mut(name).context("unknown service role")?;
        if let Some(child) = &mut role.child {
            if let Some(pid) = child.id() {
                let result = unsafe { libc::kill(pid as i32, libc::SIGINT) };
                ensure!(
                    result == 0
                        || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
                    "cannot signal service role"
                );
            }
            // Do not kill data owners if a Worker cannot finish native teardown.
            let status = tokio::time::timeout(Duration::from_secs(30), child.wait())
                .await
                .context("role still draining; data owners were retained")??;
            role.exit = Some(status.to_string());
            role.child = None;
        }
        Ok(())
    }
    async fn stop_all(&mut self) -> anyhow::Result<()> {
        let workers: Vec<_> = self
            .roles
            .keys()
            .filter(|name| name.starts_with("worker:"))
            .cloned()
            .collect();
        for worker in workers {
            self.stop_role(&worker).await?;
        }
        for name in ["pool", "node", "controller"] {
            if self.roles.contains_key(name) {
                self.stop_role(name).await?;
            }
        }
        Ok(())
    }
}
fn prepare_cgroup_root(root: Option<&Path>) -> anyhow::Result<()> {
    let Some(root) = root else {
        return Ok(());
    };
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let directory = fs::File::open(root)?;
        let mut info: libc::statfs = unsafe { std::mem::zeroed() };
        ensure!(
            unsafe { libc::fstatfs(directory.as_raw_fd(), &mut info) } == 0
                && info.f_type == 0x63677270,
            "cgroup_root must be a real delegated cgroup v2 filesystem"
        );
        let controllers = fs::read_to_string(root.join("cgroup.controllers"))?;
        ensure!(
            ["cpu", "memory"]
                .iter()
                .all(|name| controllers.split_whitespace().any(|value| value == *name)),
            "cpu and memory controllers must be delegated"
        );
        // A domain cgroup cannot contain processes while enabling controllers
        // for children. Move only this supervisor, never unrelated processes.
        if fs::read_to_string(root.join("cgroup.procs"))?
            .split_whitespace()
            .any(|pid| pid == std::process::id().to_string())
        {
            let supervisor = root.join("supervisor");
            if !supervisor.exists() {
                fs::create_dir(&supervisor)?;
            }
            ensure!(
                fs::read_to_string(supervisor.join("cgroup.procs"))?
                    .trim()
                    .is_empty(),
                "supervisor cgroup already occupied"
            );
            fs::write(supervisor.join("cgroup.procs"), "0")?;
        }
        fs::write(root.join("cgroup.subtree_control"), "+cpu +memory").context("enable delegated controllers; use a dedicated service cgroup without unrelated processes")?;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = root;
        anyhow::bail!("cgroup_root requires Linux");
    }
}
fn install_limits(
    command: &mut std::process::Command,
    root: Option<&Path>,
    name: &str,
    limits: Limits,
) -> anyhow::Result<()> {
    let Some(root) = root else {
        return Ok(());
    };
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        ensure!(
            root.join("cgroup.controllers").is_file(),
            "cgroup_root must be a delegated cgroup v2 directory"
        );
        let group = root.join(name.replace(':', "-"));
        if !group.exists() {
            fs::create_dir(&group)?;
        }
        ensure!(
            fs::read_to_string(group.join("cgroup.procs"))?
                .trim()
                .is_empty(),
            "role cgroup already contains processes"
        );
        fs::write(group.join("memory.max"), limits.memory_bytes.to_string())?;
        fs::write(group.join("memory.swap.max"), "0")?;
        fs::write(
            group.join("cpu.max"),
            format!(
                "{} 100000",
                limits
                    .cpu_millis
                    .checked_mul(100)
                    .context("CPU limit overflow")?
            ),
        )?;
        let attach = OpenOptions::new()
            .write(true)
            .open(group.join("cgroup.procs"))?;
        unsafe {
            command.pre_exec(move || {
                if libc::write(attach.as_raw_fd(), b"0\n".as_ptr().cast(), 2) != 2 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (command, root, name, limits);
        anyhow::bail!("cgroup_root requires Linux");
    }
}

pub async fn run(args: ServiceArgs) -> anyhow::Result<()> {
    let (path, request) = match args.command {
        ServiceCommand::Cluster(tool) => {
            return crate::cli::extensions::dispatch("cluster", &tool.args);
        }
        ServiceCommand::Worker(tool) => {
            return crate::cli::extensions::dispatch("worker", &tool.args);
        }
        ServiceCommand::Cache(tool) => {
            return crate::cli::extensions::dispatch("cache", &tool.args);
        }
        ServiceCommand::MemoryPool(tool) => {
            return crate::cli::extensions::dispatch("memory-pool", &tool.args);
        }
        ServiceCommand::Run { config } => return supervise(&config).await,
        ServiceCommand::Node { config } => {
            return crate::node::serve(
                Config::load(&config)?
                    .node
                    .context("node role is disabled")?,
            )
            .await;
        }
        ServiceCommand::Status { config } => (config, Request::Status),
        ServiceCommand::Restart { config, role } => (config, Request::Restart { role }),
        ServiceCommand::Stop { config, role } => (config, Request::Stop { role }),
    };
    let config = Config::load(&path)?;
    let mut stream = UnixStream::connect(config.management_socket()).await?;
    stream.write_all(&serde_json::to_vec(&request)?).await?;
    stream.write_all(b"\n").await?;
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response).await?;
    let value: serde_json::Value = serde_json::from_str(&response)?;
    ensure!(value.get("error").is_none(), "{}", value["error"]);
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
async fn supervise(path: &Path) -> anyhow::Result<()> {
    use fs2::FileExt;
    let path = path.canonicalize()?;
    let config = Config::load(&path)?;
    crate::node::private_directory(&config.state)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(config.state.join("service.lock"))?;
    lock.try_lock_exclusive()
        .context("service state already owned")?;
    prepare_cgroup_root(config.cgroup_root.as_deref())?;
    // Children use an immutable resolved configuration for this deployment,
    // independent of edits to the caller's source TOML while roles are running.
    let active_path = config.state.join("active-config.toml");
    let mut active = tempfile::NamedTempFile::new_in(&config.state)?;
    std::io::Write::write_all(&mut active, toml::to_string(&config)?.as_bytes())?;
    active.as_file().sync_all()?;
    active.persist(&active_path)?;
    let (listener, _socket) = crate::node::SocketGuard::bind(&config.management_socket())?;
    let mut manager = Manager::new(config.clone(), &active_path)?;
    let names: Vec<_> = ["controller", "node", "pool"]
        .into_iter()
        .filter(|name| manager.roles.contains_key(*name))
        .map(str::to_owned)
        .chain(
            manager
                .roles
                .keys()
                .filter(|name| name.starts_with("worker:"))
                .cloned(),
        )
        .collect();
    for name in names {
        if let Err(error) = async {
            manager.spawn(&name)?;
            manager.ready(&name).await
        }
        .await
        {
            if let Err(cleanup) = manager.stop_all().await {
                tracing::error!(%cleanup, "startup cleanup retained dependent data owners");
            }
            return Err(error);
        }
    }
    println!("{}", serde_json::to_string(&manager.status()?)?);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } { continue; }
                let mut stream = BufReader::new(stream);
                let request = tokio::time::timeout(Duration::from_secs(2), read_request(&mut stream)).await;
                let mut exiting = false;
                let result = match request {
                    Ok(Ok(Request::Status)) => manager.status_with_resources().await,
                    Ok(Ok(Request::Stop { role: Some(role) })) => match manager.stop_role(&role).await { Ok(()) => manager.status(), Err(error) => Err(error) },
                    Ok(Ok(Request::Stop { role: None })) => match manager.stop_all().await { Ok(()) => { exiting = true; manager.status() }, Err(error) => Err(error) },
                    Ok(Ok(Request::Restart { role })) => async {
                        manager.stop_role(&role).await?;
                        manager.spawn(&role)?; manager.ready(&role).await?; manager.status()
                    }.await,
                    Ok(Err(error)) => Err(error),
                    Err(error) => Err(error.into()),
                };
                let value = result.unwrap_or_else(|error| serde_json::json!({"error": format!("{error:#}")}));
                let _ = tokio::time::timeout(Duration::from_secs(2), async { stream.get_mut().write_all(&serde_json::to_vec(&value)?).await?; stream.get_mut().write_all(b"\n").await?; Ok::<_, anyhow::Error>(()) }).await;
                if exiting { break; }
            }
            _ = interval.tick() => { manager.refresh()?; },
            _ = tokio::signal::ctrl_c() => { match manager.stop_all().await { Ok(()) => break, Err(error) => tracing::error!(%error, "shutdown retained data owners; retry after draining") } },
            _ = terminate.recv() => { match manager.stop_all().await { Ok(()) => break, Err(error) => tracing::error!(%error, "shutdown retained data owners; retry after draining") } },
        }
    }
    Ok(())
}
async fn read_request(stream: &mut BufReader<UnixStream>) -> anyhow::Result<Request> {
    let mut bytes = Vec::new();
    loop {
        let available = stream.fill_buf().await?;
        ensure!(!available.is_empty(), "incomplete service request");
        let length = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|i| i + 1)
            .unwrap_or(available.len());
        ensure!(
            bytes.len() + length <= 64 * 1024,
            "service request too large"
        );
        let complete = available[length - 1] == b'\n';
        bytes.extend_from_slice(&available[..length]);
        stream.consume(length);
        if complete {
            return Ok(serde_json::from_slice(&bytes)?);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_unknown_fields_and_unsafe_worker_ids() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("service.toml");
        for text in [
            "unrecognized = true",
            "[[workers]]\nid = '../outside'",
            "[[workers]]\nid = 'same'\n[[workers]]\nid = 'same'",
            "[[workers]]\nslots = 0",
        ] {
            fs::write(&path, text).unwrap();
            assert!(Config::load(&path).is_err());
        }
    }
    #[test]
    fn configuration_paths_are_relative_to_config_and_service_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("service.toml");
        fs::write(&path, "state = 'state'\n[controller]\n[node]\ncache_location = 'cache'\n[[workers]]\nid = 'one'\nprofile = 'profile.toml'\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(
            config.node.as_ref().unwrap().socket,
            directory.path().join("state/node/node.sock")
        );
        assert_eq!(
            config.node.as_ref().unwrap().cache_location.as_deref(),
            directory.path().join("cache").to_str()
        );
        assert_eq!(
            config.controller.unwrap().journal,
            directory.path().join("state/controller/journal")
        );
        assert_eq!(
            config.workers[0].profile,
            Some(directory.path().join("profile.toml"))
        );
    }
}
