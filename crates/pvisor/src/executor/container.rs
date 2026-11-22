//! Native OCI runtime transport. pVisor materializes an OCI bundle and invokes
//! runc/crun; no Docker or Podman daemon is required.

use crate::config::{ContainerMount, ContainerPlatform, ContainerSettings};
use crate::executor::{Captured, read_limited, stdio};
use crate::executor::{ExecutionContext, ExecutorOutput, RunExecutor, SessionEnd as End};
use async_trait::async_trait;
use pvisor_core::{
    ExecutorKind, ExecutorPlan, IsolationKind, ProcessOutput, ResourceLimits, RunFailure,
    RunFailureKind, RunInvocation, RunSpec, RunState, StdioMode,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::{Child, Command};

const CAPTURE_CONFIG_ENV: &str = "PVISOR_CAPTURE_CONFIG";

#[derive(Debug)]
struct Launch {
    command: Command,
    state_root: PathBuf,
    pid_file: PathBuf,
    name: String,
}

#[derive(Debug, Clone)]
pub struct ContainerExecutor {
    settings: ContainerSettings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BindMount {
    source: PathBuf,
    target: PathBuf,
    read_only: bool,
}

impl ContainerExecutor {
    pub fn new(settings: ContainerSettings) -> anyhow::Result<Self> {
        if let Some(platform) = settings.platform {
            let native = matches!(
                (std::env::consts::ARCH, platform),
                ("x86_64", ContainerPlatform::LinuxAmd64)
                    | ("aarch64", ContainerPlatform::LinuxArm64)
            );
            anyhow::ensure!(
                native,
                "container.platform={platform:?} does not match host architecture {}; cross-platform selection is not supported by the native OCI runner; omit container.platform or select the native platform",
                std::env::consts::ARCH
            );
        }
        anyhow::ensure!(
            !settings.runtime.as_os_str().is_empty(),
            "container runtime must not be empty"
        );
        anyhow::ensure!(
            !settings.image.trim().is_empty() || settings.rootfs.is_some(),
            "container requires an image or an explicit rootfs"
        );
        anyhow::ensure!(
            settings.network != crate::config::ContainerNetwork::Bridge,
            "container.network=bridge requires CNI and is not supported by native OCI runner; use host or none"
        );
        if let Some(workdir) = &settings.workdir {
            anyhow::ensure!(
                workdir.is_absolute(),
                "container workdir must be absolute: {}",
                workdir.display()
            );
        }
        for mount in &settings.mounts {
            validate_mount(mount)?;
        }
        Ok(Self { settings })
    }

    pub fn settings(&self) -> &ContainerSettings {
        &self.settings
    }

    fn build_command(
        &self,
        spec: &RunSpec,
        execution_id: &str,
        control_dir: &Path,
        rootfs_source: &Path,
    ) -> anyhow::Result<Launch> {
        let RunInvocation::Process(invocation) = &spec.invocation;
        let run_id = spec.run_id.as_str();
        let limits = &spec.runtime.resource_limits;
        let mut mounts = BTreeMap::<PathBuf, BindMount>::new();
        for mount in &self.settings.mounts {
            add_mount(
                &mut mounts,
                bind_mount(&mount.source, &mount.target, mount.read_only)?,
            )?;
        }
        let workdir = invocation
            .cwd
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| self.settings.workdir.clone());
        if let Some(path) = invocation.cwd.as_deref().map(Path::new) {
            if path.exists() {
                let target = absolute_container_path(path)?;
                add_mount(&mut mounts, bind_mount(path, &target, false)?)?;
            } else {
                anyhow::ensure!(
                    path.is_absolute(),
                    "container-native cwd must be absolute when it is not a host path: {}",
                    path.display()
                );
            }
        }
        if let Some(value) = invocation.env.get(CAPTURE_CONFIG_ENV) {
            let path = Path::new(value);
            if path.is_absolute() && path.exists() {
                add_mount(&mut mounts, bind_mount(path, path, true)?)?;
            }
        }

        let bundle = control_dir.join("bundle");
        fs::create_dir_all(&bundle)?;
        let configured_rootfs = rootfs_source.canonicalize()?;
        let private_control = control_dir.canonicalize()?;
        let rootfs = if rootfs_source == Path::new("/") {
            // A host-root container gets a private synthetic root directory.
            // Standard host directories are mounted read-only into it, so OCI
            // mountpoint creation never mutates the real host `/`.
            let synthetic = bundle.join("rootfs");
            fs::create_dir_all(&synthetic)?;
            fs::create_dir_all(synthetic.join("tmp"))?;
            fs::create_dir_all(synthetic.join("dev"))?;
            fs::create_dir_all(synthetic.join("proc"))?;
            fs::create_dir_all(synthetic.join("sys"))?;
            for path in ["bin", "usr", "lib", "lib64", "sbin", "etc", "var"] {
                let source = PathBuf::from(format!("/{path}"));
                if source.is_dir() {
                    add_mount(
                        &mut mounts,
                        bind_mount(&source, Path::new(&format!("/{path}")), true)?,
                    )?;
                }
            }
            synthetic
        } else {
            anyhow::ensure!(
                configured_rootfs.is_dir(),
                "OCI rootfs does not exist: {}",
                configured_rootfs.display()
            );
            anyhow::ensure!(
                !private_control.starts_with(&configured_rootfs)
                    && !configured_rootfs.starts_with(&private_control),
                "OCI rootfs overlaps host control storage"
            );
            // An independent rootfs copy avoids retaining a source mount/session owner.
            let private = bundle.join("rootfs");
            pvisor_overlay_core::apply::restore_overlay_upper(&configured_rootfs, &private)?;
            private
        };
        for mount in mounts.values() {
            let source = &mount.source;
            anyhow::ensure!(
                !private_control.starts_with(source) && !source.starts_with(&private_control),
                "OCI mount {} exposes host control storage",
                mount.source.display()
            );
        }
        pvisor_overlay_core::sys::prepare_rooted_path(&rootfs, Path::new("tmp"), true, false)?;
        for mount in mounts.values() {
            let relative = mount.target.strip_prefix("/")?;
            pvisor_overlay_core::sys::prepare_rooted_path(
                &rootfs,
                relative,
                fs::metadata(&mount.source)?.is_dir(),
                false,
            )?;
        }
        let config = bundle.join("config.json");
        let mut namespaces = vec![
            serde_json::json!({"type":"pid"}),
            serde_json::json!({"type":"ipc"}),
            serde_json::json!({"type":"uts"}),
            serde_json::json!({"type":"mount"}),
        ];
        if self.settings.network != crate::config::ContainerNetwork::Host {
            namespaces.push(serde_json::json!({"type":"network"}));
        }
        let mut mounts_json = Vec::new();
        mounts_json.push(serde_json::json!({"destination":"/dev","type":"tmpfs","source":"tmpfs","options":["nosuid","noexec","nodev","mode=755"]}));
        mounts_json.push(serde_json::json!({"destination":"/dev/shm","type":"tmpfs","source":"shm","options":["nosuid","noexec","nodev"]}));
        mounts_json.push(serde_json::json!({"destination":"/proc","type":"proc","source":"proc","options":["nosuid","noexec","nodev"]}));
        mounts_json.push(serde_json::json!({"destination":"/tmp","type":"tmpfs","source":"tmpfs","options":["nosuid","nodev","mode=1777"]}));
        // Bind mounts follow the standard pseudo-filesystem mounts.
        for m in mounts.values() {
            mounts_json.push(serde_json::json!({"destination":m.target,"type":"bind","source":m.source,"options":if m.read_only { vec!["rbind","ro"] } else { vec!["rbind","rw"] }}));
        }
        let requested_user = parse_user(self.settings.user.as_deref())?;
        let host_uid = unsafe { libc::geteuid() };
        let host_gid = unsafe { libc::getegid() };
        let process_user = requested_user;
        namespaces.insert(0, serde_json::json!({"type":"user"}));
        let resources = serde_json::json!({"memory": limits.memory_bytes.map(|v| serde_json::json!({"limit":v})), "pids": limits.processes.map(|v| serde_json::json!({"limit":v}))});
        let mut env = invocation.env.clone();
        if invocation.inherit_env {
            for (key, value) in std::env::vars() {
                env.entry(key).or_insert(value);
            }
        }
        env.retain(|key, _| valid_env_name(key) && !key.starts_with("PVISOR_AGENTCTL_"));
        env.entry("PATH".into()).or_insert_with(|| {
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into()
        });
        let env = env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>();
        let devices = [
            ("/dev/null", 1, 3), ("/dev/zero", 1, 5), ("/dev/random", 1, 8),
            ("/dev/urandom", 1, 9), ("/dev/tty", 5, 0),
        ].into_iter().map(|(path, major, minor)| serde_json::json!({"path":path,"type":"c","major":major,"minor":minor,"fileMode":438,"uid":0,"gid":0})).collect::<Vec<_>>();
        // Keep the workload mapped to the caller so writable mounts retain their ownership.
        // A non-root workload also needs a separate, authorized mapping for
        // namespace root; never silently replace its requested identity.
        let (uid_mappings, gid_mappings) = if requested_user == (0, 0) {
            (
                mapping_entries(0, host_uid, host_uid),
                mapping_entries(0, host_gid, host_gid),
            )
        } else {
            // The runtime's outer namespace maps 0 to the caller and 1 to an
            // authorized subordinate ID. The inner container then maps the
            // requested workload identity back to the caller, and root to 1.
            (
                mapping_entries(requested_user.0, 0, 1),
                mapping_entries(requested_user.1, 0, 1),
            )
        };
        let argv = std::iter::once(&invocation.program)
            .chain(&invocation.args)
            .collect::<Vec<_>>();
        let cfg = serde_json::json!({
            "ociVersion": "1.0.2",
            "process": {
                "terminal": false,
                "cwd": workdir.as_deref().unwrap_or(Path::new("/")),
                "args": argv,
                "env": env,
                "user": {"uid":process_user.0,"gid":process_user.1},
                "rlimits": process_limits(limits),
            },
            "root": {"path":rootfs,"readonly":self.settings.read_only_rootfs},
            "mounts": mounts_json,
            "linux": {
                "namespaces": namespaces,
                "resources": resources,
                "devices": devices,
                "uidMappings": uid_mappings,
                "gidMappings": gid_mappings,
            },
            "annotations": {"io.pvisor.run_id":run_id,"io.pvisor.execution_id":execution_id},
        });
        crate::util::write_private_json(&config, &cfg)?;
        let state_root = control_dir.join("state");
        fs::create_dir_all(&state_root)?;
        let pid_file = control_dir.join("init.pid");
        let name = container_name(run_id, execution_id);
        let mut command = self.runtime_command(&state_root)?;
        command
            .arg("run")
            .arg("--bundle")
            .arg(bundle)
            .arg("--pid-file")
            .arg(&pid_file)
            .arg(&name);

        let stdin = match invocation.stdin {
            StdioMode::Capture => StdioMode::Null,
            mode => mode,
        };
        command
            .stdin(stdio(stdin))
            .stdout(stdio(invocation.stdout))
            .stderr(stdio(invocation.stderr))
            .kill_on_drop(true);
        Ok(Launch {
            command,
            state_root,
            pid_file,
            name,
        })
    }

    fn runtime_command(&self, state_root: &Path) -> anyhow::Result<Command> {
        let mut command = if parse_user(self.settings.user.as_deref())? == (0, 0) {
            Command::new(&self.settings.runtime)
        } else {
            let host_uid = unsafe { libc::geteuid() };
            let host_gid = unsafe { libc::getegid() };
            let root_uid = authorized_subordinate_id(host_uid, Path::new("/etc/subuid"))?;
            let root_gid = authorized_subordinate_id(host_gid, Path::new("/etc/subgid"))?;
            // runc chowns its anonymous stdio pipes to the container root ID
            // before starting the inner namespace. Give the runtime CHOWN in
            // an outer user namespace covering only the caller and one
            // authorized subordinate identity; host privileges stay unchanged.
            let mut command = Command::new("unshare");
            command.args([
                "--user".into(),
                format!("--map-users=0:{host_uid}:1"),
                format!("--map-users=1:{root_uid}:1"),
                format!("--map-groups=0:{host_gid}:1"),
                format!("--map-groups=1:{root_gid}:1"),
                "--setuid=0".into(),
                "--setgid=0".into(),
                "--".into(),
            ]);
            command
                .arg(&self.settings.runtime)
                .args(["--rootless", "true"]);
            command
        };
        command.arg("--root").arg(state_root).kill_on_drop(true);
        Ok(command)
    }

    async fn runtime_operation(&self, state_root: &Path, args: &[&str]) -> Option<String> {
        let mut command = match self.runtime_command(state_root) {
            Ok(command) => command,
            Err(error) => return Some(format!("OCI {}: {error:#}", args.join(" "))),
        };
        match tokio::time::timeout(Duration::from_secs(2), command.args(args).output()).await {
            Ok(Ok(output)) if output.status.success() => None,
            Ok(Ok(output)) => Some(format!(
                "OCI {} failed ({}): {}",
                args.join(" "),
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )),
            Ok(Err(error)) => Some(format!("OCI {}: {error}", args.join(" "))),
            Err(_) => Some(format!("OCI {} timed out", args.join(" "))),
        }
    }

    async fn terminate(
        &self,
        child: &mut Child,
        state_root: &Path,
        name: &str,
        grace_ms: u64,
    ) -> Option<String> {
        let mut errors = Vec::new();
        if let Some(error) = self
            .runtime_operation(state_root, &["kill", name, "TERM"])
            .await
        {
            errors.push(error);
        }
        let stopped = matches!(
            tokio::time::timeout(Duration::from_millis(grace_ms), child.wait()).await,
            Ok(Ok(_))
        );
        if !stopped {
            if let Some(error) = self
                .runtime_operation(state_root, &["kill", name, "KILL"])
                .await
            {
                errors.push(error);
            }
            if let Err(error) = child.start_kill() {
                errors.push(format!("kill OCI transport: {error}"));
            }
            if !matches!(
                tokio::time::timeout(Duration::from_secs(2), child.wait()).await,
                Ok(Ok(_))
            ) {
                errors.push("OCI transport did not exit before cleanup deadline".into());
            }
        }
        if let Some(error) = self
            .runtime_operation(state_root, &["delete", "--force", name])
            .await
        {
            errors.push(error);
        }
        (!errors.is_empty()).then(|| errors.join("; "))
    }
}

async fn join_capture_bounded(
    task: Option<tokio::task::JoinHandle<std::io::Result<Captured>>>,
) -> Option<Captured> {
    let mut task = task?;
    match tokio::time::timeout(Duration::from_secs(2), &mut task).await {
        Ok(result) => result.ok().and_then(Result::ok),
        Err(_) => {
            task.abort();
            Some(Captured {
                text: String::new(),
                truncated: true,
            })
        }
    }
}

#[async_trait]
impl RunExecutor for ContainerExecutor {
    fn descriptor(&self) -> ExecutorPlan {
        ExecutorPlan {
            name: "oci-pvisor".into(),
            kind: ExecutorKind::Container,
            isolation: IsolationKind::Container,
            capability_plan: Default::default(),
            supports_checkpoint: false,
            supports_migration: false,
        }
    }

    fn supports(&self, invocation: &RunInvocation) -> bool {
        matches!(invocation, RunInvocation::Process(_))
    }

    async fn execute(&self, context: &ExecutionContext) -> ExecutorOutput {
        let spec = context.spec();
        context
            .execution()
            .starting(Some("preparing OCI workload".into()))
            .await;

        let prepared = async {
            let rootfs = if let Some(rootfs) = &self.settings.rootfs {
                rootfs.clone()
            } else {
                let image = self.settings.image.clone();
                tokio::task::spawn_blocking(move || {
                    let prepared = crate::image::oci::ImageStore::new(None)?.prepare(&image)?;
                    Ok::<_, anyhow::Error>(prepared.rootfs)
                })
                .await??
            };
            // Keep private state out of caller-controlled TMPDIR and standard host-root binds.
            let temporary = tempfile::Builder::new()
                .prefix("pvisor-oci-")
                .tempdir_in("/tmp")?;
            let launch = self.build_command(
                spec,
                context.execution_id().as_str(),
                temporary.path(),
                &rootfs,
            )?;
            Ok::<_, anyhow::Error>((temporary, launch))
        }
        .await;
        let (
            _temporary,
            Launch {
                mut command,
                state_root,
                pid_file,
                name,
            },
        ) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                return failed_to_start(error.to_string());
            }
        };
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return failed_to_start(error.to_string());
            }
        };
        let stdout_task = child.stdout.take().map(|stdout| {
            let limit = spec.runtime.max_output_bytes;
            tokio::spawn(async move { read_limited(stdout, limit).await })
        });
        let stderr_task = child.stderr.take().map(|stderr| {
            let limit = spec.runtime.max_output_bytes;
            tokio::spawn(async move { read_limited(stderr, limit).await })
        });
        context.execution().ready().await;

        let end = context
            .wait_child(&mut child, spec.runtime.timeout_ms)
            .await;

        let mut warnings = Vec::new();
        if matches!(end, End::Cancelled | End::Deadline)
            && let Some(warning) = self
                .terminate(
                    &mut child,
                    &state_root,
                    &name,
                    spec.runtime.termination_grace_ms,
                )
                .await
        {
            warnings.push(warning);
        }

        let (stdout, stderr) = tokio::join!(
            join_capture_bounded(stdout_task),
            join_capture_bounded(stderr_task)
        );
        let mut output = ProcessOutput::default();
        if let Some(captured) = stdout {
            output.stdout = Some(captured.text);
            output.stdout_truncated = captured.truncated;
        }
        if let Some(captured) = stderr {
            output.stderr = Some(captured.text);
            output.stderr_truncated = captured.truncated;
        }
        let (state, exit_code, failure) = match end {
            End::Cancelled => (RunState::Cancelled, None, None),
            End::Deadline => (
                RunState::Failed,
                None,
                Some(RunFailure {
                    kind: RunFailureKind::DeadlineExceeded,
                    message: "OCI workload exceeded its execution deadline".into(),
                    retryable: false,
                }),
            ),
            // The runtime writes this outside all container mounts. Exit codes
            // such as 125 are also valid workload exit codes.
            End::Exited(Ok(status)) if !pid_file.is_file() => (
                RunState::Failed,
                status.code().filter(|&code| code != 0),
                Some(RunFailure {
                    kind: RunFailureKind::Infrastructure,
                    message: "OCI runtime exited without reporting a container init PID".into(),
                    retryable: false,
                }),
            ),
            End::Exited(Ok(status)) => crate::executor::exit_outcome(status),
            End::Exited(Err(error)) => (
                RunState::Failed,
                None,
                Some(RunFailure {
                    kind: RunFailureKind::Infrastructure,
                    message: error.to_string(),
                    retryable: true,
                }),
            ),
        };
        ExecutorOutput {
            executor_observations: Default::default(),

            state,

            exit_code,
            failure,
            output,
            value: None,
            metrics: Default::default(),
            artifacts: Vec::new(),
            event_stream_ref: None,
            warnings,
        }
    }
}

fn process_limits(limits: &ResourceLimits) -> Vec<serde_json::Value> {
    [
        ("RLIMIT_AS", limits.memory_bytes),
        ("RLIMIT_NPROC", limits.processes),
        (
            "RLIMIT_CPU",
            limits.cpu_time_ms.map(|ms| ms.div_ceil(1000).max(1)),
        ),
        ("RLIMIT_NOFILE", limits.open_files),
        ("RLIMIT_FSIZE", limits.file_size_bytes),
    ]
    .into_iter()
    .filter_map(|(kind, value)| {
        value.map(|value| serde_json::json!({"type":kind,"soft":value,"hard":value}))
    })
    .collect()
}

fn failed_to_start(message: String) -> ExecutorOutput {
    ExecutorOutput {
        executor_observations: Default::default(),

        state: RunState::Failed,

        exit_code: None,
        failure: Some(RunFailure {
            kind: RunFailureKind::Spawn,
            message,
            retryable: false,
        }),
        output: ProcessOutput::default(),
        value: None,
        metrics: Default::default(),
        artifacts: Vec::new(),
        event_stream_ref: None,
        warnings: Vec::new(),
    }
}

fn validate_mount(mount: &ContainerMount) -> anyhow::Result<()> {
    anyhow::ensure!(
        !mount.source.as_os_str().is_empty(),
        "container mount source must not be empty"
    );
    anyhow::ensure!(
        mount.target.is_absolute(),
        "container mount target must be absolute: {}",
        mount.target.display()
    );
    validate_mount_path(&mount.target)
}

fn bind_mount(source: &Path, target: &Path, read_only: bool) -> anyhow::Result<BindMount> {
    let source = source.canonicalize().map_err(|error| {
        anyhow::anyhow!("resolve container mount {}: {error}", source.display())
    })?;
    anyhow::ensure!(
        target.is_absolute(),
        "container mount target must be absolute"
    );
    validate_mount_path(&source)?;
    validate_mount_path(target)?;
    Ok(BindMount {
        source,
        target: target.to_path_buf(),
        read_only,
    })
}

fn add_mount(mounts: &mut BTreeMap<PathBuf, BindMount>, mount: BindMount) -> anyhow::Result<()> {
    if let Some(existing) = mounts.get(&mount.target) {
        anyhow::ensure!(
            existing == &mount,
            "conflicting container mounts for {}",
            mount.target.display()
        );
        return Ok(());
    }
    mounts.insert(mount.target.clone(), mount);
    Ok(())
}

fn validate_mount_path(path: &Path) -> anyhow::Result<()> {
    let value = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("container mount path is not UTF-8"))?;
    anyhow::ensure!(
        !value.contains([',', '\n', '\r']),
        "container mount path contains an unsupported delimiter: {}",
        path.display()
    );
    Ok(())
}

fn absolute_container_path(path: &Path) -> anyhow::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

fn valid_env_name(key: &str) -> bool {
    !key.is_empty() && !key.contains(['=', '\0'])
}

fn mapping_entries(requested: u32, host: u32, namespace_root: u32) -> Vec<serde_json::Value> {
    if requested == 0 {
        return vec![serde_json::json!({"containerID":0,"hostID":host,"size":1})];
    }
    vec![
        serde_json::json!({"containerID":0,"hostID":namespace_root,"size":1}),
        serde_json::json!({"containerID":requested,"hostID":host,"size":1}),
    ]
}

fn subordinate_id(entries: &str, username: &str, owner: u32, host: u32) -> Option<u32> {
    let owner = owner.to_string();
    entries.lines().find_map(|line| {
        let mut fields = line.split(':');
        let account = fields.next()?;
        if account != username && account != owner {
            return None;
        }
        let start = fields.next()?.parse::<u32>().ok()?;
        let count = fields.next()?.parse::<u32>().ok()?;
        if fields.next().is_some() || count == 0 || start.checked_add(count - 1).is_none() {
            return None;
        }
        if start != host {
            Some(start)
        } else if count > 1 {
            start.checked_add(1)
        } else {
            None
        }
    })
}

fn authorized_subordinate_id(host: u32, path: &Path) -> anyhow::Result<u32> {
    let owner = unsafe { libc::geteuid() };
    // Use the system account database, rather than an inherited USER variable,
    // to find the owner of the kernel-authorized subordinate ranges.
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; 16384];
    let status = unsafe {
        libc::getpwuid_r(
            owner,
            &mut passwd,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    let username = if status == 0 && !result.is_null() {
        unsafe { std::ffi::CStr::from_ptr(passwd.pw_name) }.to_str()?
    } else {
        ""
    };
    let entries = fs::read_to_string(path).map_err(|error| {
        anyhow::anyhow!(
            "non-root container user requires {}: {error}",
            path.display()
        )
    })?;
    let root = subordinate_id(&entries, username, owner, host).ok_or_else(|| {
        anyhow::anyhow!(
            "non-root container user requires an authorized range in {} for UID {owner}",
            path.display()
        )
    })?;
    Ok(root)
}

fn parse_user(value: Option<&str>) -> anyhow::Result<(u32, u32)> {
    let Some(value) = value else {
        return Ok((0, 0));
    };
    let mut parts = value.split(':');
    let uid: u32 = parts
        .next()
        .unwrap_or("0")
        .parse()
        .map_err(|_| anyhow::anyhow!("container user must be uid[:gid]"))?;
    let gid: u32 = parts
        .next()
        .unwrap_or("0")
        .parse()
        .map_err(|_| anyhow::anyhow!("container user must be uid[:gid]"))?;
    anyhow::ensure!(parts.next().is_none(), "container user must be uid[:gid]");
    Ok((uid, gid))
}

fn container_name(run_id: &str, execution_id: &str) -> String {
    fn clean(value: &str) -> String {
        value
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                    ch
                } else {
                    '-'
                }
            })
            .collect()
    }
    let run = clean(run_id).chars().take(32).collect::<String>();
    let execution = clean(execution_id);
    let suffix = execution
        .chars()
        .rev()
        .take(24)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("pvisor-{run}-{suffix}")
}

#[cfg(test)]
mod tests {

    #[test]
    fn control_storage_cannot_be_reexposed_through_a_user_mount() {
        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("image");
        fs::create_dir(&image).unwrap();
        let control = temp.path().join("private");
        fs::create_dir(&control).unwrap();
        let spec = RunSpec::process("private-control", "test", "true");
        let executor = ContainerExecutor::new(ContainerSettings {
            rootfs: Some(image.clone()),
            mounts: vec![ContainerMount {
                source: control.clone(),
                target: "/exposed".into(),
                read_only: false,
            }],
            ..Default::default()
        })
        .unwrap();
        let error = executor
            .build_command(&spec, "execution", &control, &image)
            .unwrap_err();
        assert!(
            error.to_string().contains("host control storage"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn direct_transcript_obeys_host_output_limit_at_utf8_boundaries() {
        let stdout = read_limited("a界b".as_bytes(), 2).await.unwrap();
        let stderr = read_limited("abcdef".as_bytes(), 2).await.unwrap();
        assert_eq!(stdout.text, "a");
        assert_eq!(stderr.text, "ab");
        assert!(stdout.truncated && stderr.truncated);
    }

    #[tokio::test]
    async fn output_capture_has_a_deadline_when_a_descendant_keeps_the_pipe_open() {
        let task = tokio::spawn(std::future::pending::<std::io::Result<Captured>>());
        let abort = task.abort_handle();
        assert!(join_capture_bounded(Some(task)).await.unwrap().truncated);
        tokio::task::yield_now().await;
        assert!(abort.is_finished());
    }

    #[test]
    fn bundle_uses_a_private_root_and_correct_file_mountpoints() {
        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("image");
        fs::create_dir(&image).unwrap();
        fs::write(image.join("unchanged"), b"cache").unwrap();
        let file = temp.path().join("input");
        fs::write(&file, b"mount source").unwrap();
        let control = temp.path().join("private");
        let executor = ContainerExecutor::new(ContainerSettings {
            rootfs: Some(image.clone()),
            mounts: vec![ContainerMount {
                source: file,
                target: "/input".into(),
                read_only: true,
            }],
            ..Default::default()
        })
        .unwrap();
        let spec = RunSpec::process("r", "a", "true");
        executor
            .build_command(&spec, "a", &control, &image)
            .unwrap();
        let private = control.join("bundle/rootfs");
        assert!(private.join("input").is_file());
        assert!(!image.join("input").exists());
        fs::write(private.join("unchanged"), b"changed").unwrap();
        assert_eq!(fs::read(image.join("unchanged")).unwrap(), b"cache");
        let command = executor
            .runtime_command(Path::new("/private-state"))
            .unwrap();
        assert_eq!(
            command.as_std().get_args().collect::<Vec<_>>(),
            vec![
                std::ffi::OsStr::new("--root"),
                std::ffi::OsStr::new("/private-state")
            ]
        );
    }
    use super::*;
    use crate::config::{ContainerNetwork, ContainerPlatform};
    use std::ffi::OsStr;

    #[test]
    fn bundle_runs_workload_directly_with_host_prepared_drivers() {
        let temporary = tempfile::tempdir().unwrap();
        let rootfs = temporary.path().join("rootfs");
        fs::create_dir(&rootfs).unwrap();
        let control = temporary.path().join("private");
        let cwd = temporary.path().join("workspace");
        fs::create_dir(&cwd).unwrap();
        let capture = temporary.path().join("capture.json");
        fs::write(&capture, b"{}").unwrap();
        let executor = ContainerExecutor::new(ContainerSettings {
            rootfs: Some(rootfs.clone()),
            pvisor_binary: Some("/not-used".into()),
            network: ContainerNetwork::Host,
            ..ContainerSettings::default()
        })
        .unwrap();
        let mut spec = pvisor_core::RunSpec::process("run-one", "agent", "secret-agent");
        spec.runtime.resource_limits = ResourceLimits {
            memory_bytes: Some(1_048_576),
            processes: Some(8),
            open_files: Some(32),
            ..ResourceLimits::default()
        };
        let RunInvocation::Process(invocation) = &mut spec.invocation;
        invocation.cwd = Some(cwd.display().to_string());
        invocation.inherit_env = false;
        invocation.args = vec!["literal ;$ argument".into()];
        invocation
            .env
            .insert("HTTP_PROXY".into(), "http://127.0.0.1:8080".into());
        invocation
            .env
            .insert(CAPTURE_CONFIG_ENV.into(), capture.display().to_string());
        invocation
            .env
            .insert("PVISOR_AGENTCTL_TOKEN".into(), "not-for-oci".into());
        let launch = executor
            .build_command(&spec, "execution-one", &control, &rootfs)
            .unwrap();
        let args = launch
            .command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            args.iter()
                .any(|arg| arg == &control.join("bundle").display().to_string())
        );
        assert!(!args.iter().any(|arg| arg == "secret-agent"));
        let config: serde_json::Value =
            serde_json::from_slice(&fs::read(control.join("bundle/config.json")).unwrap()).unwrap();
        assert_eq!(
            config["process"]["args"],
            serde_json::json!(["secret-agent", "literal ;$ argument"])
        );
        let env = config["process"]["env"].as_array().unwrap();
        assert!(
            env.iter()
                .any(|entry| entry == "HTTP_PROXY=http://127.0.0.1:8080")
        );
        assert!(
            env.iter()
                .all(|entry| !entry.as_str().unwrap().starts_with("PVISOR_AGENTCTL_"))
        );
        assert_eq!(config["process"]["cwd"], serde_json::json!(cwd));
        assert_eq!(
            config["process"]["rlimits"],
            serde_json::json!([
                {"type":"RLIMIT_AS","soft":1_048_576,"hard":1_048_576},
                {"type":"RLIMIT_NPROC","soft":8,"hard":8},
                {"type":"RLIMIT_NOFILE","soft":32,"hard":32}
            ])
        );
        assert_eq!(config["linux"]["resources"]["memory"]["limit"], 1_048_576);
        assert_eq!(config["linux"]["resources"]["pids"]["limit"], 8);
        let mounts = config["mounts"].as_array().unwrap();
        let bind_mounts = mounts
            .iter()
            .filter(|m| m["type"] == "bind")
            .collect::<Vec<_>>();
        assert_eq!(bind_mounts.len(), 2);
        assert!(
            bind_mounts
                .iter()
                .any(|m| m["source"] == serde_json::json!(capture) && m["options"][1] == "ro")
        );
        assert!(
            bind_mounts
                .iter()
                .any(|m| m["source"] == serde_json::json!(cwd) && m["options"][1] == "rw")
        );
    }

    #[test]
    fn descriptor_reports_container_without_overclaiming_enforcement() {
        let executor = ContainerExecutor::new(ContainerSettings {
            image: "example/agent:latest".into(),
            ..ContainerSettings::default()
        })
        .unwrap();
        let descriptor = executor.descriptor();
        assert_eq!(descriptor.name, "oci-pvisor");
        assert_eq!(descriptor.kind, ExecutorKind::Container);
        assert_eq!(descriptor.isolation, IsolationKind::Container);
        assert!(descriptor.capability_plan.dimensions.is_empty());
    }

    #[test]
    fn explicit_platform_requires_native_architecture_for_images_and_rootfs() {
        for platform in [ContainerPlatform::LinuxAmd64, ContainerPlatform::LinuxArm64] {
            for rootfs in [None, Some(PathBuf::from("/prepared-rootfs"))] {
                let result = ContainerExecutor::new(ContainerSettings {
                    image: "example/agent:latest".into(),
                    rootfs,
                    platform: Some(platform),
                    pvisor_binary: Some(PathBuf::from("/custom-pvisor")),
                    ..Default::default()
                });
                let native = matches!(
                    (std::env::consts::ARCH, platform),
                    ("x86_64", ContainerPlatform::LinuxAmd64)
                        | ("aarch64", ContainerPlatform::LinuxArm64)
                );
                if native {
                    assert_eq!(result.unwrap().settings().platform, Some(platform));
                } else {
                    assert!(
                        result
                            .unwrap_err()
                            .to_string()
                            .contains("cross-platform selection is not supported")
                    );
                }
            }
        }
    }

    #[test]
    fn accepts_numeric_container_user() {
        assert!(
            ContainerExecutor::new(ContainerSettings {
                image: "agent".into(),
                user: Some("1000".into()),
                ..ContainerSettings::default()
            })
            .is_ok()
        );
    }

    #[test]
    fn non_root_mappings_preserve_workload_identity_and_mount_ownership() {
        assert_eq!(
            mapping_entries(1000, 1234, 524288),
            vec![
                serde_json::json!({"containerID":0,"hostID":524288,"size":1}),
                serde_json::json!({"containerID":1000,"hostID":1234,"size":1}),
            ]
        );
        assert_eq!(mapping_entries(0, 1234, 524288).len(), 1);
        assert_eq!(
            subordinate_id("other:1:65536\nuser:524288:65536", "user", 1234, 1234),
            Some(524288)
        );
        assert_eq!(
            subordinate_id("1234:524288:1", "user", 1234, 1234),
            Some(524288)
        );
        assert_eq!(subordinate_id("user:1234:1", "user", 1234, 1234), None);
        assert_eq!(
            subordinate_id("user:1234:2", "user", 1234, 1234),
            Some(1235)
        );
        assert_eq!(
            subordinate_id("user:4294967295:2", "user", 1234, 1234),
            None
        );
        assert_eq!(
            subordinate_id("other:524288:65536", "user", 1234, 1234),
            None
        );
    }

    #[test]
    fn runtime_name_is_path_safe_and_retains_execution_entropy() {
        let name = container_name("run/unsafe", "execution:1234567890");
        assert!(!name.contains('/'));
        assert!(!name.contains(':'));
        assert!(name.ends_with("1234567890"));
        assert_ne!(
            container_name("run", "execution-one"),
            container_name("run", "execution-two")
        );
        assert_ne!(OsStr::new(&name), OsStr::new(""));
    }
}
