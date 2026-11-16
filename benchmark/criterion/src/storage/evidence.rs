//! Private storage experiment evidence, commands, and exact disk inventories.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    os::fd::{AsRawFd, RawFd},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    pub output: PathBuf,
    #[serde(default)]
    pub binary: PathBuf,
    #[serde(default)]
    pub daemon: PathBuf,
    #[serde(default)]
    pub inputs: PathBuf,
    #[serde(default)]
    pub baseline: PathBuf,
    #[serde(default)]
    pub candidate: PathBuf,
    #[serde(default)]
    pub baseline_runner: PathBuf,
    #[serde(default = "files")]
    pub files: usize,
    #[serde(default = "one")]
    pub directories: usize,
    #[serde(default = "pool")]
    pub backend: String,
    #[serde(default = "repeat")]
    pub payload: String,
    #[serde(default)]
    pub cpu: Option<usize>,
    #[serde(default = "shell")]
    pub workload: String,
    #[serde(default = "combined")]
    pub image: String,
    /// Selected real observation: `pvisor.ready_ns`, `docker.allocated`, etc.
    pub metric: String,
    /// Worker executable built from storage/main.rs; never a Python adapter.
    pub worker: PathBuf,
}
fn files() -> usize {
    10000
}
fn one() -> usize {
    1
}
fn pool() -> String {
    "pool".into()
}
fn repeat() -> String {
    "repeat".into()
}
fn shell() -> String {
    "shell".into()
}
fn combined() -> String {
    "combined".into()
}
pub(super) fn load(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(
        &fs::read(path).with_context(|| path.display().to_string())?,
    )?)
}
pub(super) fn save(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
pub(super) fn sha(path: &Path) -> Result<String> {
    let mut stream = File::open(path)?;
    let mut digest = Sha256::new();
    let mut block = [0; 1024 * 1024];
    loop {
        let n = stream.read(&mut block)?;
        if n == 0 {
            break;
        }
        digest.update(&block[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub(super) fn unique() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}
pub(super) fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).to_owned()).collect()
}
pub(super) fn s(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(super) struct Evidence {
    pub root: PathBuf,
    pub binary: PathBuf,
    pub images: Vec<Value>,
    pub manifest: Value,
    pub number: usize,
    pub rng: Rng,
}
impl Evidence {
    pub fn new(config: &Config, entry: &str) -> Result<Self> {
        ensure!(
            config.output.components().any(|c| c.as_os_str() == ".data"),
            "new evidence must be under .data"
        );
        fs::create_dir_all(config.output.parent().context("output parent")?)?;
        fs::create_dir(&config.output).context("refuse existing evidence batch")?;
        fs::set_permissions(&config.output, fs::Permissions::from_mode(0o700))?;
        let root = fs::canonicalize(&config.output)?;
        let binary = root.join("pvisor");
        if !config.binary.as_os_str().is_empty() {
            fs::copy(&config.binary, &binary)?;
        }
        let mut e = Self {
            root,
            binary,
            images: vec![],
            number: 0,
            rng: Rng(20261026),
            manifest: json!({"entry":entry, "status":"preparing", "sampling":"Criterion 0.7; calibration and warmup retained as raw trials; no fixed-count historical compatibility", "config":config, "seed":20261026, "rng":"splitmix64-v1", "cache":"host page cache retained; new private instances"}),
        };
        e.persist()?;
        save(&e.root.join("config.json"), config)?;
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/storage");
        fs::create_dir(e.root.join("harness"))?;
        let mut hashes = BTreeMap::new();
        for path in walk(&source_root)? {
            if path.extension().is_some_and(|x| x == "rs") {
                let destination = e.root.join("harness").join(path.file_name().unwrap());
                fs::copy(&path, &destination)?;
                hashes.insert(s(&path), sha(&path)?);
            }
        }
        ensure!(
            !hashes.is_empty(),
            "storage harness sources unavailable at build manifest directory"
        );
        e.manifest["harness"] = json!(hashes);
        e.manifest["worker_sha256"] = json!(sha(&config.worker)?);
        fs::copy(&config.worker, e.root.join("storage-worker"))?;
        let mut magic = [0; 4];
        File::open(e.root.join("storage-worker"))?.read_exact(&mut magic)?;
        ensure!(
            &magic == b"\x7fELF",
            "storage-worker must be the compiled Linux Rust executable"
        );
        let mut tools = BTreeMap::new();
        for name in [
            "docker",
            "dockerd",
            "dockerd-rootless.sh",
            "rootlesskit",
            "pasta",
            "runc",
            "containerd",
            "openssl",
            "unshare",
            "nsenter",
            "taskset",
            "systemd-run",
            "systemctl",
            "busctl",
            "rustfmt",
        ] {
            if let Some(path) = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|p| p.join(name))
                .find(|p| p.is_file())
            {
                tools.insert(name, json!({"path":path,"sha256":sha(&path)?}));
            }
        }
        e.manifest["tools"] = json!(tools);
        e.manifest["host"] =
            json!(e.command(args(&["uname", "-a"]), &BTreeMap::new(), 10, false)?);
        e.manifest["source_revision"] = json!(e.command(
            args(&["git", "rev-parse", "HEAD"]),
            &BTreeMap::new(),
            10,
            false
        )?);
        e.manifest["source_patch"] = json!(e.command(
            args(&["git", "diff", "HEAD", "--binary"]),
            &BTreeMap::new(),
            30,
            false
        )?);
        e.snapshot("before")?;
        if e.binary.exists() {
            e.manifest["binary_sha256"] = json!(sha(&e.binary)?);
        }
        e.persist()?;
        Ok(e)
    }
    pub fn persist(&self) -> Result<()> {
        save(&self.root.join("manifest.json"), &self.manifest)
    }
    pub fn freeze_files(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> Result<()> {
        if !self.manifest["frozen_inputs"].is_object() {
            self.manifest["frozen_inputs"] = json!({});
        }
        for path in paths {
            if fs::symlink_metadata(&path)?.is_file() {
                self.manifest["frozen_inputs"][s(&path)] = json!(sha(&path)?);
            }
        }
        self.persist()
    }
    pub fn cleanup_error(&mut self, error: anyhow::Error) {
        if !self.manifest["cleanup_errors"].is_array() {
            self.manifest["cleanup_errors"] = json!([]);
        }
        self.manifest["cleanup_errors"]
            .as_array_mut()
            .unwrap()
            .push(json!(format!("{error:#}")));
        let _ = self.persist();
    }
    pub fn finish(&mut self, result: &Result<()>) -> Result<()> {
        let verified = (|| -> Result<()> {
            self.snapshot("after")?;
            if self.binary.exists() {
                ensure!(
                    self.manifest["binary_sha256"] == sha(&self.binary)?,
                    "frozen binary changed"
                );
            }
            ensure!(
                self.manifest["worker_sha256"] == sha(&self.root.join("storage-worker"))?,
                "frozen worker changed"
            );
            for table in ["harness", "frozen_inputs"] {
                if let Some(hashes) = self.manifest[table].as_object() {
                    for (path, expected) in hashes {
                        ensure!(
                            sha(Path::new(path))? == expected.as_str().context("input hash")?,
                            "frozen source/input changed: {path}"
                        );
                    }
                }
            }
            ensure!(
                !self.manifest["cleanup_errors"]
                    .as_array()
                    .is_some_and(|errors| !errors.is_empty()),
                "owned service cleanup failed; retained cleanup_errors"
            );
            Ok(())
        })();
        self.manifest["status"] = json!(if result.is_ok() && verified.is_ok() {
            "complete"
        } else {
            "failed"
        });
        if let Some(error) = result.as_ref().err().or_else(|| verified.as_ref().err()) {
            self.manifest["error"] = json!(format!("{error:#}"));
        }
        self.persist()?;
        verified
    }
    pub fn snapshot(&mut self, label: &str) -> Result<()> {
        let data = self.command(
            args(&["ps", "-eo", "pid,ppid,psr,pcpu,etime,args"]),
            &BTreeMap::new(),
            10,
            false,
        )?;
        fs::write(self.root.join(format!("processes-{label}.txt")), data)?;
        if let Ok(data) = fs::read("/proc/loadavg") {
            fs::write(self.root.join(format!("load-{label}.txt")), data)?;
        }
        if let Ok(data) = fs::read("/proc/self/status") {
            fs::write(self.root.join(format!("status-{label}.txt")), data)?;
        }
        Ok(())
    }
    pub fn command(
        &mut self,
        argv: Vec<String>,
        env: &BTreeMap<String, String>,
        timeout: u64,
        allow_failure: bool,
    ) -> Result<String> {
        self.number += 1;
        let directory = self
            .root
            .join("commands")
            .join(format!("{:06}", self.number));
        fs::create_dir_all(&directory)?;
        let result = capture(&argv, env, &directory, timeout, false)?;
        ensure!(
            allow_failure || result["exit_code"] == 0,
            "command failed: {argv:?}; evidence {}",
            directory.display()
        );
        Ok(fs::read_to_string(directory.join("stdout"))?)
    }
    pub fn prepare_inputs(&mut self, config: &Config) -> Result<()> {
        self.command(
            args(&["chattr", "+m", &s(&self.root)]),
            &BTreeMap::new(),
            10,
            false,
        )?;
        let flags = self.command(
            args(&["lsattr", "-d", &s(&self.root)]),
            &BTreeMap::new(),
            10,
            false,
        )?;
        ensure!(
            flags.split_whitespace().next().unwrap_or("").contains('m'),
            "Btrfs no-compression flag absent"
        );
        self.manifest["directory_flags"] = json!(flags);
        self.manifest["filesystem"] = json!(self.command(
            args(&["findmnt", "-no", "FSTYPE,OPTIONS", "-T", &s(&self.root)]),
            &BTreeMap::new(),
            10,
            false
        )?);
        let values: Vec<Value> = serde_json::from_slice(&fs::read(&config.inputs)?)?;
        ensure!(!values.is_empty(), "no OCI inputs");
        let blobs = self.root.join("sources");
        fs::create_dir(&blobs)?;
        for mut image in values {
            let digest = digest_hex(image["digest"].as_str().context("digest")?)?;
            ensure!(
                image["pinned"]
                    .as_str()
                    .context("pinned reference")?
                    .ends_with(&format!("@sha256:{digest}")),
                "reference not pinned to manifest"
            );
            let source = PathBuf::from(image["source"].as_str().context("source")?);
            let manifest = image["manifest_path"]
                .as_str()
                .map(PathBuf::from)
                .unwrap_or(source.join("manifest.json"));
            ensure!(
                sha(&manifest)? == digest && load(&manifest)? == image["manifest"],
                "manifest digest/parsed content mismatch"
            );
            let mut descriptors = vec![image["manifest"]["config"].clone()];
            descriptors.extend(
                image["manifest"]["layers"]
                    .as_array()
                    .context("layers")?
                    .clone(),
            );
            for descriptor in descriptors {
                let key = digest_hex(descriptor["digest"].as_str().context("blob digest")?)?;
                let path = source.join(&key);
                ensure!(
                    fs::metadata(&path)?.len()
                        == descriptor["size"].as_u64().context("blob size")?
                        && sha(&path)? == key,
                    "OCI blob mismatch"
                );
                if !blobs.join(&key).exists() {
                    fs::copy(&path, blobs.join(&key))?;
                }
            }
            let config_digest = digest_hex(
                image["manifest"]["config"]["digest"]
                    .as_str()
                    .context("config digest")?,
            )?;
            let config = load(&blobs.join(config_digest))?;
            ensure!(
                config["architecture"] == "amd64" && config["os"] == "linux",
                "input must be linux/amd64"
            );
            fs::copy(manifest, blobs.join(&digest))?;
            image["source"] = json!(blobs);
            image["manifest_path"] = json!(blobs.join(digest));
            self.images.push(image);
        }
        save(&self.root.join("inputs.json"), &self.images)?;
        self.freeze_files(walk(&blobs)?)?;
        Ok(())
    }
    pub fn trial(&mut self, label: &str) -> Result<PathBuf> {
        ensure!(
            !label.is_empty()
                && label.len() <= 128
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)),
            "unsafe evidence label"
        );
        self.number += 1;
        let path = self.root.join(format!("{:06}-{label}", self.number));
        fs::create_dir(&path)?;
        Ok(path)
    }
}
pub(super) fn digest_hex(value: &str) -> Result<String> {
    let hex = value
        .strip_prefix("sha256:")
        .context("SHA-256 descriptor required")?;
    ensure!(
        hex.len() == 64
            && hex
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "invalid digest"
    );
    Ok(hex.into())
}
/// A waitable leader anchors its PID/PGID until every group signal is finished.
/// No caller gets access to `Child::try_wait`, which would release that anchor.
pub(super) struct OwnedChild {
    child: Option<Child>,
}
impl OwnedChild {
    pub fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }
    pub fn id(&self) -> u32 {
        self.child.as_ref().expect("unreaped child").id()
    }
    pub fn stdin(&mut self) -> Option<ChildStdin> {
        self.child.as_mut()?.stdin.take()
    }
    pub fn stdout(&mut self) -> Option<ChildStdout> {
        self.child.as_mut()?.stdout.take()
    }
    pub fn stderr(&mut self) -> Option<ChildStderr> {
        self.child.as_mut()?.stderr.take()
    }
    pub fn exited(&self) -> std::io::Result<bool> {
        let Some(child) = &self.child else {
            return Ok(true);
        };
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        loop {
            let rc = unsafe {
                libc::waitid(
                    libc::P_PID,
                    child.id(),
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if rc == 0 {
                return Ok(unsafe { info.si_pid() } != 0);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
    fn signal(&self, signal: i32) -> std::io::Result<()> {
        if self.child.is_none() {
            return Ok(());
        }
        // WNOWAIT proves this is still our unreaped child immediately before
        // using the numerical PGID; even a zombie prevents PID/PGID reuse.
        self.exited()?;
        if unsafe { libc::kill(-(self.id() as i32), signal) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }
    pub fn finish(&mut self) -> Result<ExitStatus> {
        ensure!(self.exited()?, "cannot reap live child");
        self.signal(libc::SIGKILL)?;
        // This wait cannot block: WNOWAIT observed an exit, and nobody else can
        // reap the private Child. No group signals occur after taking it.
        Ok(self.child.take().context("child already reaped")?.wait()?)
    }
    fn terminate(&mut self, grace: Duration) -> Result<ExitStatus> {
        self.signal(libc::SIGTERM)?;
        let deadline = Instant::now() + grace;
        while !self.exited()? && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        // Always signal the group, even if its leader exited before descendants.
        self.signal(libc::SIGKILL)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.exited()? {
            ensure!(
                Instant::now() < deadline,
                "child did not become waitable after SIGKILL"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        self.finish()
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = self.terminate(Duration::ZERO);
            if let Some(mut child) = self.child.take() {
                // An uninterruptible child may not exit within our deadline.
                // Transfer only reaping to a background owner: no later signal
                // uses its numeric ID, and the caller never blocks indefinitely.
                let _ = std::thread::Builder::new()
                    .name("storage-child-reaper".into())
                    .spawn(move || {
                        let _ = child.wait();
                    });
            }
        }
    }
}
fn command(argv: &[String], env: &BTreeMap<String, String>) -> Result<Command> {
    ensure!(!argv.is_empty(), "empty command");
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    for (key, _) in std::env::vars()
        .filter(|(k, _)| k.starts_with("PVISOR_") || k == "DOCKER_HOST" || k == "DOCKER_CONTEXT")
    {
        command.env_remove(key);
    }
    command.envs(env);
    Ok(command)
}
/// Ownership is installed immediately after spawn, before any fallible setup.
pub(super) fn spawn(argv: &[String], env: &BTreeMap<String, String>) -> Result<OwnedChild> {
    Ok(OwnedChild::new(
        command(argv, env)?
            .spawn()
            .with_context(|| format!("spawn {argv:?}"))?,
    ))
}
/// Open both logs before starting a service; no pipe-copy threads are needed.
pub(super) fn spawn_logged(
    argv: &[String],
    env: &BTreeMap<String, String>,
    stdout: &Path,
    stderr: &Path,
) -> Result<OwnedChild> {
    let out = File::create(stdout)?;
    let err = File::create(stderr)?;
    let mut command = command(argv, env)?;
    command.stdout(out).stderr(err);
    Ok(OwnedChild::new(
        command.spawn().with_context(|| format!("spawn {argv:?}"))?,
    ))
}
pub(super) fn stop(child: &mut OwnedChild) -> Result<()> {
    if child.child.is_some() {
        child.terminate(Duration::from_secs(20))?;
    }
    Ok(())
}
pub(super) fn nonblocking(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "set nonblocking pipe: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}
pub(super) fn capture(
    argv: &[String],
    env: &BTreeMap<String, String>,
    directory: &Path,
    timeout: u64,
    ready: bool,
) -> Result<Value> {
    capture_with_drain(argv, env, directory, timeout, ready, Duration::from_secs(5))
}
fn capture_with_drain(
    argv: &[String],
    env: &BTreeMap<String, String>,
    directory: &Path,
    timeout: u64,
    ready: bool,
    drain_timeout: Duration,
) -> Result<Value> {
    fs::create_dir_all(directory)?;
    save(
        &directory.join("command.json"),
        &json!({"argv":argv,"env":env,"timeout_s":timeout,"ready_marker":if ready {Some("REFERENCE_READY")} else {None},"cwd":std::env::current_dir()?}),
    )?;
    let mut stdout_file = File::create(directory.join("stdout"))?;
    let mut stderr_file = File::create(directory.join("stderr"))?;
    let start = Instant::now();
    let mut child = spawn(argv, env)?;
    let mut out = child.stdout().context("stdout pipe")?;
    let mut err = child.stderr().context("stderr pipe")?;
    nonblocking(out.as_raw_fd())?;
    nonblocking(err.as_raw_fd())?;
    let deadline = start + Duration::from_secs(timeout);
    let mut drain_deadline = None;
    let mut eof = [false; 2];
    let mut pending = Vec::new();
    let mut markers = Vec::new();
    let (timed_out, drained) = loop {
        let exited = child.exited()?;
        if exited && drain_deadline.is_none() {
            drain_deadline = Some(Instant::now() + drain_timeout);
        }
        if exited && eof.iter().all(|v| *v) {
            break (false, true);
        }
        if Instant::now() >= deadline {
            break (true, false);
        }
        if drain_deadline.is_some_and(|end| Instant::now() >= end) {
            break (false, false);
        }
        let mut fds = [
            libc::pollfd {
                fd: if eof[0] { -1 } else { out.as_raw_fd() },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if eof[1] { -1 } else { err.as_raw_fd() },
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 2, 10) };
        if rc < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        for index in 0..2 {
            if eof[index] || fds[index].revents == 0 {
                continue;
            }
            // Bound each drain burst, so a noisy writer cannot starve deadlines.
            for _ in 0..16 {
                let mut block = [0u8; 65536];
                let received = if index == 0 {
                    out.read(&mut block)
                } else {
                    err.read(&mut block)
                };
                let n = match received {
                    Ok(0) => {
                        eof[index] = true;
                        break;
                    }
                    Ok(n) => n,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.into()),
                };
                let now = start.elapsed().as_nanos() as u64;
                if index == 0 {
                    stdout_file.write_all(&block[..n])?;
                    for &byte in &block[..n] {
                        if byte == b'\n' {
                            if pending == b"REFERENCE_READY" {
                                markers.push(now);
                            }
                            pending.clear();
                        } else {
                            pending.push(byte);
                            ensure!(
                                pending.len() <= 1024 * 1024,
                                "stdout line exceeds evidence limit"
                            );
                        }
                    }
                } else {
                    stderr_file.write_all(&block[..n])?;
                }
            }
        }
    };
    let status = if drained {
        child.finish()?
    } else {
        child.terminate(Duration::ZERO)?
    };
    // Dropping our read ends is finite even when an escaped process still holds
    // a write end. There are no reader threads to join or detach.
    drop(out);
    drop(err);
    // Completion follows output draining and normal wait, matching the Ready
    // observer protocol rather than a racing waitpid-only timestamp.
    let exit_ns = start.elapsed().as_nanos() as u64;
    let record = json!({"exit_code":status.code(),"timed_out":timed_out,"exit_ns":exit_ns,"ready_observations_ns":markers,"ready_ns":markers.first(),"clock":"host Instant; spawn through newline observation / normal exit"});
    save(&directory.join("observation.json"), &record)?;
    ensure!(drained, "command left inherited output pipes open");
    ensure!(
        !timed_out,
        "command timed out; retained {}",
        directory.display()
    );
    if ready {
        ensure!(
            status.success() && markers.len() == 1 && markers[0] <= exit_ns,
            "normal exit and one Ready required"
        );
    }
    Ok(record)
}
pub(super) fn walk(root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = vec![];
    if !root.exists() {
        return Ok(paths);
    }
    let mut children = fs::read_dir(root)?
        .map(|r| r.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    children.sort();
    for child in children {
        let metadata = fs::symlink_metadata(&child)?;
        paths.push(child.clone());
        if metadata.is_dir() {
            paths.extend(walk(&child)?);
        }
    }
    Ok(paths)
}
pub(super) fn inventory(root: &Path) -> Result<Value> {
    ensure!(root.is_dir(), "inventory root absent");
    let mut seen = BTreeSet::new();
    let mut rows = vec![];
    let mut apparent = 0u64;
    let mut allocated = 0u64;
    for path in std::iter::once(root.to_owned()).chain(walk(root)?) {
        let m = fs::symlink_metadata(&path)?;
        if !seen.insert((m.dev(), m.ino())) {
            continue;
        }
        apparent += m.len();
        allocated += m.blocks() * 512;
        rows.push(json!({"path":path.strip_prefix(root)?,"apparent":m.len(),"allocated":m.blocks()*512,"mode":m.mode(),"inode":m.ino(),"device":m.dev()}));
    }
    Ok(json!({"apparent":apparent,"allocated":allocated,"inodes":rows.len(),"rows":rows}))
}
pub(super) struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
    pub fn order(&mut self) -> [&'static str; 2] {
        if self.next() & 1 == 0 {
            ["docker", "pvisor"]
        } else {
            ["pvisor", "docker"]
        }
    }
}
pub(super) fn metric(value: &Value, name: &str) -> Result<f64> {
    let mut selected = value;
    for component in name.split('.') {
        selected = &selected[component];
    }
    let number = selected
        .as_f64()
        .with_context(|| format!("metric {name} missing from {value}"))?;
    if !number.is_finite() || number < 0. {
        bail!("invalid nonnegative metric");
    }
    Ok(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wait_until(mut predicate: impl FnMut() -> bool) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !predicate() {
            ensure!(Instant::now() < deadline, "test fixture deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
    #[test]
    fn exited_leader_stays_anchored_until_descendants_are_killed() -> Result<()> {
        let root = std::env::temp_dir().join(unique());
        fs::create_dir(&root)?;
        let pid_file = root.join("descendant");
        let mut child = spawn(
            &args(&[
                "/bin/sh",
                "-c",
                &format!(
                    "(trap '' TERM; echo ready > '{}'; exec /bin/sleep 30) & echo $! > '{}'; exit 0",
                    root.join("ready").display(),
                    pid_file.display()
                ),
            ]),
            &BTreeMap::new(),
        )?;
        wait_until(|| {
            pid_file.exists() && root.join("ready").exists() && child.exited().unwrap_or(false)
        })?;
        let descendant: u32 = fs::read_to_string(pid_file)?.trim().parse()?;
        let leader = child.id();
        // Multiple readiness checks must not reap the exited leader.
        assert!(child.exited()?);
        assert!(child.exited()?);
        assert!(fs::read_to_string(format!("/proc/{leader}/stat"))?.contains(") Z "));
        stop(&mut child)?;
        wait_until(|| {
            fs::read_to_string(format!("/proc/{descendant}/stat"))
                .map(|stat| stat.contains(") Z "))
                .unwrap_or(true)
        })?;
        assert!(!Path::new(&format!("/proc/{leader}")).exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn failed_log_creation_never_starts_a_subject() -> Result<()> {
        let root = std::env::temp_dir().join(unique());
        fs::create_dir(&root)?;
        let bad = root.join("stderr");
        fs::create_dir(&bad)?;
        let marker = root.join("started");
        let argv = args(&[
            "/bin/sh",
            "-c",
            &format!("touch '{}'; exec /bin/sleep 30", marker.display()),
        ]);
        assert!(spawn_logged(&argv, &BTreeMap::new(), &root.join("stdout"), &bad).is_err());
        assert!(capture(&argv, &BTreeMap::new(), &root, 1, false).is_err());
        assert!(!marker.exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn fallible_setup_after_spawn_is_guarded() -> Result<()> {
        let pid;
        {
            let child = spawn(&args(&["/bin/sleep", "30"]), &BTreeMap::new())?;
            pid = child.id();
            let failure = File::open("/no/such/storage-fixture-file");
            assert!(failure.is_err());
            // Scope exit must kill and reap, despite no explicit stop call.
        }
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        Ok(())
    }
    #[test]
    fn outside_group_pipe_holder_cannot_block_capture_cleanup() -> Result<()> {
        let root = std::env::temp_dir().join(unique());
        fs::create_dir(&root)?;
        let pid_file = root.join("leader");
        let release = root.join("release");
        let directory = root.join("capture");
        let script = format!(
            "echo $$ > '{}'; while test ! -e '{}'; do /bin/sleep 0.01; done; printf 'REFERENCE_READY\\n'",
            pid_file.display(),
            release.display()
        );
        let capture_thread = std::thread::spawn(move || {
            capture_with_drain(
                &args(&["/bin/sh", "-c", &script]),
                &BTreeMap::new(),
                &directory,
                5,
                true,
                Duration::from_millis(100),
            )
        });
        wait_until(|| pid_file.exists())?;
        let pid = fs::read_to_string(&pid_file)?;
        // Hold a duplicate write end outside the owned process group. This
        // models a setsid/escaped pipe holder without leaving an escaped process.
        // The shell cannot exit before our release file is written.
        let held = fs::OpenOptions::new()
            .write(true)
            .open(format!("/proc/{}/fd/1", pid.trim()))?;
        fs::write(&release, b"go")?;
        let start = Instant::now();
        let result = capture_thread.join().expect("capture fixture panic");
        assert!(result.is_err());
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "pipe drain was not bounded"
        );
        assert!(format!("{:#}", result.unwrap_err()).contains("inherited output pipes"));
        drop(held);
        assert_eq!(
            load(&root.join("capture/observation.json"))?["timed_out"],
            false
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn malformed_digest_rejected() {
        assert!(digest_hex("sha256:../bad").is_err());
        assert!(digest_hex(&format!("sha256:{}", "a".repeat(64))).is_ok());
    }
    #[test]
    fn metric_keeps_units() {
        assert_eq!(
            metric(&json!({"docker":{"allocated":4096}}), "docker.allocated").unwrap(),
            4096.
        );
        assert!(metric(&json!({"a":-1}), "a").is_err());
    }
    #[test]
    fn inventory_deduplicates_links_without_following_symlinks() -> Result<()> {
        let root = std::env::temp_dir().join(unique());
        fs::create_dir(&root)?;
        fs::write(root.join("a"), b"hello")?;
        fs::hard_link(root.join("a"), root.join("b"))?;
        std::os::unix::fs::symlink("/does/not/exist", root.join("c"))?;
        let observed = inventory(&root)?;
        assert_eq!(observed["inodes"], 3);
        fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn ready_capture_keeps_exact_marker_and_failure_evidence() -> Result<()> {
        let root = std::env::temp_dir().join(unique());
        let okay = root.join("okay");
        let observation = capture(
            &args(&["/bin/sh", "-c", "printf 'REFERENCE_READY\\n'"]),
            &BTreeMap::new(),
            &okay,
            5,
            true,
        )?;
        assert_eq!(observation["exit_code"], 0);
        assert!(
            observation["ready_ns"].as_u64().unwrap() <= observation["exit_ns"].as_u64().unwrap()
        );
        let duplicate = root.join("duplicate");
        assert!(
            capture(
                &args(&[
                    "/bin/sh",
                    "-c",
                    "printf 'REFERENCE_READY\\nREFERENCE_READY\\n'"
                ]),
                &BTreeMap::new(),
                &duplicate,
                5,
                true
            )
            .is_err()
        );
        assert!(duplicate.join("observation.json").is_file());
        assert_eq!(
            load(&duplicate.join("observation.json"))?["ready_observations_ns"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let nonzero = root.join("nonzero");
        assert!(
            capture(
                &args(&["/bin/sh", "-c", "printf 'REFERENCE_READY\\n'; exit 2"]),
                &BTreeMap::new(),
                &nonzero,
                5,
                true
            )
            .is_err()
        );
        fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn timeout_retains_failure_instead_of_a_sample() -> Result<()> {
        let root = std::env::temp_dir().join(unique());
        assert!(
            capture(
                &args(&["/bin/sleep", "2"]),
                &BTreeMap::new(),
                &root,
                0,
                true
            )
            .is_err()
        );
        assert_eq!(load(&root.join("observation.json"))?["timed_out"], true);
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
