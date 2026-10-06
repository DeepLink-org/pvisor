//! B-COLD-RUNTIME-ENG: fresh live VM, local cold store, no snapshots/offload.
use anyhow::{Context, ensure};
use clap::{Parser, ValueEnum};
use pvisor::{OverlayHint, PVisor, RunHandle, VmExecutor, VmSettings};
use pvisor_core::{RunInvocation, RunSpec, StdioMode};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use tokio::time::{Duration, sleep, timeout};

const BYTES: usize = 64 * 1024 * 1024;
const PAGE: usize = 4096;
const MIX: u64 = 0x9e3779b97f4a7c15;
static OWNS_OUTPUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Pattern {
    Repeated,
    RandomUnique,
}
#[derive(Parser, Clone, Debug, Serialize)]
struct Args {
    #[arg(long)]
    rootfs: PathBuf,
    #[arg(long)]
    firmware: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, value_enum)]
    pattern: Pattern,
    #[arg(long, action=clap::ArgAction::Set)]
    cold: bool,
    #[arg(long, default_value_t = 20261006)]
    seed: u64,
    #[arg(long, default_value_t=20, value_parser=clap::value_parser!(u64).range(20..=30))]
    wait1: u64,
    #[arg(long, default_value_t=35, value_parser=clap::value_parser!(u64).range(35..=45))]
    wait2: u64,
}
fn validate(_: &Args) -> anyhow::Result<()> {
    ensure!(
        cfg!(all(target_os = "linux", target_arch = "x86_64")),
        "Linux x86-64 required"
    );
    ensure!(
        std::env::var_os("PVISOR_EXPERIMENTAL_MEMORY_POOL").is_none(),
        "external pool forbidden"
    );
    Ok(())
}
fn page(seed: u64, instance: u64, index: usize, random: bool) -> [u8; PAGE] {
    let mut out = [0; PAGE];
    if !random {
        for (i, b) in out.iter_mut().enumerate() {
            *b = (i % 256) as u8;
        }
    } else {
        let mut x =
            seed ^ instance.wrapping_mul(0xd1b54a32d192ed03) ^ (index as u64).wrapping_mul(MIX);
        for word in out.chunks_exact_mut(8) {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            word.copy_from_slice(&x.to_le_bytes());
        }
    }
    out
}
fn persist(root: &Path, report: &Value) -> anyhow::Result<()> {
    fs::write(
        root.join("raw.json.tmp"),
        serde_json::to_vec_pretty(report)?,
    )?;
    fs::rename(root.join("raw.json.tmp"), root.join("raw.json"))?;
    Ok(())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn expected(a: &Args) -> BTreeMap<(usize, usize), String> {
    let mut result = BTreeMap::new();
    for percent in [0, 100] {
        let mut digest = Sha256::new();
        for p in 0..BYTES / PAGE {
            let instance = if percent == 100 {
                1 | (1 << 32)
            } else if a.pattern == Pattern::RandomUnique {
                1
            } else {
                0
            };
            digest.update(page(
                a.seed,
                instance,
                p,
                percent == 100 || a.pattern != Pattern::Repeated,
            ));
        }
        result.insert((1, percent), hex(&digest.finalize()));
    }
    result
}
const GUEST: &str = r#"
import hashlib, json, pathlib, time, struct, sys, os
pattern, seed, initial_id = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
SIZE, PAGE, MIX, MASK = 64*1024*1024, 4096, 0x9e3779b97f4a7c15, (1<<64)-1
def page(instance, index, random):
    if not random: return bytes(range(256))*16
    x = seed ^ ((instance*0xd1b54a32d192ed03)&MASK) ^ ((index*MIX)&MASK)
    words = []
    for i in range(512):
        x = (x*6364136223846793005+1442695040888963407)&MASK
        words.append(x)
    return struct.pack('<512Q', *words)
data = bytearray(SIZE)
def fill(instance, percent, mutation=False):
    for p in range((SIZE//PAGE)*percent//100):
        data[p*PAGE:(p+1)*PAGE] = page(instance, p, mutation or pattern != 'repeated')
fill(initial_id if pattern == 'random-unique' else 0, 100)
scratch = bytearray(1024*1024)
n, last, current_id, percent = 0, '', initial_id, 0
def atomic(name, value):
    pathlib.Path(name+'.tmp').write_text(value)
    pathlib.Path(name+'.tmp').replace(name)
while True:
    assert scratch == n.to_bytes(8,'little')*(len(scratch)//8), 'scratch corruption'
    n += 1
    scratch[:] = n.to_bytes(8,'little')*(len(scratch)//8)
    atomic('heartbeat', str(n))
    try: req = json.loads(pathlib.Path('request').read_text())
    except (FileNotFoundError, ValueError): req = None
    if req and req['token'] != last:
        op = req['op']
        if op == 'mutate':
            assert req['instance'] == current_id
            assert hashlib.sha256(data).hexdigest() == req['pre_digest'], 'prewrite digest mismatch'
            percent = req['percent']
            fill(current_id | (1<<32), percent, True)
        elif op not in ('read', 'exit'): raise RuntimeError('unknown command')
        start = time.perf_counter_ns()
        digest = hashlib.sha256(data).hexdigest()
        digest_ms = (time.perf_counter_ns()-start)/1e6
        with open('device-io', 'wb') as f:
            assert f.write(data) == SIZE
            f.flush(); os.fsync(f.fileno())
        with open('device-io', 'rb') as f:
            for offset in range(0, SIZE, 65536):
                assert f.read(65536) == memoryview(data)[offset:offset+65536], 'device I/O mismatch'
            assert f.read(1) == b'', 'device I/O extra bytes'
        atomic('ack', json.dumps(dict(token=req['token'], op=op, instance=current_id,
            percent=percent, digest=digest, heartbeat=n, device_io_bytes=SIZE, digest_ms=digest_ms, read_ms=(time.perf_counter_ns()-start)/1e6)))
        last = req['token']
        if op == 'exit':
            print('guest-memory-ok '+digest, flush=True)
            break
    time.sleep(0.01)
"#;

struct Worker {
    id: usize,
    upper: PathBuf,
    handle: Option<RunHandle>,
    waiter: Option<tokio::task::JoinHandle<Result<pvisor_core::RunResult, pvisor::PVisorError>>>,
    cancellation: pvisor::RunCancellation,
    startup_heartbeat: u64,
    heartbeat: u64,
    reaped: bool,
}
impl Worker {
    fn handle(&self) -> anyhow::Result<&RunHandle> {
        self.handle.as_ref().context("worker already reaping")
    }
    fn alive(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(|h| !h.status().state.is_terminal())
    }
    async fn command(
        &mut self,
        token: &str,
        op: &str,
        percent: usize,
        digest: &str,
    ) -> anyhow::Result<Value> {
        timeout(Duration::from_secs(20), async {
            while !heartbeat_advanced(&self.upper, self.startup_heartbeat) {
                ensure!(
                    self.alive(),
                    "VM {} exited before protocol startup",
                    self.id
                );
                sleep(Duration::from_millis(10)).await;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("guest startup timeout")??;
        let req = json!({"token":token,"op":op,"instance":self.id,"percent":percent,"pre_digest":fs::read_to_string(self.upper.join("expected-prewrite")).unwrap_or_default()});
        fs::write(
            self.upper.join("request.host.tmp"),
            serde_json::to_vec(&req)?,
        )?;
        fs::rename(
            self.upper.join("request.host.tmp"),
            self.upper.join("request"),
        )?;
        timeout(Duration::from_secs(20), async {
            loop {
                if let Ok(bytes) = fs::read(self.upper.join("ack"))
                    && let Ok(ack) = serde_json::from_slice::<Value>(&bytes)
                    && ack["token"] == token
                {
                    ensure!(
                        ack["digest"] == digest,
                        "VM {} full payload mismatch at {token}: {ack}",
                        self.id
                    );
                    ensure!(
                        ack["op"] == op && ack["percent"] == percent && ack["instance"] == self.id,
                        "guest command/state mismatch: {ack}"
                    );
                    let n = ack_heartbeat(&ack, self.heartbeat)
                        .with_context(|| format!("VM {} invalid heartbeat at {token}", self.id))?;
                    self.heartbeat = n;
                    return Ok(ack);
                }
                ensure!(self.alive(), "VM {} exited before {token}", self.id);
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("guest command timeout")?
    }
    async fn wait_for_progress(&mut self, previous: u64) -> anyhow::Result<u64> {
        timeout(Duration::from_secs(20), async {
            loop {
                ensure!(self.alive(), "VM {} exited before resume progress", self.id);
                if let Some(n) = heartbeat_value(&self.upper)
                    && n > previous
                {
                    ensure!(
                        n >= self.heartbeat,
                        "VM {} resume heartbeat regressed",
                        self.id
                    );
                    self.heartbeat = n;
                    return Ok(n);
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("guest resume progress timeout")?
    }
    fn begin_wait(&mut self) {
        if let Some(h) = self.handle.take() {
            self.waiter = Some(tokio::spawn(h.wait()));
        }
    }
    async fn reap(
        &mut self,
        root: &Path,
        report: &mut Value,
    ) -> anyhow::Result<pvisor_core::RunResult> {
        self.begin_wait();
        // Keep the JoinHandle in Worker if this caller's deadline expires.
        let waited = self.waiter.as_mut().context("already reaped")?.await;
        self.waiter = None;
        let result = waited??;
        self.reaped = true;
        fs::write(
            root.join(format!("guest-{}.stderr", self.id)),
            result.output.stderr.as_deref().unwrap_or_default(),
        )?;
        fs::write(
            root.join(format!("guest-{}.stdout", self.id)),
            result.output.stdout.as_deref().unwrap_or_default(),
        )?;
        report["guests"]
            .as_array_mut()
            .unwrap()
            .push(json!({"instance":self.id,"result":result}));
        Ok(result)
    }
}
fn spec(a: &Args, workspace: &Path, id: usize) -> RunSpec {
    let mut s = RunSpec::process("memory-scale", "integrity", "/usr/bin/python3");
    s.runtime.timeout_ms = Some(165_000);
    let RunInvocation::Process(p) = &mut s.invocation;
    p.args = vec![
        "-c".into(),
        GUEST.into(),
        match a.pattern {
            Pattern::Repeated => "repeated",
            Pattern::RandomUnique => "random-unique",
        }
        .into(),
        a.seed.to_string(),
        id.to_string(),
    ];
    p.cwd = Some("/workspace".into());
    p.stdout = StdioMode::Capture;
    // The bounded coordinator retains and timestamps pager telemetry live.
    p.stderr = StdioMode::Inherit;
    s.metadata
        .insert("pvisor.workspace".into(), json!(workspace));
    s.metadata
        .insert("pvisor.vm.overlay_target".into(), json!("/workspace"));
    s.metadata
        .insert("pvisor.vm.guest_cwd".into(), json!("/workspace"));
    s
}
fn network_profile() -> pvisor::NetworkDriverConfig {
    // Auto installs a VM NIC even for deny policies, which excludes native capture.
    pvisor::NetworkDriverConfig::new(
        pvisor::OverlayNetMode::Off,
        pvisor_core::NetworkConfig {
            mode: pvisor_core::NetworkMode::NoNetwork,
            ..Default::default()
        },
    )
}
fn heartbeat_value(upper: &Path) -> Option<u64> {
    fs::read_to_string(upper.join("heartbeat"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
}
fn heartbeat_advanced(upper: &Path, previous: u64) -> bool {
    heartbeat_value(upper).is_some_and(|n| n > previous)
}
fn ack_heartbeat(ack: &Value, previous: u64) -> anyhow::Result<u64> {
    // Token/op/digest are checked first. A new command can finish in the same
    // iteration as the sampled heartbeat; liveness is checked separately.
    let n = ack["heartbeat"].as_u64().context("missing heartbeat")?;
    ensure!(
        n >= previous,
        "guest heartbeat regressed from {previous}: {ack}"
    );
    Ok(n)
}
fn protocol_upper(overlay: &OverlayHint) -> anyhow::Result<PathBuf> {
    // Restore projects its writable upper independently of the metadata stage.
    overlay
        .upper_dir
        .clone()
        .or_else(|| overlay.stage_dir.as_ref().map(|stage| stage.join("upper")))
        .context("workspace overlay has neither an explicit upper nor a stage")
}
async fn start(a: &Args, root: &Path, workspace: &Path, id: usize) -> anyhow::Result<Worker> {
    let storage = root.join("s1");
    let settings = VmSettings {
        rootfs: Some(a.rootfs.canonicalize()?),
        library_dir: Some(a.firmware.canonicalize()?),
        cold_ram_compression: a.cold,
        memory_mib: 256,
        cpus: 1,
        ..Default::default()
    };
    let executor = VmExecutor::new(settings)?;
    let overlay = OverlayHint {
        lower_dirs: vec![workspace.to_owned()],
        stage_dir: Some(root.join("t1")),
        ..Default::default()
    };
    let upper = protocol_upper(&overlay)?;
    let runtime = PVisor::builder()
        .network(network_profile())
        .executors(vec![Arc::new(executor)])
        .storage(storage)
        .overlay(overlay)
        .build();
    let handle = runtime.run(spec(a, workspace, id)).await?;
    let cancellation = handle.cancellation();
    Ok(Worker {
        id,
        upper,
        handle: Some(handle),
        waiter: None,
        cancellation,
        startup_heartbeat: 0,
        heartbeat: 0,
        reaped: false,
    })
}
fn evidence_file(path: &Path) -> Value {
    match fs::read_to_string(path) {
        Ok(s) => json!({"raw":s}),
        Err(e) => json!({"error":e.to_string()}),
    }
}
fn cgroup_root() -> anyhow::Result<PathBuf> {
    let membership = fs::read_to_string("/proc/self/cgroup")?;
    let relative = membership
        .lines()
        .find_map(|s| s.strip_prefix("0::"))
        .context("requires cgroup v2")?;
    Ok(Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/')))
}
fn collect_pids(root: &Path, pids: &mut BTreeSet<u32>, errors: &mut Vec<String>) {
    match fs::read_to_string(root.join("cgroup.procs")) {
        Ok(s) => pids.extend(s.lines().filter_map(|s| s.parse::<u32>().ok())),
        Err(e) => errors.push(format!("{}: {e}", root.display())),
    }
    match fs::read_dir(root) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    collect_pids(&entry.path(), pids, errors);
                }
            }
        }
        Err(e) => errors.push(format!("{}: {e}", root.display())),
    }
}
fn accounting() -> anyhow::Result<Value> {
    let root = cgroup_root()?;
    let mut counters = serde_json::Map::new();
    for name in [
        "memory.current",
        "memory.peak",
        "memory.stat",
        "memory.events",
        "memory.events.local",
        "memory.pressure",
        "memory.swap.current",
        "memory.max",
        "memory.swap.max",
        "cpu.stat",
        "cpu.max",
        "cpu.pressure",
        "pids.current",
    ] {
        counters.insert(name.into(), evidence_file(&root.join(name)));
    }
    let mut pids = BTreeSet::new();
    let mut errors = Vec::new();
    collect_pids(&root, &mut pids, &mut errors);
    let mut processes = Vec::new();
    for pid in pids {
        let proc = PathBuf::from(format!("/proc/{pid}"));
        let mut sums = BTreeMap::<String, u64>::new();
        let smaps = evidence_file(&proc.join("smaps"));
        if let Some(raw) = smaps["raw"].as_str() {
            for line in raw.lines() {
                let mut words = line.split_whitespace();
                if let Some(key) = words.next()
                    && ["Rss:", "Pss:", "KSM:", "Private_Dirty:", "Shared_Clean:"].contains(&key)
                    && let Some(n) = words.next().and_then(|n| n.parse::<u64>().ok())
                {
                    *sums.entry(key.trim_end_matches(':').into()).or_default() += n * 1024;
                }
            }
        }
        let mut fds = Vec::new();
        let entries = fs::read_dir(proc.join("fd"));
        let fd_error = entries.as_ref().err().map(|e| e.to_string());
        if let Ok(entries) = entries {
            for entry in entries.flatten() {
                if let Ok(m) = fs::metadata(entry.path())
                    && m.is_file()
                    && m.len() >= BYTES as u64
                {
                    fds.push(json!({"fd":entry.file_name().to_string_lossy(),"target":fs::read_link(entry.path()).ok(),"dev":m.dev(),"inode":m.ino(),"bytes":m.len()}));
                }
            }
        }
        processes.push(json!({"pid":pid,"cmdline":evidence_file(&proc.join("cmdline")),"status":evidence_file(&proc.join("status")),"smaps":smaps,"smaps_totals_bytes":sums,"large_file_fds":fds,"fd_inspection_error":fd_error}));
    }
    let mut ksm = serde_json::Map::new();
    for name in [
        "run",
        "pages_shared",
        "pages_sharing",
        "pages_unshared",
        "pages_volatile",
        "full_scans",
        "pages_to_scan",
        "sleep_millisecs",
    ] {
        ksm.insert(
            name.into(),
            evidence_file(&Path::new("/sys/kernel/mm/ksm").join(name)),
        );
    }
    Ok(
        json!({"cgroup":root,"counters":counters,"processes":processes,"process_errors":errors,"ksm":ksm}),
    )
}
async fn heartbeats(group: &[Worker]) -> anyhow::Result<Vec<u64>> {
    group
        .iter()
        .map(|w| {
            Ok(fs::read_to_string(w.upper.join("heartbeat"))?
                .trim()
                .parse()?)
        })
        .collect()
}
async fn phase(
    a: &Args,
    group: &mut [Worker],
    name: &str,
    offloaded: bool,
    report: &mut Value,
    start: Instant,
) -> anyhow::Result<()> {
    if !offloaded {
        for w in group.iter() {
            w.handle()?.pause_vm().await?;
        }
    }
    // Pause acknowledges CPUs, not all outstanding virtio-fs device writes.
    sleep(Duration::from_millis(300)).await;
    let before = heartbeats(group).await?;
    sleep(Duration::from_millis(100)).await;
    ensure!(
        heartbeats(group).await? == before,
        "heartbeat advanced during {name} barrier"
    );
    for (w, &n) in group.iter_mut().zip(&before) {
        ensure!(
            n >= w.heartbeat,
            "VM {} heartbeat regressed at {name}",
            w.id
        );
        w.heartbeat = n;
    }
    let sample = accounting()?;
    report["phases"].as_array_mut().unwrap().push(json!({"name":name,"elapsed_ms":start.elapsed().as_millis(),"paused":true,"offloaded":offloaded,"instances":group.iter().map(|w|w.id).collect::<Vec<_>>(),"heartbeat":before,"heartbeat_stable":true,"accounting":sample}));
    persist(&a.output, report)?;
    println!(
        "{}",
        json!({"schema":"pvisor-memory-scale/v1","phase":name,"output":a.output})
    );
    if !offloaded {
        for w in group.iter() {
            w.handle()?.resume_vm().await?;
        }
        for w in group.iter_mut() {
            let parked = w.heartbeat;
            let advanced = w.wait_for_progress(parked).await?;
            check(
                report,
                "resume_heartbeat_progress",
                json!({"phase":name,"instance":w.id,"before":parked,"after":advanced}),
            );
        }
    }
    Ok(())
}
fn check(report: &mut Value, name: &str, evidence: Value) {
    report["checks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":name,"passed":true,"evidence":evidence}));
}
async fn experiment(
    a: &Args,
    root: &Path,
    workers: &mut Vec<Worker>,
    report: &mut Value,
    hashes: &BTreeMap<(usize, usize), String>,
    started: Instant,
) -> anyhow::Result<()> {
    let workspace = root.join("workspace");
    fs::create_dir(&workspace)?;
    workers.push(start(a, root, &workspace, 1).await?);
    workers[0].wait_for_progress(0).await?;
    let baseline = &hashes[&(1, 0)];
    let mutated = &hashes[&(1, 100)];
    for (token, op, percent, digest, wait) in [
        ("ready", "read", 0, baseline, 0),
        ("restore1", "read", 0, baseline, a.wait1),
        ("mutation", "mutate", 100, mutated, 0),
        ("restore2", "read", 100, mutated, a.wait2),
    ] {
        if wait > 0 {
            let before = workers[0].heartbeat;
            sleep(Duration::from_secs(wait)).await;
            workers[0].wait_for_progress(before).await?;
            phase(
                a,
                workers,
                if token == "restore1" {
                    "cold1"
                } else {
                    "cold2"
                },
                false,
                report,
                started,
            )
            .await?;
        }
        if op == "mutate" {
            fs::write(workers[0].upper.join("expected-prewrite"), baseline)?;
        }
        let ack = workers[0].command(token, op, percent, digest).await?;
        ensure!(ack["device_io_bytes"] == BYTES, "device I/O proof missing");
        check(report, token, ack);
        sleep(Duration::from_millis(500)).await;
        phase(a, workers, token, false, report, started).await?;
    }
    let ack = workers[0].command("exit", "exit", 100, mutated).await?;
    check(report, "exit", ack);
    let result = workers[0].reap(root, report).await?;
    ensure!(
        result.state == pvisor_core::RunState::Completed && result.exit_code == Some(0),
        "guest exit failure: {result:?}"
    );
    Ok(())
}
async fn run(a: Args) -> anyhow::Result<()> {
    let start = Instant::now();
    fs::create_dir(&a.output).context("--output must be a NEW directory")?;
    OWNS_OUTPUT.store(true, std::sync::atomic::Ordering::Release);
    let root = a.output.canonicalize()?;
    let mut report = json!({"schema":"pvisor-cold-runtime/v1","conditions":a,"profile":{"memory_mib":256,"cpus":1,"payload_bytes":BYTES,"max_live_vms":1,"overlaynet_mode":"off","network_policy":"no-network"},"correctness":"failed","phases":[],"checks":[],"guests":[]});
    // Persist failure evidence even if validation, startup or capture fails.
    persist(&root, &report)?;
    let mut workers = Vec::new();
    let result = timeout(Duration::from_secs(150), async {
        validate(&a)?;
        ensure!(root.as_os_str().len() <= 70, "output path must be short (<=70 bytes) for native control sockets");
        let croot = std::ffi::CString::new(root.as_os_str().as_encoded_bytes())?;
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        ensure!(unsafe { libc::statfs(croot.as_ptr(), stat.as_mut_ptr()) } == 0, "cannot inspect output filesystem");
        ensure!(unsafe { stat.assume_init() }.f_type != 0x01021994, "output must not be tmpfs");
        report["before"] = accounting()?;
        let cg = cgroup_root()?;
        ensure!(fs::read_to_string(cg.join("memory.max"))?.trim() == "2147483648", "parent must set group memory.max=2147483648 (2 GiB)");
        ensure!(fs::read_to_string(cg.join("memory.swap.max"))?.trim() == "0", "parent must set group memory.swap.max=0");
        let cpu = fs::read_to_string(cg.join("cpu.max"))?;
        let parts: Vec<_> = cpu.split_whitespace().collect();
        ensure!(parts.len() == 2 && parts[0].parse::<u64>().ok().zip(parts[1].parse::<u64>().ok()).is_some_and(|(quota, period)| period > 0 && quota == period.saturating_mul(4)), "parent must set cpu.max to exactly four cores");
        check(&mut report, "group_budget", json!({"memory_max":2147483648u64,"swap_max":0,"cpu_max":cpu}));
        report["source"] = json!({"binary_sha256":pvisor::environment_snapshot::file_hash(&std::env::current_exe()?)?,"rootfs":a.rootfs.canonicalize()?,"firmware":a.firmware.canonicalize()?,"compatibility":VmExecutor::checkpoint_compatibility(&VmSettings { library_dir:Some(a.firmware.canonicalize()?), ..Default::default() })?,"guest_sha256":hex(&Sha256::digest(GUEST.as_bytes())),"payload_algorithm":"page-local LCG64 little-endian v1; repeated=bytes(range(256))*16; mutations instance|(1<<32)"});
        // Hash work must yield to the deadline, not block the async supervisor.
        let hash_args = a.clone();
        let hashes = tokio::task::spawn_blocking(move || expected(&hash_args)).await?;
        report["expected_digests"] = json!(hashes.iter().map(|(&(id,percent),digest)|json!({"instance":id,"percent":percent,"digest":digest})).collect::<Vec<_>>());
        experiment(&a, &root, &mut workers, &mut report, &hashes, start).await
    }).await.unwrap_or_else(|_| Err(anyhow::anyhow!("150-second experiment deadline; cancelling all owned VMs")));
    if let Err(e) = &result {
        report["error"] = json!(format!("{e:#}"));
        report["checks"].as_array_mut().unwrap().push(
            json!({"name":"experiment_completion","passed":false,"evidence":format!("{e:#}")}),
        );
        eprintln!("cold-runtime: {e:#}");
    }
    // Cancel all unfinished attempts first, then reap concurrently under ONE
    // cleanup deadline. The terminal result is the public native-reaping fence.
    for w in &mut workers {
        if w.handle.is_some() || w.waiter.is_some() {
            w.cancellation.cancel();
            w.begin_wait();
        }
    }
    let cleanup = timeout(Duration::from_secs(20), async {
        let mut errors = Vec::new();
        for w in &mut workers {
            if w.waiter.is_some() {
                if let Err(e) = w.reap(&root, &mut report).await {
                    errors.push(format!("VM {}: {e:#}", w.id));
                }
            }
        }
        ensure!(
            workers.iter().all(|w| w.reaped),
            "not every owned VM has a terminal reaping receipt; {}",
            errors.join("; ")
        );
        ensure!(errors.is_empty(), "cleanup errors: {}", errors.join("; "));
        Ok::<_, anyhow::Error>(())
    })
    .await;
    let cleanup_ok = matches!(cleanup, Ok(Ok(())));
    report["cleanup"] = json!({"all_reaped":cleanup_ok,"error":match cleanup { Ok(Ok(())) => None, Ok(Err(e)) => Some(format!("{e:#}")), Err(e) => Some(e.to_string()) }});
    let cleanup_evidence = report["cleanup"].clone();
    report["checks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"cleanup_all_reaped","passed":cleanup_ok,"evidence":cleanup_evidence}));
    report["correctness"] = json!(if result.is_ok() && cleanup_ok {
        "passed"
    } else {
        "failed"
    });
    report["elapsed_ms"] = json!(start.elapsed().as_millis());
    report["after"] = accounting().unwrap_or_else(|e| json!({"error":format!("{e:#}")}));
    persist(&root, &report)?;
    println!(
        "{}",
        json!({"schema":"pvisor-cold-runtime/v1","raw_json":root.join("raw.json"),"correctness":report["correctness"]})
    );
    result?;
    ensure!(cleanup_ok, "cleanup deadline/error; see raw.json");
    Ok(())
}
fn main() -> anyhow::Result<()> {
    if pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    let a = Args::parse();
    let watchdog_output = a.output.clone();
    // A final watchdog bounds even synchronous SDK/filesystem stalls. Normal
    // failures get the asynchronous cleanup window and complete raw.json first.
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(179));
        eprintln!("cold-runtime: hard deadline watchdog (parent must reap cgroup)");
        // Filesystem I/O must not prevent the watchdog itself from exiting.
        if OWNS_OUTPUT.load(std::sync::atomic::Ordering::Acquire) {
            std::thread::spawn(move || {
                if let Ok(bytes) = fs::read(watchdog_output.join("raw.json"))
                    && let Ok(mut report) = serde_json::from_slice::<Value>(&bytes)
                    && report["schema"] == "pvisor-cold-runtime/v1"
                {
                    report["correctness"] = json!("failed");
                    report["error"] = json!(
                        "hard deadline watchdog; synchronous stall; parent must cancel/reap cgroup"
                    );
                    report["cleanup"] = json!({"all_reaped":false,"error":"watchdog; native termination not proven"});
                    let _ = persist(&watchdog_output, &report);
                }
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        std::process::exit(124);
    });
    tokio::runtime::Runtime::new()?.block_on(run(a))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn python_protocol_matches_rust_oracle() {
        // Protocol test only: four pages, ordinary host Python, no VM/accounting.
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        for pattern in ["repeated", "random-unique"] {
            let dir = tempfile::tempdir().unwrap();
            let guest = GUEST.replace("64*1024*1024", "4*4096");
            let mut child = Child(
                std::process::Command::new("python3")
                    .args(["-c", &guest, pattern, "18446744073709551615", "1"])
                    .current_dir(dir.path())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
            let mut heartbeat = 0;
            let mut baseline = Sha256::new();
            for p in 0..4 {
                baseline.update(page(
                    u64::MAX,
                    if pattern == "random-unique" { 1 } else { 0 },
                    p,
                    pattern != "repeated",
                ));
            }
            let baseline = hex(&baseline.finalize());
            for (token, op, percent) in [
                ("ready", "read", 0),
                ("restore1", "read", 0),
                ("mutation", "mutate", 100),
                ("restore2", "read", 100),
                ("exit", "exit", 100),
            ] {
                let req = json!({"token":token,"op":op,"instance":1,"percent":percent,"pre_digest":baseline});
                fs::write(
                    dir.path().join("request.tmp.host"),
                    serde_json::to_vec(&req).unwrap(),
                )
                .unwrap();
                fs::rename(
                    dir.path().join("request.tmp.host"),
                    dir.path().join("request"),
                )
                .unwrap();
                let deadline = Instant::now() + std::time::Duration::from_secs(5);
                let ack = loop {
                    if let Ok(raw) = fs::read(dir.path().join("ack"))
                        && let Ok(ack) = serde_json::from_slice::<Value>(&raw)
                        && ack["token"] == token
                    {
                        break ack;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "Python protocol timed out at {pattern}/{token}"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(5));
                };
                let mut digest = Sha256::new();
                for p in 0..4 {
                    let mutated = p < 4 * percent / 100;
                    let instance = if mutated {
                        1 | (1 << 32)
                    } else if pattern == "random-unique" {
                        1
                    } else {
                        0
                    };
                    digest.update(page(
                        u64::MAX,
                        instance,
                        p,
                        mutated || pattern != "repeated",
                    ));
                }
                assert_eq!(ack["digest"], hex(&digest.finalize()), "{pattern}/{token}");
                assert_eq!(ack["op"], op);
                assert_eq!(ack["percent"], percent);
                let n = ack["heartbeat"].as_u64().unwrap();
                assert!(n > heartbeat);
                heartbeat = n;
            }
            let deadline = Instant::now() + std::time::Duration::from_secs(5);
            loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                assert!(Instant::now() < deadline, "Python did not exit");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
}
