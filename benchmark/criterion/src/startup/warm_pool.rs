//! Benchmark: B-DAEMON-WARM-POOL (benchmark/REGISTRY.md#b-daemon-warm-pool), role user-facing.
//! Motivation: decide whether to reserve an idle VM for repeat sandbox creates.
//! Conclusion sought: paired end-to-end latency and observed fallback on a two-request burst.
//! Design: same native image and daemon, randomized serial pairs and separately scored bursts.

use super::evidence::{digest, read, save};
use anyhow::{Context, Result, bail, ensure};
use criterion::{Criterion, SamplingMode};
use reqwest::{Method, blocking::Client};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::DirBuilderExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Barrier,
    thread,
    time::{Duration, Instant},
};

const LIMIT: Duration = Duration::from_secs(15);
const PROFILE: &str = "4cx256M";
const BASE: &str = "/v1/sandboxes";
const MARKER: &str = "/tmp/pvisor-warm-pool-bench";
const SHELL: &str = r#"set -eu
test "$(cat /etc/os-release | grep -c '^ID=ubuntu$')" = 1
test ! -e "$1"
printf '%s\n' "$2" >"$1"
cat "$1"
"#;

fn wait_for<T>(mut check: impl FnMut() -> Result<Option<T>>) -> Result<T> {
    let deadline = Instant::now() + LIMIT;
    loop {
        if let Some(value) = check()? {
            return Ok(value);
        }
        ensure!(Instant::now() < deadline, "observation deadline exceeded");
        thread::sleep(Duration::from_millis(5));
    }
}

fn input(key: &str) -> Result<PathBuf> {
    PathBuf::from(env::var_os(key).with_context(|| format!("{key} is required"))?)
        .canonicalize()
        .with_context(|| format!("resolve {key}"))
}

fn scope() -> Result<PathBuf> {
    let membership = fs::read_to_string("/proc/self/cgroup")?;
    let scope = Path::new("/sys/fs/cgroup").join(
        membership
            .trim()
            .strip_prefix("0::/")
            .context("requires unified cgroup v2")?,
    );
    let name = scope
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    ensure!(
        name.starts_with("pvisor-warm-") && name.ends_with(".scope"),
        "run inside a new pvisor-warm-*.scope; see the startup README"
    );
    ensure!(
        fs::read_to_string(scope.join("cgroup.procs"))?.trim() == std::process::id().to_string(),
        "scope must initially contain only the benchmark observer"
    );
    for (file, expected) in [
        ("cpu.max", "400000 100000"),
        ("memory.max", "2147483648"),
        ("memory.swap.max", "0"),
    ] {
        ensure!(
            fs::read_to_string(scope.join(file))?.trim() == expected,
            "incorrect scope {file}"
        );
    }
    let mut mask = unsafe { std::mem::zeroed::<libc::cpu_set_t>() };
    ensure!(
        unsafe { libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &mut mask) } == 0
            && unsafe { libc::CPU_COUNT(&mask) } == 4,
        "pin the entire scope to four logical CPUs"
    );
    fs::create_dir(scope.join("observer"))?;
    fs::write(scope.join("observer/cgroup.procs"), "0")?;
    enable(&scope)?;
    Ok(scope)
}

fn enable(group: &Path) -> Result<()> {
    fs::write(group.join("cgroup.subtree_control"), "+cpu +memory +pids")?;
    Ok(())
}

fn oom_free(group: &Path) -> Result<()> {
    let counters = fs::read_to_string(group.join("memory.events"))?;
    for name in ["oom", "oom_kill"] {
        ensure!(
            counters
                .lines()
                .any(|line| line.strip_prefix(name) == Some(" 0")),
            "OOM in {group:?}"
        );
    }
    Ok(())
}

fn groups(root: &Path) -> Result<Vec<PathBuf>> {
    let mut groups = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            groups.push(entry.path());
        }
    }
    Ok(groups)
}

struct Daemon {
    child: Child,
    client: Client,
    url: String,
    key: String,
    state: PathBuf,
    scope: PathBuf,
    instances: PathBuf,
    log: PathBuf,
}

impl Daemon {
    fn start(
        binary: &Path,
        images: &Path,
        scope: &Path,
        temp: &Path,
        output: &Path,
        warm: bool,
    ) -> Result<Self> {
        let name = if warm { "warm" } else { "cold" };
        let scope = scope.join(name);
        fs::create_dir(&scope)?;
        enable(&scope)?;
        let service = scope.join("service");
        let instances = scope.join("instances");
        fs::create_dir(&service)?;
        fs::create_dir(&instances)?;
        enable(&instances)?;
        let procs = OpenOptions::new()
            .write(true)
            .open(service.join("cgroup.procs"))?;
        let mut random = [0u8; 32];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        let key = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let client = Client::builder().timeout(LIMIT).no_proxy().build()?;
        let state = temp.join(name);
        let log = output.join(format!("{name}.log"));
        let mut command = Command::new(binary);
        command
            .env_clear()
            .env("OPEN_SANDBOX_API_KEY", &key)
            .args([
                "serve",
                "--sandbox",
                "--listen",
                "127.0.0.1:0",
                "--max-sandboxes",
                "2",
                "--cpu-millis",
                "8000",
                "--memory-bytes",
                "536870912",
                "--images-dir",
            ])
            .arg(images)
            .arg("--state")
            .arg(&state)
            .arg("--cgroup-root")
            .arg(&instances)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(&log)?);
        if warm {
            command.args(["--warm-pool", PROFILE]);
        }
        save(
            output.join(format!("{name}-argv.json")),
            &command.get_args().collect::<Vec<_>>(),
        )?;
        // Join the owned service leaf before charging daemon allocations.
        unsafe {
            command.pre_exec(move || {
                if libc::write(procs.as_raw_fd(), b"0".as_ptr().cast(), 1) != 1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut daemon = Self {
            child: command.spawn()?,
            client,
            url: String::new(),
            key,
            state,
            scope,
            instances,
            log,
        };
        let authority = wait_for(|| {
            ensure!(
                daemon.child.try_wait()?.is_none(),
                "{name} daemon exited before listening"
            );
            let text = fs::read_to_string(&daemon.log)?;
            Ok(text.lines().find_map(|line| {
                line.strip_prefix("pVisor sandbox service listening on ")
                    .map(str::to_owned)
            }))
        })?;
        daemon.url = format!("http://{authority}");
        daemon.call(Method::GET, BASE, None, 200)?;
        Ok(daemon)
    }

    fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        expected: u16,
    ) -> Result<String> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.url))
            .header("OPEN-SANDBOX-API-KEY", &self.key);
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(body.to_string());
        }
        let response = request.send()?;
        let status = response.status().as_u16();
        let text = response.text()?;
        ensure!(
            status == expected,
            "{path}: expected {expected}, got {status}: {text}"
        );
        Ok(text)
    }

    fn parked(&self) -> Result<String> {
        wait_for(|| {
            let text = fs::read_to_string(&self.log)?;
            let last = text.lines().rev().find_map(|line| {
                line.strip_prefix("warm pool parked ")?
                    .split_whitespace()
                    .next()
            });
            Ok(last
                .filter(|id| {
                    let directory = self.state.join(id);
                    directory.join("warm.json").is_file()
                        && !directory.join("identity.json").exists()
                })
                .map(str::to_owned))
        })
    }

    fn launch(&self, image: &str, token: &str, barrier: Option<&Barrier>) -> Result<Trial<'_>> {
        let payload = json!({
            "image": {"uri": image},
            "entrypoint": ["/bin/sleep", "120"],
            "resourceLimits": {"cpu": "4", "memory": "256Mi"},
            "timeout": 120,
        });
        let command = json!({"argv":["/bin/sh","-c",SHELL,"sh",MARKER,token]});
        if let Some(barrier) = barrier {
            barrier.wait();
        }
        let start = Instant::now();
        let response = self.call(Method::POST, BASE, Some(payload), 202)?;
        let ready = start.elapsed();
        let response: Value = serde_json::from_str(&response)?;
        let id = response["id"].as_str().context("missing sandbox ID")?;
        ensure!(
            id.starts_with("sb-") && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "invalid sandbox ID"
        );
        let mut trial = Trial {
            daemon: self,
            id: id.into(),
            alive: true,
            ready,
            finished: Duration::ZERO,
            events: Vec::new(),
        };
        ensure!(
            response["status"]["state"] == "Running",
            "create did not return Running"
        );
        let events = self.call(
            Method::POST,
            &format!("{}/{id}/proxy/44772/command", BASE),
            Some(command),
            200,
        )?;
        trial.finished = start.elapsed();
        trial.events = events
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(serde_json::from_str)
            .collect::<std::result::Result<_, _>>()?;
        let mut stdout = String::new();
        let mut complete = 0;
        for event in &trial.events {
            match event["type"].as_str() {
                Some("stdout") => stdout.push_str(event["text"].as_str().context("stdout text")?),
                Some("execution_complete") => complete += 1,
                Some("init") => {}
                other => bail!("unexpected command event {other:?}: {event}"),
            }
        }
        ensure!(
            stdout == format!("{token}\n") && complete == 1,
            "command output or exit mismatch: {stdout:?}"
        );
        Ok(trial)
    }

    fn stop(&mut self) -> Result<()> {
        ensure!(
            self.child.try_wait()?.is_none(),
            "daemon exited unexpectedly"
        );
        ensure!(
            unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) } == 0,
            "stop daemon"
        );
        let status = wait_for(|| Ok(self.child.try_wait()?))?;
        ensure!(status.success(), "daemon shutdown failed: {status}");
        ensure!(
            groups(&self.instances)?.is_empty(),
            "daemon shutdown left instance cgroups"
        );
        Ok(())
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = fs::write(self.scope.join("cgroup.kill"), "1");
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct Trial<'a> {
    daemon: &'a Daemon,
    id: String,
    alive: bool,
    ready: Duration,
    finished: Duration,
    events: Vec<Value>,
}

#[derive(Serialize)]
struct Sample {
    id: String,
    create_ns: u64,
    first_command_ns: u64,
    hit: bool,
    startup: Value,
    events: Vec<Value>,
}

impl Trial<'_> {
    fn path(&self) -> String {
        format!("{BASE}/{}", self.id)
    }

    fn finish(mut self, token: &str) -> Result<Sample> {
        let directory = self.daemon.state.join(&self.id);
        let startup = read(directory.join("startup.json"))?;
        let identity = read(directory.join("identity.json"))?;
        let group = PathBuf::from(identity["cgroup"].as_str().context("cgroup path")?);
        ensure!(
            group.parent() == Some(self.daemon.instances.as_path()),
            "unexpected instance cgroup"
        );
        for (file, expected) in [
            ("cpu.max", "400000 100000"),
            ("memory.max", "268435456"),
            ("memory.swap.max", "0"),
        ] {
            ensure!(
                fs::read_to_string(group.join(file))?.trim() == expected,
                "instance limits differ"
            );
        }
        oom_free(&group)?;
        let path = self.path();
        let file = self.daemon.call(
            Method::GET,
            &format!("{path}/proxy/44772/files/download?path={MARKER}"),
            None,
            200,
        )?;
        ensure!(
            file == format!("{token}\n"),
            "private file changed after command completed"
        );
        self.daemon.call(Method::DELETE, &path, None, 204)?;
        self.alive = false;
        self.daemon.call(Method::GET, &path, None, 404)?;
        ensure!(!group.exists(), "instance cgroup leaked");
        let hit = startup.get("warm_root_assigned_us").is_some();
        if hit {
            ensure!(
                startup["pid1_waiting_for_root_us"]
                    .as_u64()
                    .context("missing park clock")?
                    <= startup["warm_root_assigned_us"]
                        .as_u64()
                        .context("missing assignment clock")?
                    && startup["warm_root_assigned_us"].as_u64()
                        <= startup["services_initialized_us"].as_u64(),
                "warm startup clocks out of order"
            );
        }
        Ok(Sample {
            id: self.id.clone(),
            create_ns: self.ready.as_nanos().try_into()?,
            first_command_ns: self.finished.as_nanos().try_into()?,
            hit,
            startup,
            events: std::mem::take(&mut self.events),
        })
    }
}

impl Drop for Trial<'_> {
    fn drop(&mut self) {
        if self.alive {
            let _ = self.daemon.call(Method::DELETE, &self.path(), None, 204);
        }
    }
}

#[derive(Serialize)]
struct Pair {
    phase: &'static str,
    warm_first: bool,
    cold: Sample,
    hit: Sample,
    burst: [Sample; 2],
}

fn pair(
    cold: &Daemon,
    warm: &Daemon,
    image: &str,
    phase: &'static str,
    index: usize,
    warm_first: bool,
) -> Result<Pair> {
    let t = |suffix| format!("warm-pool-{phase}-{index}-{suffix}");
    let cold_one = || {
        let token = t("cold");
        cold.launch(image, &token, None)?.finish(&token)
    };
    let hit_one = || {
        let parked = warm.parked()?;
        let token = t("hit");
        let sample = warm.launch(image, &token, None)?.finish(&token)?;
        ensure!(
            sample.hit && sample.id == parked,
            "serial create did not consume the observed parked slot"
        );
        Ok::<_, anyhow::Error>(sample)
    };
    let (cold, hit) = if warm_first {
        let hit = hit_one()?;
        (cold_one()?, hit)
    } else {
        (cold_one()?, hit_one()?)
    };
    ensure!(!cold.hit, "cold baseline unexpectedly hit");
    let parked = warm.parked()?;
    let a = t("burst-a");
    let b = t("burst-b");
    let barrier = Barrier::new(2);
    let [first, second] = thread::scope(|threads| {
        let first = threads.spawn(|| warm.launch(image, &a, Some(&barrier)));
        let second = threads.spawn(|| warm.launch(image, &b, Some(&barrier)));
        [
            first.join().expect("HTTP worker panicked"),
            second.join().expect("HTTP worker panicked"),
        ]
    });
    let burst = [first?.finish(&a)?, second?.finish(&b)?];
    ensure!(
        burst.iter().filter(|trial| trial.hit).count() == 1
            && burst.iter().any(|trial| trial.hit && trial.id == parked),
        "two-request burst did not observe one hit and one fallback"
    );
    Ok(Pair {
        phase,
        warm_first,
        cold,
        hit,
        burst,
    })
}

fn draw(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn median(sorted: &[f64]) -> f64 {
    (sorted[(sorted.len() - 1) / 2] + sorted[sorted.len() / 2]) / 2.0
}

fn distribution(data: impl Iterator<Item = u64>) -> Value {
    let mut ms: Vec<_> = data.map(|ns| ns as f64 / 1e6).collect();
    ms.sort_by(f64::total_cmp);
    json!({
        "n": ms.len(), "p50_ms": median(&ms),
        "p95_ms": ms[(ms.len() * 95).div_ceil(100) - 1],
        "min_ms": ms[0], "max_ms": ms[ms.len() - 1],
        "below_1_ms": ms.iter().filter(|&&m| m < 1.0).count(),
    })
}

fn metrics(pairs: &[Pair], time: impl Fn(&Sample) -> u64 + Copy) -> Value {
    let mut differences: Vec<_> = pairs
        .iter()
        .map(|p| (time(&p.cold) as f64 - time(&p.hit) as f64) / 1e6)
        .collect();
    differences.sort_by(f64::total_cmp);
    let mut rng = 92763;
    let mut estimates = Vec::with_capacity(10_000);
    for _ in 0..10_000 {
        let mut resample: Vec<_> = differences
            .iter()
            .map(|_| differences[draw(&mut rng) as usize % differences.len()])
            .collect();
        resample.sort_by(f64::total_cmp);
        estimates.push(median(&resample));
    }
    estimates.sort_by(f64::total_cmp);
    let burst = |hit| {
        distribution(
            pairs
                .iter()
                .flat_map(|p| &p.burst)
                .filter(move |s| s.hit == hit)
                .map(time),
        )
    };
    json!({
        "cold": distribution(pairs.iter().map(|p| time(&p.cold))),
        "hit": distribution(pairs.iter().map(|p| time(&p.hit))),
        "burst_hit": burst(true), "burst_miss": burst(false),
        "paired_saving_ms": median(&differences),
        "paired_saving_ci95_ms": [estimates[249], estimates[9749]],
    })
}

pub(super) fn register(criterion: &mut Criterion) -> Result<()> {
    let output = PathBuf::from(
        env::var_os("PVISOR_BENCH_OUTPUT").context("new evidence directory is required")?,
    );
    fs::DirBuilder::new().mode(0o700).create(&output)?;
    if let Err(error) = measure(criterion, &output) {
        let _ = save(
            output.join("failure.json"),
            &json!({"error": format!("{error:#}")}),
        );
        return Err(error);
    }
    Ok(())
}

fn measure(criterion: &mut Criterion, output: &Path) -> Result<()> {
    let binary = input("PVISOR_BENCH_BINARY")?;
    let images = input("PVISOR_BENCH_DAEMON_IMAGES")?;
    let image = env::var("PVISOR_BENCH_DAEMON_IMAGE")?;
    ensure!(
        !image.is_empty()
            && image
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "invalid image key"
    );
    let manifest = images.join(format!("{image}.json"));
    let image_record = read(&manifest)?;
    ensure!(
        image_record["native_guest"] == true
            && image_record["e2b"].is_null()
            && image_record["library_dir"].is_null()
            && image_record["pooled_image"]["cache"]["repository"].is_null(),
        "need a local native OpenSandbox Ubuntu template using default firmware"
    );
    let bin_dir = output.join("binaries");
    fs::create_dir(&bin_dir)?;
    let frozen = bin_dir.join("pvisor-daemon");
    fs::copy(&binary, &frozen)?;
    let firmware = binary.with_file_name("libkrunfw.so.5");
    let dynamic = if firmware.is_file() {
        let target = bin_dir.join("libkrunfw.so.5");
        fs::copy(&firmware, &target)?;
        Some(digest(&target)?)
    } else {
        None
    };
    let binary_hash = digest(&frozen)?;
    let image_hash = digest(&manifest)?;
    let scope = scope()?;
    fs::write(output.join("harness.rs"), include_str!("warm_pool.rs"))?;
    fs::copy(&manifest, output.join("image-manifest.json"))?;
    save(
        output.join("environment.json"),
        &json!({
            "benchmark": "B-DAEMON-WARM-POOL", "daemon_sha256": binary_hash, "firmware_sha256": dynamic,
            "image_manifest_sha256": image_hash, "image_digest": image_record["pooled_image"]["digest"],
            "kernel": fs::read_to_string("/proc/sys/kernel/osrelease")?.trim(),
            "observer_status": fs::read_to_string("/proc/self/status")?,
            "host_cpu": fs::read_to_string("/proc/cpuinfo")?, "load_before": fs::read_to_string("/proc/loadavg")?,
            "scope_budget": {"cpu":"400000 100000", "memory_bytes":2147483648u64,"swap_bytes":0},
            "profile": PROFILE, "random_seed": 74723,
        }),
    )?;
    let temp = tempfile::Builder::new().prefix("pvd-").tempdir_in("/tmp")?;
    let mut cold = Daemon::start(&frozen, &images, &scope, temp.path(), output, false)?;
    let mut warm = Daemon::start(&frozen, &images, &scope, temp.path(), output, true)?;
    let marker = Path::new(MARKER);
    let before = fs::read(marker).ok();
    let mut rng = 74723;
    let mut raw = File::create(output.join("pairs.jsonl"))?;
    let mut samples = Vec::new();
    let mut trial_index = 0;
    let mut trial = |phase| -> Result<Duration> {
        let result = pair(
            &cold,
            &warm,
            &image,
            phase,
            trial_index,
            draw(&mut rng) & 1 == 0,
        )
        .with_context(|| format!("{phase} pair {trial_index}"))?;
        trial_index += 1;
        oom_free(&scope)?;
        ensure!(
            fs::read(marker).ok() == before,
            "guest modified host canary"
        );
        let measured =
            Duration::from_nanos(result.cold.first_command_ns + result.hit.first_command_ns);
        serde_json::to_writer(&mut raw, &result)?;
        raw.write_all(b"\n")?;
        if phase == "criterion" {
            samples.push(result);
        }
        Ok(measured)
    };
    trial("preflight")?;
    for _ in 0..3 {
        trial("protocol-warmup")?;
    }
    let mut group = criterion.benchmark_group("daemon-warm-pool");
    group.sample_size(40).sampling_mode(SamplingMode::Flat);
    group
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(3));
    group.bench_function("cold-plus-hit-first-command", |bencher| {
        bencher.iter_custom(|iterations| {
            match crate::command::measured_total(iterations, || trial("criterion")) {
                Ok(duration) => duration,
                Err(error) => {
                    let _ = save(
                        output.join("failure.json"),
                        &json!({"error": format!("{error:#}")}),
                    );
                    panic!("warm-pool correctness gate failed: {error:#}");
                }
            }
        });
    });
    group.finish();
    ensure!(samples.len() >= 30, "too few complete pairs");
    cold.stop()?;
    warm.stop()?;
    oom_free(&scope)?;
    ensure!(
        digest(&binary)? == binary_hash
            && digest(&frozen)? == binary_hash
            && digest(&manifest)? == image_hash
            && dynamic.is_none_or(|hash| digest(&bin_dir.join("libkrunfw.so.5"))
                .is_ok_and(|value| value == hash)
                && digest(&firmware).is_ok_and(|value| value == hash)),
        "input or artifact changed"
    );
    save(
        output.join("summary.json"),
        &json!({
            "create_to_running": metrics(&samples, |s| s.create_ns),
            "create_to_correct_first_command": metrics(&samples, |s| s.first_command_ns),
            "paired_operations": samples.len(),
            "burst_hits": samples.len(), "burst_requests": samples.len() * 2,
            "load_after": fs::read_to_string("/proc/loadavg")?,
            "failed": 0, "excluded_criterion_operations": 0,
        }),
    )
}
