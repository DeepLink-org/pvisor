//! B-LAZY-STARTUP, same host staged executor and immutable source files.
//! Measures lower open/rpull/mount through validated Ready and normal wrapper
//! exit, plus the mounted-only stage interval. Image programs are never run.

use anyhow::{Context, Result, ensure};
use criterion::{Criterion, SamplingMode};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env,
    fs::{self, File},
    io::{BufReader, Read},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use super::super::evidence::{digest, files, read, save};
use super::{lab::Lab, process, proxy::Proxy, source};

const SOURCE: &str = "sha256:88a381d5b5eeb2b35d3ad70925a362c37ce569daf43ede89ff818ec20e4d3794";
const FILES: [&str; 4] = [
    "usr/lib/os-release",
    "usr/bin/dash",
    "usr/lib/x86_64-linux-gnu/libc.so.6",
    "usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
];
const MARKER: &str = "STAGE_READY files 4 bytes 2575738";
const EXECUTABLE: &str = "/opt/pvisor-stage-bench/pvisor";
const MOUNTER: &str = "/opt/pvisor-stage-bench/lazy-stage-mount";

struct CacheService(Child, bool);
struct RestoreAffinity(Vec<usize>);
impl Drop for RestoreAffinity {
    fn drop(&mut self) {
        let _ = process::pin(&self.0);
    }
}
impl CacheService {
    fn stop(&mut self, evidence: &Path) -> Result<()> {
        if self.1 {
            return Ok(());
        }
        ensure!(
            self.0.try_wait()?.is_none(),
            "cache service exited before cohort teardown"
        );
        ensure!(
            unsafe { libc::kill(-(self.0.id() as i32), libc::SIGTERM) } == 0,
            "cache service termination failed"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.0.try_wait()? {
                use std::os::unix::process::ExitStatusExt;
                self.1 = true;
                save(
                    evidence,
                    &json!({"exit_code":status.code(),"signal":status.signal()}),
                )?;
                ensure!(
                    status.success() || status.signal() == Some(libc::SIGTERM),
                    "cache service teardown was unsuccessful: {status}"
                );
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "cache service did not terminate within deadline"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for CacheService {
    fn drop(&mut self) {
        if self.1 {
            return;
        }
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.0.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

struct Stage {
    batch: PathBuf,
    inside: String,
    cpus: String,
    source: BTreeMap<String, SourceFile>,
    workload: Vec<String>,
    handle: String,
    token: String,
    lab: Lab,
    proxy: Proxy,
    service: CacheService,
    artifacts: BTreeMap<PathBuf, String>,
    records: Vec<Value>,
    sequence: u64,
    schedule: process::Schedule,
    _affinity: RestoreAffinity,
}

fn required(name: &str) -> Result<PathBuf> {
    Ok(
        PathBuf::from(env::var_os(name).with_context(|| format!("{name} is required"))?)
            .canonicalize()?,
    )
}

fn command(argv: Vec<String>, folder: &Path, seconds: u64) -> Result<String> {
    String::from_utf8(process::command(&argv, folder, seconds)?)
        .context("command emitted invalid UTF8")
}

impl Stage {
    fn prepare() -> Result<Self> {
        let lab_path = required("PVISOR_BENCH_LAB")?;
        let output = PathBuf::from(
            env::var_os("PVISOR_BENCH_OUTPUT").context("PVISOR_BENCH_OUTPUT required")?,
        );
        ensure!(
            output.parent().context("batch parent")?.canonicalize()? == lab_path
                && output.components().any(|part| part.as_os_str() == ".data")
                && output.file_name().is_some_and(|value| value.len() <= 36),
            "stage evidence must be a NEW short-named directory directly under owned .data lab"
        );
        fs::create_dir(&output).context("refuse reused stage evidence")?;
        fs::set_permissions(&output, fs::Permissions::from_mode(0o700))?;
        let batch = output.canonicalize()?;
        save(
            batch.join("status.json"),
            &json!({"status":"preparing","valid":false}),
        )?;
        let result = Self::prepare_in(batch.clone(), lab_path);
        if let Err(error) = &result {
            save(
                batch.join("status.json"),
                &json!({"status":"failed","valid":false,"error":format!("{error:#}")}),
            )?;
        }
        result
    }

    fn prepare_in(batch: PathBuf, lab_path: PathBuf) -> Result<Self> {
        let cpus =
            process::cpus(&env::var("PVISOR_BENCH_CPUS").unwrap_or_else(|_| "12,13".into()))?;
        let service_cpus = process::cpus(
            &env::var("PVISOR_BENCH_SERVICE_CPUS").unwrap_or_else(|_| "14,15".into()),
        )?;
        let available = process::affinity()?;
        ensure!(
            cpus.len() == 2
                && service_cpus.len() == 2
                && !cpus.iter().any(|cpu| service_cpus.contains(cpu))
                && cpus
                    .iter()
                    .chain(&service_cpus)
                    .all(|cpu| available.contains(cpu)),
            "require disjoint available two-CPU application/service sets"
        );
        let affinity = RestoreAffinity(available);
        // Proxy threads and the native observer inherit the service CPU set;
        // both application arms are pinned separately by the common wrapper.
        process::pin(&service_cpus)?;
        let cpu_text = cpus
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let service_cpu_text = service_cpus
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        // Lab owns the shared nonblocking comparison lock for the entire cohort,
        // acquired before any mutable cache/client/service preparation.
        let mut lab = Lab::new(
            &lab_path,
            &batch.join("lab-verification"),
            &cpus,
            &service_cpus,
        )?;
        ensure!(
            lab.source_digest == SOURCE,
            "OverlayBD source digest differs"
        );
        let binary_dir = required("PVISOR_BENCH_BINARY_DIR")?;
        let store = required("PVISOR_BENCH_PREPARED_STORE")?;
        let runtime = required("PVISOR_BENCH_RUNTIME_RLIB")?;
        let inside = format!(
            "/lab/{}",
            batch.file_name().unwrap().to_str().context("batch UTF8")?
        );
        for name in ["tools", "harness", "trials", "pairs", "clients"] {
            fs::create_dir(batch.join(name))?;
        }
        for name in ["pvisor", "pvisor-daemon"] {
            fs::copy(binary_dir.join(name), batch.join("tools").join(name))?;
        }
        fs::copy(
            binary_dir.join("examples/cache_probe"),
            batch.join("tools/cache_probe"),
        )?;
        fs::write(
            batch.join("harness/stage_lazy.rs"),
            include_str!("stage_lazy.rs"),
        )?;
        fs::write(
            batch.join("harness/lazy_stage_mount.rs"),
            include_str!("../../../../startup/lazy_stage_mount.rs"),
        )?;
        for (name, body) in [
            ("lab.rs", include_str!("lab.rs")),
            ("process.rs", include_str!("process.rs")),
            ("proxy.rs", include_str!("proxy.rs")),
            ("source.rs", include_str!("source.rs")),
            ("evidence.rs", include_str!("../evidence.rs")),
        ] {
            fs::write(batch.join("harness").join(name), body)?;
        }
        let build = vec![
            "taskset".into(),
            "-c".into(),
            cpu_text.clone(),
            "rustc".into(),
            "--edition=2024".into(),
            "-C".into(),
            "opt-level=3".into(),
            "-C".into(),
            "panic=abort".into(),
            batch
                .join("harness/lazy_stage_mount.rs")
                .display()
                .to_string(),
            "--extern".into(),
            format!("pvisor={}", runtime.display()),
            "-L".into(),
            format!(
                "dependency={}",
                runtime.parent().context("rlib parent")?.display()
            ),
            "-o".into(),
            batch.join("tools/lazy-stage-mount").display().to_string(),
        ];
        let before_rlib = digest(&runtime)?;
        command(build.clone(), &batch.join("helper-build"), 120)?;
        ensure!(
            digest(&runtime)? == before_rlib,
            "runtime rlib changed during helper link"
        );
        let compiler = command(
            process::strings(&["rustc", "--version"]),
            &batch.join("compiler"),
            10,
        )?;
        save(
            batch.join("helper-build.json"),
            &json!({"argv":build,"rlib":runtime,
            "rlib_sha256":before_rlib,"compiler":compiler.trim(),
            "source_relationship":"existing runtime rlib/CLI, exact source-build correspondence unverified"}),
        )?;
        command(
            vec![
                "cp".into(),
                "-a".into(),
                "--reflink=auto".into(),
                store.display().to_string(),
                batch.join("service-store").display().to_string(),
            ],
            &batch.join("copy-store"),
            600,
        )?;
        let source = source_files(&batch.join("service-store"))?;
        let workload = vec!["/bin/sh".into(), "-ec".into(), probe(&source)?];
        // Install only benchmark-owned tools inside the already-verified private
        // lab. Never replace a host installation or unrelated container's files.
        let mut install = lab.docker.clone();
        install.extend(process::strings(&["sh", "-ec",
            "mkdir -p /opt/pvisor-stage-bench; cp \"$1/tools/pvisor\" /opt/pvisor-stage-bench/pvisor; cp \"$1/tools/lazy-stage-mount\" /opt/pvisor-stage-bench/lazy-stage-mount; chmod 755 /opt/pvisor-stage-bench /opt/pvisor-stage-bench/*",
            "sh", &inside]));
        lab.command(&install)?;
        let mut token_bytes = [0u8; 24];
        File::open("/dev/urandom")?.read_exact(&mut token_bytes)?;
        let token = token_bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = reservation.local_addr()?.port();
        drop(reservation);
        let upstream = format!("tcp://127.0.0.1:{port}");
        let service_log = File::create(batch.join("cache-service.log"))?;
        let mut cmd = Command::new("taskset");
        cmd.args(["-c", &service_cpu_text])
            .arg(batch.join("tools/pvisor-daemon"))
            .args([
                "serve",
                "--cache",
                "--cache-listen",
                &upstream,
                "--cache-image-store",
            ])
            .arg(batch.join("service-store"))
            .env_clear()
            .envs(env::vars_os().filter(|(key, _)| !key.to_string_lossy().starts_with("PVISOR_")))
            .env("PVISOR_CACHE_TOKEN", &token)
            .env("PVISOR_STARTUP_TIMING", "0")
            .env("XDG_CACHE_HOME", batch.join("server-cache"))
            .env("XDG_CONFIG_HOME", batch.join("server-config"))
            .stdin(Stdio::null())
            .stdout(service_log.try_clone()?)
            .stderr(service_log)
            .process_group(0);
        let mut service = CacheService(cmd.spawn()?, false);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            ensure!(
                service.0.try_wait()?.is_none(),
                "cache service exited during startup"
            );
            if std::net::TcpStream::connect_timeout(
                &format!("127.0.0.1:{port}").parse()?,
                Duration::from_millis(100),
            )
            .is_ok()
            {
                break;
            }
            ensure!(Instant::now() < deadline, "cache service startup timeout");
            thread::sleep(Duration::from_millis(50));
        }
        let proxy = Proxy::start_unix(&batch.join("proxy.sock"), &upstream, &token)?;
        let mut prepare = process::clean_command(&[
            batch.join("tools/cache_probe").display().to_string(),
            json!({"op":"prepare", "image":format!("docker.io/library/ubuntu@{SOURCE}"), "architecture":"amd64"}).to_string(),
        ])?;
        prepare
            .env("PVISOR_CACHE_SERVER", &upstream)
            .env("PVISOR_CACHE_TOKEN", &token)
            .env("XDG_CACHE_HOME", batch.join("server-cache"))
            .env("XDG_CONFIG_HOME", batch.join("server-config"));
        let prepared = process::observe(
            &mut prepare,
            &batch.join("cache-prepare"),
            None,
            Duration::from_secs(60),
            None,
        )?;
        let opened: Value = serde_json::from_slice(&prepared.stdout)?;
        ensure!(opened["digest"] == SOURCE, "cache source identity differs");
        let handle = opened["image_handle"]
            .as_str()
            .context("cache image handle")?
            .to_string();
        save(batch.join("cache-prepared.json"), &opened)?;
        let mut artifacts = BTreeMap::new();
        for directory in ["tools", "harness"] {
            for path in files(&batch.join(directory))? {
                artifacts.insert(path.clone(), digest(&path)?);
            }
        }
        for filename in FILES {
            let path = batch.join(format!(
                "service-store/rootfs-v3/sha256/{}/{}",
                &SOURCE[7..],
                filename
            ));
            artifacts.insert(path.clone(), digest(&path)?);
        }
        let platform_manifest = batch.join(format!(
            "service-store/metadata/manifests-v1/{}.json",
            &SOURCE[7..]
        ));
        artifacts.insert(platform_manifest.clone(), digest(&platform_manifest)?);
        for layer in read(platform_manifest)?["layers"]
            .as_array()
            .context("source layer inventory")?
        {
            let hash = layer["digest"].as_str().context("source layer digest")?;
            let path = batch.join("service-store/blobs/sha256").join(&hash[7..]);
            artifacts.insert(path.clone(), digest(&path)?);
        }
        let executable = env::current_exe()?.canonicalize()?;
        artifacts.insert(executable.clone(), digest(&executable)?);
        save(
            batch.join("observer-binary.json"),
            &json!({"path":executable,
            "source_relationship":"embedded Rust harness source; no product source-build correspondence asserted"}),
        )?;
        save(
            batch.join("protocol.json"),
            &json!({
                "benchmark":"B-LAZY-STARTUP","scope":"same-host-staged-lower-readonly",
                "engine":"Rust Instant / Criterion 0.7.0","groups":["pvisor","turbo"],
                "minimum_warmups":3,"criterion_samples":30,"cpus":cpus,"service_cpus":service_cpus,
                "source_digest":SOURCE,"source_files":source,"workload_argv":workload,"artifacts":artifacts,
                "clock":"Popen before common Docker exec; exact verified marker and normal wrapper exit, including open/rpull/mount/stage/unmount",
                "cache":"private cold client then hot reuse, new mount/stage each time; host page cache retained",
                "transport":"private Unix framed proxy -> local TCP cache vs local HTTP registry, no WAN",
                "budget":"same app/service affinity; no enclosing cgroup memory cap claimed",
                "historical_compatibility":"new adaptive Rust cohort, not pooled with fixed-count Python samples",
                "criterion_metric":"sum of four Ready durations in a randomized two-provider cold/warm pair",
                "traffic_units":"actual response bytes, retained separately; not nanoseconds or wire bytes"
            }),
        )?;
        Ok(Self {
            batch,
            inside,
            cpus: cpu_text,
            source,
            workload,
            handle,
            token,
            lab,
            proxy,
            service,
            artifacts,
            records: Vec::new(),
            sequence: 0,
            schedule: process::Schedule::new(2, 20261018),
            _affinity: affinity,
        })
    }

    fn check_sources(&self) -> Result<()> {
        for (path, expected) in &self.artifacts {
            ensure!(
                digest(path)? == *expected,
                "frozen stage artifact changed: {}",
                path.display()
            );
        }
        Ok(())
    }

    fn trial(&mut self, pvisor: bool, phase: &str, round: u64, warm: bool) -> Result<[u64; 3]> {
        self.check_sources()?;
        self.lab.check_artifacts()?;
        let group = if pvisor { "pvisor" } else { "turbo" };
        let cache_state = if warm { "warm" } else { "cold" };
        let name = format!("{phase}-{round:08}-{group}-{cache_state}");
        let folder = self.batch.join("trials").join(&name);
        fs::create_dir(&folder)?;
        let target = format!("{}/trials/{name}", self.inside);
        let cache = format!("{}/clients/{phase}-{round:08}-{group}", self.inside);
        if pvisor {
            let mut check = self.lab.docker.clone();
            check.extend(process::strings(&[
                "sh",
                "-ec",
                "if test \"$1\" = cold; then test ! -e \"$2\"; else test -d \"$2\"; fi",
                "sh",
                cache_state,
                &cache,
            ]));
            self.lab.command(&check)?;
        } else {
            fs::create_dir(folder.join("lower"))?;
        }
        let mut argv = vec!["docker".to_string(), "exec".to_string()];
        for (key, value) in [
            ("PVISOR_CACHE_TOKEN", self.token.clone()),
            ("PVISOR_STARTUP_TIMING", "0".into()),
            ("PVISOR_FS_PROFILE", "0".into()),
            ("PVISOR_LAZY_IMAGE_V2", "1".into()),
            ("PVISOR_LAZY_INDEX_PAGES", "1".into()),
            ("XDG_CACHE_HOME", cache.clone()),
            ("XDG_CONFIG_HOME", format!("{target}/config")),
            ("PVISOR_RUN_HOME", format!("{target}/runs")),
            ("PVISOR_IMAGE_STORE", format!("{cache}/store")),
        ] {
            argv.extend(["-e".into(), format!("{key}={value}")]);
        }
        argv.extend([
            self.lab.container.clone(),
            "/lab/tools/taskset".into(),
            "-c".into(),
            self.cpus.clone(),
        ]);
        // This identical launcher for both arms records the container-side
        // owner before any open/rpull/mount, enabling exact timeout recovery.
        argv.extend(process::strings(&[
            "sh",
            "-ec",
            r#"set -eu
target=$1
shift
generation=$(sed 's/.*) //' "/proc/$$/stat" | awk '{print $20}')
printf 'pid=%s\nstarttime=%s\n' "$$" "$generation" > "$target/wrapper.owner"
exec "$@""#,
            "sh",
            &target,
        ]));
        if pvisor {
            argv.extend([
                MOUNTER.into(),
                format!("unix://{}/proxy.sock", self.inside),
                self.handle.clone(),
                format!("{cache}/store"),
                format!("{target}/mount.receipt"),
                "--".into(),
            ]);
        } else {
            argv.extend([
                "sh".into(),
                "-ec".into(),
                TURBO_WRAPPER.into(),
                "sh".into(),
                target.clone(),
                format!("stage-{name}"),
                "-".into(),
                self.lab.derived_reference.clone(),
                MOUNTER.into(),
                "--plain".into(),
                format!("{target}/lower"),
                "-".into(),
                format!("{target}/mount.receipt"),
                "--".into(),
            ]);
        }
        argv.extend(process::strings(&[
            EXECUTABLE,
            "spawn",
            "--executor",
            "host",
            "--no-agent-defaults",
            "--overlaynet",
            "off",
            "--gateway-mode",
            "off",
            "--stdio",
            "inherit",
            "--timeout",
            "30s",
            "--stage",
        ]));
        argv.extend([format!("{target}/stage"), "--".into()]);
        argv.extend(self.workload.clone());
        let before = self.proxy.cursor()?;
        let cursor = self.lab.registry_cursor()?;
        let mut cmd = process::clean_command(&argv)?;
        let observed = process::observe(
            &mut cmd,
            &folder.join("timed"),
            Some(format!("{MARKER}\n").as_bytes()),
            Duration::from_secs(90),
            None,
        );
        if let Err(error) = observed {
            let recovery = self.recover_trial(&target, &cache, pvisor, &folder);
            save(
                folder.join("failure.json"),
                &json!({"valid":false,"error":format!("{error:#}"),
                "recovery_error":recovery.err().map(|error|format!("{error:#}"))}),
            )?;
            return Err(error);
        }
        let timed = observed?;
        ensure!(
            timed.stdout == format!("{MARKER}\n").as_bytes(),
            "stage stdout differs from unique exact marker"
        );
        let mut chown = self.lab.docker.clone();
        chown.extend(process::strings(&[
            "chown",
            "-R",
            &format!("{}:{}", unsafe { libc::getuid() }, unsafe {
                libc::getgid()
            }),
            &target,
        ]));
        self.lab.command(&chown)?;
        let cache_traffic = self.proxy.observation(before, &folder)?;
        let registry = self.lab.registry_observation(cursor, &folder)?;
        let (content, metadata, response) = if pvisor {
            ensure!(
                registry.response_bytes == 0,
                "pVisor unexpectedly fetched registry content"
            );
            (
                cache_traffic.content_bytes,
                cache_traffic.metadata_bytes,
                cache_traffic.response_bytes,
            )
        } else {
            ensure!(
                cache_traffic.response_bytes == 0,
                "OverlayBD unexpectedly used pVisor cache"
            );
            (
                registry.content_bytes,
                registry.metadata_bytes,
                registry.response_bytes,
            )
        };
        ensure!(
            if warm { content == 0 } else { content > 0 },
            "cold nonzero / hot zero content gate failed"
        );
        let mounted = validate_mount_and_bundle(
            &folder,
            &target,
            &cache,
            pvisor,
            &self.source,
            &self.workload,
            timed.timing.completion_ns,
        )?;
        let ready_ns = timed.timing.ready_ns.context("missing stage Ready time")?;
        let stage_ns = mounted["stage_completion_ns"]
            .as_u64()
            .context("missing mounted stage interval")?;
        self.lab.audit_inventory(&folder)?;
        self.check_sources()?;
        self.lab.check_artifacts()?;
        let mut raw_frames = BTreeMap::new();
        for row in read(folder.join("requests.json"))?["rows"]
            .as_array()
            .context("cache rows missing")?
        {
            let raw = PathBuf::from(
                row["raw_dir"]
                    .as_str()
                    .context("raw frame directory missing")?,
            );
            ensure!(
                raw.starts_with(self.batch.join("proxy-frames")),
                "cache evidence escapes owned batch"
            );
            raw_frames.insert(raw.clone(), source::tree(&raw)?);
        }
        let result = json!({"valid":true,"phase":phase,"round":round,
            "group":group,"cache":cache_state,"ready_ns":ready_ns,
            "completion_ns":timed.timing.completion_ns,"content_bytes":content,"metadata_bytes":metadata,
            "response_bytes":response,"mounted_stage":mounted,
            "stage_completion_ns":mounted["stage_completion_ns"],
            "traffic":{"content_bytes":content,"metadata_bytes":metadata,"response_bytes":response},
            "evidence_manifest":source::tree(&folder)?,"raw_frames":raw_frames});
        save(folder.join("result.json"), &result)?;
        self.records.push(json!({"phase":phase,"block":round,"condition":group,"state":cache_state,"result":result}));
        save(self.batch.join("records.partial.json"), &self.records)?;
        Ok([ready_ns, timed.timing.completion_ns, stage_ns])
    }

    fn recover_trial(
        &mut self,
        target: &str,
        cache: &str,
        pvisor: bool,
        folder: &Path,
    ) -> Result<()> {
        // The helper records its PID and Linux starttime before opening a lower.
        // Check both plus its exact receipt argv before signalling; PID reuse or
        // an unrelated process must never turn into a broad kill.
        let stop = r#"set -eu
target=$1
owner=$target/mount.receipt.owner
if test -f "$owner"; then
 pid=$(sed -n 's/^pid=//p' "$owner")
 generation=$(sed -n 's/^starttime=//p' "$owner")
 case "$pid:$generation" in ''|*[!0-9:]*) exit 1;; esac
 test "$pid" -gt 1
 if test -r "/proc/$pid/stat"; then
   current=$(sed 's/.*) //' "/proc/$pid/stat" | awk '{print $20}')
   if test "$current" = "$generation"; then
     tr '\000' '\n' < "/proc/$pid/cmdline" | grep -Fx "$target/mount.receipt" >/dev/null
     readlink "/proc/$pid/exe" | grep -Fx /opt/pvisor-stage-bench/lazy-stage-mount >/dev/null
     kill -TERM "$pid"
     n=0
     while test -r "/proc/$pid/stat"; do
       current=$(sed 's/.*) //' "/proc/$pid/stat" | awk '{print $20}')
       test "$current" = "$generation" || break
       n=$((n+1))
       if test "$n" -ge 120; then kill -KILL "$pid"; break; fi
       sleep .1
     done
   fi
 fi
fi"#;
        let mut argv = self.lab.docker.clone();
        argv.extend(process::strings(&["sh", "-ec", stop, "sh", target]));
        self.lab.command(&argv)?;
        let stop_wrapper = r#"set -eu
target=$1
owner=$target/wrapper.owner
if test -f "$owner"; then
 pid=$(sed -n 's/^pid=//p' "$owner")
 generation=$(sed -n 's/^starttime=//p' "$owner")
 case "$pid:$generation" in ''|*[!0-9:]*) exit 1;; esac
 test "$pid" -gt 1
 if test -r "/proc/$pid/stat"; then
  current=$(sed 's/.*) //' "/proc/$pid/stat" | awk '{print $20}')
  if test "$current" = "$generation"; then
   tr '\000' '\n' < "/proc/$pid/cmdline" | grep -Fx "$target" >/dev/null
   descendants() {
    local parent child stat ppid
    parent=$1
    for stat in /proc/[0-9]*/stat; do
     test -r "$stat" || continue
     child=${stat%/stat}; child=${child##*/}
     ppid=$(sed 's/.*) //' "$stat" 2>/dev/null | awk '{print $2}') || continue
     if test "$ppid" = "$parent"; then
      descendants "$child"
      test ! -r "$stat" || kill -TERM "$child" 2>/dev/null || true
     fi
    done
   }
   descendants "$pid"
   kill -TERM "$pid"
   n=0
   while test -r "/proc/$pid/stat"; do
    current=$(sed 's/.*) //' "/proc/$pid/stat" | awk '{print $20}')
    test "$current" = "$generation" || break
    n=$((n+1))
    if test "$n" -ge 100; then kill -KILL "$pid"; break; fi
    sleep .1
   done
  fi
 fi
fi"#;
        let mut argv = self.lab.docker.clone();
        argv.extend(process::strings(&["sh", "-ec", stop_wrapper, "sh", target]));
        self.lab.command(&argv)?;
        if pvisor {
            let release = r#"set -eu
receipt=$1/mount.receipt
cache=$2
if test -f "$receipt"; then
 lower=$(sed -n 's/^rootfs=//p' "$receipt")
 case "$lower" in "$cache"/store/projections/.lazy-mount-*) ;; *) exit 1;; esac
 case "$lower" in *'/../'*|*'/./'*) exit 1;; esac
 if awk -v lower="$lower" '$5==lower {found=1} END {exit !found}' /proc/self/mountinfo; then
   umount "$lower"
 fi
 ! awk -v lower="$lower" '$5==lower {found=1} END {exit !found}' /proc/self/mountinfo
fi"#;
            let mut argv = self.lab.docker.clone();
            argv.extend(process::strings(&[
                "sh", "-ec", release, "sh", target, cache,
            ]));
            self.lab.command(&argv)?;
        } else {
            let release = r#"set -eu
target=$1
ctr=/opt/overlaybd/snapshotter/ctr
if awk -v lower="$target/lower" '$5==lower {found=1} END {exit !found}' /proc/self/mountinfo; then
 $ctr --address /lab/run/containerd.sock --namespace pvisor-obd-smoke images unmount --snapshotter overlaybd --rm "$target/lower"
fi
if test -f "$target/mount-command.sh"; then
 read -r mountcmd opt kind source_path rest < "$target/mount-command.sh"
 test "$mountcmd" = mount && test "$opt" = -t && test "$kind" = bind
 case "$source_path" in /lab/containerd-root/io.containerd.snapshotter.v1.overlaybd/snapshots/*/block/mountpoint) ;; *) exit 1;; esac
 sn=${source_path%/block/mountpoint}
 sn=${sn##*/}
 case "$sn" in ''|*[!0-9]*) exit 1;; esac
 info=/sys/kernel/config/target/core/user_999999999/dev_$sn/info
 if test -f "$info"; then
   grep -F "overlaybd//lab/containerd-root/io.containerd.snapshotter.v1.overlaybd/snapshots/$sn/block/config.v1.json" "$info" >/dev/null
   if awk -v lower="$source_path" '$5==lower {found=1} END {exit !found}' /proc/self/mountinfo; then umount "$source_path"; fi
   /opt/overlaybd/snapshotter/overlaybd-attacher detach --id "$sn"
 fi
fi"#;
            let mut argv = self.lab.docker.clone();
            argv.extend(process::strings(&["sh", "-ec", release, "sh", target]));
            self.lab.command(&argv)?;
        }
        self.lab.audit_inventory(folder)
    }

    fn pair(&mut self, phase: &str) -> Result<[u64; 3]> {
        self.check_sources()?;
        let round = self.sequence;
        self.sequence += 1;
        let before = process::snapshot(&[self.service.0.id()])?;
        let order = self.schedule.next();
        save(
            self.batch.join(format!("order-{phase}-{round:08}.json")),
            &order,
        )?;
        let mut measured = [0u64; 3];
        for group in order {
            let pvisor = group == 0;
            let pair = self
                .batch
                .join("pairs")
                .join(format!("{phase}-{round:08}-{group}"));
            fs::create_dir(&pair)?;
            if !pvisor {
                self.lab.reset_client(&pair)?;
            }
            let mut warm_host = self.lab.docker.clone();
            warm_host.extend(process::strings(&[
                "/lab/tools/taskset",
                "-c",
                &self.cpus,
                EXECUTABLE,
                "spawn",
                "--executor",
                "host",
                "--no-agent-defaults",
                "--overlaynet",
                "off",
                "--gateway-mode",
                "off",
                "--stdio",
                "inherit",
                "--filesystem",
                "host",
                "--",
                "/bin/true",
            ]));
            self.lab.command(&warm_host)?;
            for warm in [false, true] {
                let observed = self.trial(pvisor, phase, round, warm)?;
                for index in 0..3 {
                    measured[index] = measured[index]
                        .checked_add(observed[index])
                        .context("timing sum overflow")?;
                }
            }
            self.lab.audit_inventory(&pair)?;
        }
        let after = process::snapshot(&[self.service.0.id()])?;
        let cpus = self
            .cpus
            .split(',')
            .map(str::parse)
            .collect::<std::result::Result<Vec<usize>, _>>()?;
        process::background(
            &before,
            &after,
            &cpus,
            &self
                .batch
                .join(format!("background-{phase}-{round:08}.json")),
        )?;
        Ok(measured)
    }

    fn audit(&self) -> Result<()> {
        let outcome = (|| -> Result<()> {
            self.check_sources()?;
            ensure!(
                self.records.len() as u64 == self.sequence * 4,
                "incomplete same-stage paired cohort"
            );
            let mut blocks: BTreeMap<u64, std::collections::BTreeSet<(String, String)>> =
                BTreeMap::new();
            for row in &self.records {
                let phase = row["phase"].as_str().context("phase missing")?;
                let block = row["block"].as_u64().context("block missing")?;
                let group = row["condition"].as_str().context("condition missing")?;
                let state = row["state"].as_str().context("cache state missing")?;
                ensure!(
                    ["pvisor", "turbo"].contains(&group) && ["cold", "warm"].contains(&state),
                    "unknown paired arm"
                );
                ensure!(
                    blocks
                        .entry(block)
                        .or_default()
                        .insert((group.into(), state.into())),
                    "duplicate stage pair arm"
                );
                let name = format!("{phase}-{block:08}-{group}-{state}");
                let folder = self.batch.join("trials").join(&name);
                let target = format!("{}/trials/{name}", self.inside);
                let cache = format!("{}/clients/{phase}-{block:08}-{group}", self.inside);
                let expected = &row["result"];
                ensure!(
                    read(folder.join("result.json"))? == *expected && expected["valid"] == true,
                    "retained result differs from checked stage observation"
                );
                let mut actual = source::tree(&folder)?;
                actual
                    .as_object_mut()
                    .context("trial evidence must be an object")?
                    .remove("result.json");
                ensure!(
                    actual == expected["evidence_manifest"],
                    "stage trial evidence changed"
                );
                for (raw, manifest) in expected["raw_frames"]
                    .as_object()
                    .context("raw frame inventory")?
                {
                    let path = Path::new(raw);
                    ensure!(
                        path.starts_with(self.batch.join("proxy-frames"))
                            && source::tree(path)? == *manifest,
                        "cache raw frame evidence changed or escaped owner"
                    );
                }
                let timing = read(folder.join("timed/timing.json"))?;
                let start = timing["start_ns"]
                    .as_u64()
                    .context("missing process start")?;
                let ready = timing["marker_ns"]
                    .as_u64()
                    .context("missing Ready observation")?;
                let end = timing["end_ns"]
                    .as_u64()
                    .context("missing normal completion")?;
                ensure!(
                    timing["valid"] == true
                        && timing["exit_code"] == 0
                        && timing["timed_out"] == false
                        && start < ready
                        && ready <= end
                        && expected["ready_ns"].as_u64() == Some(ready - start)
                        && expected["completion_ns"].as_u64() == Some(end - start),
                    "retained stage clock arithmetic or normal exit differs"
                );
                ensure!(
                    fs::read(folder.join("timed/stdout.bin"))? == format!("{MARKER}\n").as_bytes(),
                    "retained stage marker differs"
                );
                let mounted = validate_mount_and_bundle(
                    &folder,
                    &target,
                    &cache,
                    group == "pvisor",
                    &self.source,
                    &self.workload,
                    end - start,
                )?;
                ensure!(
                    mounted == expected["mounted_stage"],
                    "retained mount/Bundle evidence differs"
                );
                let content = expected["traffic"]["content_bytes"]
                    .as_u64()
                    .context("content counter missing")?;
                ensure!(
                    if state == "cold" {
                        content > 0
                    } else {
                        content == 0
                    },
                    "retained cold/hot content gate failed"
                );
            }
            ensure!(
                blocks.len() as u64 == self.sequence && blocks.values().all(|arms| arms.len() == 4),
                "missing complete randomized same-stage pair"
            );
            for pair in self.records.chunks_exact(4) {
                let phase = pair[0]["phase"].as_str().context("pair phase missing")?;
                let block = pair[0]["block"].as_u64().context("pair block missing")?;
                ensure!(
                    pair.iter()
                        .all(|row| row["phase"] == phase && row["block"] == block)
                        && pair[0]["condition"] == pair[1]["condition"]
                        && pair[2]["condition"] == pair[3]["condition"]
                        && pair[0]["condition"] != pair[2]["condition"]
                        && pair[0]["state"] == "cold"
                        && pair[1]["state"] == "warm"
                        && pair[2]["state"] == "cold"
                        && pair[3]["state"] == "warm",
                    "stage records do not preserve immediate cold-then-hot randomized pairs"
                );
                let planned = read(self.batch.join(format!("order-{phase}-{block:08}.json")))?;
                let actual = pair
                    .iter()
                    .step_by(2)
                    .map(|row| if row["condition"] == "pvisor" { 0 } else { 1 })
                    .collect::<Vec<_>>();
                ensure!(
                    planned == json!(actual),
                    "stage records differ from retained randomized order"
                );
            }
            Ok(())
        })();
        save(
            self.batch.join("audit.json"),
            &json!({"valid":outcome.is_ok(),
            "pairs":self.sequence,"operations":self.records.len(),
            "scope":"read-only semantic audit of all raw operations, including framework calibration; not statistical inference",
            "error":outcome.as_ref().err().map(|error|format!("{error:#}"))}),
        )?;
        outcome
    }
}

pub(super) fn run(criterion: &mut Criterion) -> Result<()> {
    let mut stage = Stage::prepare()?;
    let result = (|| {
        stage.pair("preflight")?;
        for _ in 0..3 {
            stage.pair("protocol-warmup")?;
        }
        let mut group = criterion.benchmark_group("startup/stage_lazy_compare");
        group.sample_size(30).sampling_mode(SamplingMode::Flat);
        for (index, metric) in ["ready", "completion", "stage_completion"]
            .into_iter()
            .enumerate()
        {
            let mut callbacks = Vec::new();
            let phase = format!("criterion-{metric}");
            group.bench_function(format!("paired_cold_warm_{metric}"), |bencher| {
                bencher.iter_custom(|iterations| {
                    let mut sum = Duration::ZERO;
                    let first=stage.sequence;
                    for _ in 0..iterations {
                        match stage.pair(&phase) {
                            Ok(nanos) => sum += Duration::from_nanos(nanos[index]),
                            Err(error) => {
                                let _ = save(stage.batch.join("status.json"),&json!({"valid":false,"status":"failed",
                                    "error":format!("{error:#}")}));
                                panic!("stage pair failed: {error:#}");
                            }
                        }
                    }
                    callbacks.push(first);
                    save(stage.batch.join(format!("criterion-{metric}-call-{:06}.json",callbacks.len())),&json!({
                        "first_block":first,"last_block_exclusive":stage.sequence,
                        "iterations_requested":iterations,"returned_ns":sum.as_nanos().to_string(),
                        "metric":metric,"scope":"sum of four independently observed intervals in a randomized cold/hot pair",
                        "includes_framework_calibration":true})).expect("retain Criterion callback mapping");
                    sum
                });
            });
            save(
                stage.batch.join(format!("criterion-{metric}-scope.json")),
                &json!({
                    "callback_count":callbacks.len(),
                    "phase_mapping":"Criterion does not expose callback phases; no inferred formal subset",
                    "statistics":"Criterion output only; raw callbacks include framework calibration",
                    "paired_publication_gate":"not satisfied by Criterion aggregate pair intervals alone; no published speedup or paired CI claimed"
                }),
            )?;
        }
        group.finish();
        save(stage.batch.join("records.json"), &stage.records)?;
        stage.check_sources()?;
        stage.proxy.stop()?;
        stage.audit()?;
        stage
            .service
            .stop(&stage.batch.join("cache-service-teardown.json"))?;
        save(
            stage.batch.join("status.json"),
            &json!({"valid":true,"status":"complete","pairs":stage.sequence}),
        )?;
        Ok(())
    })();
    if let Err(error) = &result {
        save(
            stage.batch.join("status.json"),
            &json!({"valid":false,"status":"failed","error":format!("{error:#}")}),
        )?;
    }
    result
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SourceFile {
    bytes: u64,
    sha256: String,
}

/// Verify local file identities against the actual compressed OCI layers, not
/// merely a second receipt that points at the same extracted directory.
fn source_files(store: &Path) -> Result<BTreeMap<String, SourceFile>> {
    let manifest_path = store.join(format!("metadata/manifests-v1/{}.json", &SOURCE[7..]));
    ensure!(
        digest(&manifest_path)? == SOURCE[7..],
        "source platform manifest mismatch"
    );
    let manifest = read(manifest_path)?;
    let root = store.join(format!("rootfs-v3/sha256/{}", &SOURCE[7..]));
    let mut expected = BTreeMap::new();
    for filename in FILES {
        let path = root.join(filename);
        for ancestor in path.ancestors() {
            ensure!(
                !fs::symlink_metadata(ancestor)?.is_symlink(),
                "probe paths must not follow symlinks: {}",
                ancestor.display()
            );
        }
        let info = fs::symlink_metadata(&path)?;
        ensure!(info.is_file(), "probe source must be regular");
        expected.insert(
            filename.to_string(),
            SourceFile {
                bytes: info.len(),
                sha256: digest(&path)?,
            },
        );
    }
    ensure!(
        expected.values().map(|item| item.bytes).sum::<u64>() == 2_575_738,
        "source working set size changed"
    );
    let mut observed = BTreeMap::new();
    for layer in manifest["layers"]
        .as_array()
        .context("source manifest layers")?
    {
        let hash = layer["digest"].as_str().context("layer digest")?;
        ensure!(
            hash.starts_with("sha256:")
                && hash.len() == 71
                && hash[7..].bytes().all(|value| value.is_ascii_hexdigit()),
            "invalid layer digest"
        );
        let blob = store.join("blobs/sha256").join(&hash[7..]);
        ensure!(
            digest(&blob)? == hash[7..]
                && layer["size"].as_u64() == Some(fs::metadata(&blob)?.len()),
            "source compressed layer mismatch"
        );
        let mut magic = [0; 2];
        File::open(&blob)?.read_exact(&mut magic)?;
        let stream: Box<dyn Read> = if magic == [0x1f, 0x8b] {
            Box::new(GzDecoder::new(BufReader::new(File::open(blob)?)))
        } else {
            Box::new(BufReader::new(File::open(blob)?))
        };
        let mut archive = tar::Archive::new(stream);
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            ensure!(
                !path.is_absolute()
                    && !path
                        .components()
                        .any(|part| matches!(part, Component::ParentDir)),
                "unsafe OCI tar entry"
            );
            let normalized = path
                .components()
                .filter_map(|part| match part {
                    Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("/");
            // A later whiteout of a selected file (or an opaque ancestor)
            // must not allow stale earlier bytes to pass extraction validation.
            for filename in FILES {
                let path = Path::new(filename);
                let whiteout = path.with_file_name(format!(
                    ".wh.{}",
                    path.file_name().unwrap().to_string_lossy()
                ));
                let opaque = path
                    .ancestors()
                    .skip(1)
                    .any(|parent| parent.join(".wh..wh..opq").to_string_lossy() == normalized);
                if whiteout.to_string_lossy() == normalized || opaque {
                    observed.remove(filename);
                }
            }
            if FILES.contains(&normalized.as_str()) {
                ensure!(
                    entry.header().entry_type().is_file(),
                    "selected OCI entry must be regular"
                );
                let mut bytes = 0u64;
                let mut hash = Sha256::new();
                let mut buffer = [0; 65536];
                loop {
                    let count = entry.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    bytes += count as u64;
                    hash.update(&buffer[..count]);
                }
                observed.insert(
                    normalized,
                    SourceFile {
                        bytes,
                        sha256: format!("{:x}", hash.finalize()),
                    },
                );
            }
        }
    }
    ensure!(
        observed == expected,
        "prepared files differ from original OCI layer contents"
    );
    Ok(expected)
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn probe(source: &BTreeMap<String, SourceFile>) -> Result<String> {
    ensure!(
        source.len() == FILES.len() && FILES.iter().all(|name| source.contains_key(*name)),
        "probe selection differs"
    );
    let expected = FILES
        .iter()
        .map(|name| format!("{}  {name}\n", source[*name].sha256))
        .collect::<String>();
    Ok(format!(
        "printf %s {} | sha256sum -c - >/dev/null; printf {}",
        quote(&expected),
        quote(&format!("{MARKER}\n"))
    ))
}

fn key_values(path: &Path) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for line in fs::read_to_string(path)?.lines() {
        let (key, value) = line.split_once('=').context("malformed helper receipt")?;
        ensure!(
            values.insert(key.into(), value.into()).is_none(),
            "duplicate helper receipt key"
        );
    }
    Ok(values)
}

fn validate_mount_and_bundle(
    directory: &Path,
    target: &str,
    cache: &str,
    pvisor: bool,
    source: &BTreeMap<String, SourceFile>,
    workload: &[String],
    completion_ns: u64,
) -> Result<Value> {
    let receipt = key_values(&directory.join("mount.receipt"))?;
    let lower = receipt
        .get("rootfs")
        .context("lower receipt missing rootfs")?;
    let mounts = fs::read_to_string(directory.join("mount.receipt.mountinfo"))?;
    let actual: Vec<_> = mounts
        .lines()
        .filter(|line| {
            line.split(" - ")
                .next()
                .and_then(|left| left.split_whitespace().nth(4))
                == Some(lower)
        })
        .collect();
    ensure!(actual.len() == 1, "missing unique lower mount");
    let (left, right) = actual[0].split_once(" - ").context("invalid mountinfo")?;
    let fields: Vec<_> = left.split_whitespace().collect();
    let filesystem: Vec<_> = right.split_whitespace().collect();
    ensure!(
        fields.len() >= 6 && fields[5].split(',').any(|value| value == "ro"),
        "lower mount must actually be read-only"
    );
    ensure!(
        key_values(&directory.join("mount.receipt.unmounted"))?
            == BTreeMap::from([("mountpoint_removed".into(), "true".into())]),
        "lower owner did not report release"
    );
    if pvisor {
        ensure!(
            filesystem.get(..2) == Some(&["fuse", "pvisor-image"][..])
                && receipt.get("digest").map(String::as_str) == Some(SOURCE)
                && lower.starts_with(&format!("{cache}/store/projections/.lazy-mount-")),
            "lower is not the expected actual host FUSE image"
        );
    } else {
        ensure!(
            filesystem.first() == Some(&"ext4")
                && lower == &format!("{target}/lower")
                && receipt.get("digest").map(String::as_str) == Some("overlaybd-view"),
            "TurboOCI lower is not the owned ext4 mount"
        );
        let mount_command = fs::read_to_string(directory.join("mount-command.sh"))?;
        let mount: Vec<_> = mount_command.split_whitespace().collect();
        ensure!(
            mount.len() >= 5 && mount[..3] == ["mount", "-t", "bind"] && mount[4] == lower,
            "snapshotter mount differs from lower"
        );
        let snapshot = mount[3]
            .strip_prefix("/lab/containerd-root/io.containerd.snapshotter.v1.overlaybd/snapshots/")
            .and_then(|value| value.strip_suffix("/block/mountpoint"))
            .context("snapshot escapes owned state")?;
        ensure!(
            !snapshot.is_empty() && snapshot.bytes().all(|byte| byte.is_ascii_digit()),
            "invalid snapshot ID"
        );
        let info = fs::read_to_string(directory.join("target-info.txt"))?;
        ensure!(info.contains(&format!("overlaybd//lab/containerd-root/io.containerd.snapshotter.v1.overlaybd/snapshots/{snapshot}/block/config.v1.json")),
            "TCMU target does not identify selected owned snapshot");
        let device = fs::read_to_string(directory.join("device.txt"))?;
        let device = device.trim();
        ensure!(
            device.strip_prefix("/dev/sd").is_some_and(
                |tail| !tail.is_empty() && tail.bytes().all(|byte| byte.is_ascii_lowercase())
            ) && filesystem.get(1) == Some(&device),
            "ext4 source differs from OverlayBD backstore"
        );
    }
    let bundle_path = directory.join("stage/run-bundle.json");
    let bundle = read(&bundle_path)?;
    ensure!(
        bundle["run"]["state"] == "completed"
            && bundle["run"]["exit_code"] == 0
            && bundle["run"]["executor"]["isolation"] == "rootless_process"
            && bundle["run"]["command"] == json!(workload)
            && bundle["safety"]["filesystem_changes_staged"] == true,
        "Bundle is not successful identical staged host task"
    );
    let filesystem_state = &bundle["filesystem"];
    ensure!(
        filesystem_state["state"] == "staged"
            && filesystem_state["target"] == *lower
            && filesystem_state["upper"] == format!("{target}/stage/upper")
            && filesystem_state["changed_files"] == 0
            && filesystem_state["changes"] == json!([])
            && filesystem_state
                .get("whiteouts")
                .is_none_or(|value| *value == 0),
        "Bundle contains mutations or different workspace"
    );
    super::super::evidence::require_empty_upper(&directory.join("stage/upper"))?;
    for (filename, expected) in source {
        let read = &bundle["run_observation"]["filesystem"]["paths"][filename]["read"];
        ensure!(
            read["bytes_read"].as_u64() == Some(expected.bytes)
                && read.get("failed").is_none_or(|value| *value == 0),
            "Bundle did not observe exact source bytes: {filename}"
        );
    }
    let stage_ns: u64 = key_values(&directory.join("mount.receipt.stage"))?
        .get("completion_ns")
        .context("missing stage completion")?
        .parse()?;
    ensure!(
        stage_ns > 0 && stage_ns <= completion_ns,
        "stage interval exceeds wrapper interval"
    );
    let mount_ns: u64 = receipt
        .get("mount_ns")
        .context("missing mount interval")?
        .parse()?;
    Ok(
        json!({"lower_mount":actual[0],"mount_receipt":receipt,"mount_ns":mount_ns,
        "stage_completion_ns":stage_ns,"bundle_sha256":digest(&bundle_path)?}),
    )
}

// The subject wrapper is constant, retained in the evidence and passed as a
// literal sh -ec argument. No interpolated command or mount text is evaluated.
const TURBO_WRAPPER: &str = r#"set -eu
target=$1
key=$2
parent=$3
ref=$4
shift 4
ctr=/opt/overlaybd/snapshotter/ctr
$ctr --address /lab/run/containerd.sock --namespace pvisor-obd-smoke rpull --plain-http "$ref" >&2
$ctr --address /lab/run/containerd.sock --namespace pvisor-obd-smoke images mount --snapshotter overlaybd "$ref" "$target/lower" > "$target/image-mount.log"
$ctr --address /lab/run/containerd.sock --namespace pvisor-obd-smoke snapshots --snapshotter overlaybd mounts "$target/lower" "$target/lower" > "$target/mount-command.sh"
read -r mountcmd opt kind source_path rest < "$target/mount-command.sh"
test "$mountcmd" = mount && test "$opt" = -t && test "$kind" = bind
case "$source_path" in /lab/containerd-root/io.containerd.snapshotter.v1.overlaybd/snapshots/*/block/mountpoint) ;; *) exit 1 ;; esac
sn=${source_path%/block/mountpoint}
sn=${sn##*/}
case "$sn" in ''|*[!0-9]*) exit 1 ;; esac
info=/sys/kernel/config/target/core/user_999999999/dev_$sn/info
grep -F "overlaybd//lab/containerd-root/io.containerd.snapshotter.v1.overlaybd/snapshots/$sn/block/config.v1.json" "$info" > "$target/target-info.txt"
cat "${source_path%/mountpoint}/backstore_mark" > "$target/device.txt"
set +e
"$@"
code=$?
set -e
$ctr --address /lab/run/containerd.sock --namespace pvisor-obd-smoke images unmount --snapshotter overlaybd --rm "$target/lower" >&2
if test -f "$info"; then
  umount "$source_path"
  /opt/overlaybd/snapshotter/overlaybd-attacher detach --id "$sn" >&2
fi
exit "$code"
"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn probe_checks_every_file_before_marker_and_does_not_execute_image_programs() {
        let source = FILES
            .iter()
            .map(|name| {
                (
                    name.to_string(),
                    SourceFile {
                        bytes: 1,
                        sha256: "a".repeat(64),
                    },
                )
            })
            .collect();
        let probe = probe(&source).unwrap();
        assert!(probe.find("sha256sum -c").unwrap() < probe.find(MARKER).unwrap());
        for name in FILES {
            assert!(probe.contains(name));
        }
        assert!(!probe.contains("exec"));
    }

    #[test]
    fn helper_receipts_reject_duplicate_fields() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("receipt");
        fs::write(&path, "rootfs=/a\nrootfs=/b\n").unwrap();
        assert!(key_values(&path).is_err());
    }

    fn mounted_fixture(root: &Path) -> (BTreeMap<String, SourceFile>, Vec<String>) {
        let source: BTreeMap<_, _> = FILES
            .iter()
            .map(|name| {
                (
                    name.to_string(),
                    SourceFile {
                        bytes: 17,
                        sha256: "a".repeat(64),
                    },
                )
            })
            .collect();
        let workload = vec!["/bin/sh".into(), "-ec".into(), probe(&source).unwrap()];
        fs::create_dir_all(root.join("stage/upper")).unwrap();
        fs::write(
            root.join("mount.receipt"),
            format!("rootfs=/client/store/projections/.lazy-mount-fixture\ndigest={SOURCE}\nmount_ns=100\n"),
        )
        .unwrap();
        fs::write(root.join("mount.receipt.stage"), "completion_ns=200\n").unwrap();
        fs::write(
            root.join("mount.receipt.unmounted"),
            "mountpoint_removed=true\n",
        )
        .unwrap();
        fs::write(
            root.join("mount.receipt.mountinfo"),
            "42 1 0:17 / /client/store/projections/.lazy-mount-fixture ro,nosuid - fuse pvisor-image ro\n",
        )
        .unwrap();
        let paths: serde_json::Map<String, Value> = FILES
            .iter()
            .map(|name| {
                (
                    name.to_string(),
                    json!({"read":{"bytes_read":17,"failed":0}}),
                )
            })
            .collect();
        save(root.join("stage/run-bundle.json"),&json!({
            "run":{"state":"completed","exit_code":0,"executor":{"isolation":"rootless_process"},
                "command":workload},
            "safety":{"filesystem_changes_staged":true},
            "filesystem":{"state":"staged","target":"/client/store/projections/.lazy-mount-fixture",
                "upper":"/target/stage/upper","changed_files":0,"changes":[],"whiteouts":0},
            "run_observation":{"filesystem":{"paths":paths}}
        })).unwrap();
        (source, workload)
    }

    #[test]
    fn staged_bundle_requires_actual_readonly_mount_and_exact_observed_bytes() {
        let root = tempfile::tempdir().unwrap();
        let (source, workload) = mounted_fixture(root.path());
        validate_mount_and_bundle(
            root.path(),
            "/target",
            "/client",
            true,
            &source,
            &workload,
            1000,
        )
        .unwrap();
        let bundle_path = root.path().join("stage/run-bundle.json");
        let mut bundle = read(&bundle_path).unwrap();
        bundle["run_observation"]["filesystem"]["paths"][FILES[0]]["read"]["bytes_read"] =
            16.into();
        save(&bundle_path, &bundle).unwrap();
        assert!(
            validate_mount_and_bundle(
                root.path(),
                "/target",
                "/client",
                true,
                &source,
                &workload,
                1000
            )
            .is_err()
        );
        mounted_fixture(root.path());
        fs::write(
            root.path().join("mount.receipt.mountinfo"),
            "42 1 0:17 / /client/store/projections/.lazy-mount-fixture rw,nosuid - fuse pvisor-image rw\n",
        )
        .unwrap();
        assert!(
            validate_mount_and_bundle(
                root.path(),
                "/target",
                "/client",
                true,
                &source,
                &workload,
                1000
            )
            .is_err()
        );
    }

    #[test]
    fn mounted_subphase_cannot_exceed_full_completion_or_hide_upper_mutations() {
        let root = tempfile::tempdir().unwrap();
        let (source, workload) = mounted_fixture(root.path());
        assert!(
            validate_mount_and_bundle(
                root.path(),
                "/target",
                "/client",
                true,
                &source,
                &workload,
                199
            )
            .is_err()
        );
        fs::write(root.path().join("stage/upper/unexpected"), "mutation").unwrap();
        assert!(
            validate_mount_and_bundle(
                root.path(),
                "/target",
                "/client",
                true,
                &source,
                &workload,
                1000
            )
            .is_err()
        );
    }
}
