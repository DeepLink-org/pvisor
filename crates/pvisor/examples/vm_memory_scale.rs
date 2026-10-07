//! Shared worker for B-MEMORY-SCALE engineering and B-VM-MEMORY user protocols.
//! The coordinator must declare its role and validate its complete cohort.
//! Question: how does a bounded 1/2/4-VM group behave under shared-baseline COW
//! and fresh-live offload, with complete data/state checks and cgroup evidence?
//! Parent owns randomized pairing, repetitions, resource limits and monitoring.
use anyhow::{Context, ensure};
use clap::{Parser, ValueEnum};
use pvisor::{OverlayHint, PVisor, RunHandle, VmExecutor, VmSettings};
use pvisor_core::operation::{
    ExecutionCheckpoint, ExecutionSuspension, OperationKind, SnapshotRamStorage,
};
use pvisor_core::{RunInvocation, RunSpec, RunState, StdioMode};
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
enum Mode {
    Fresh,
    Pool,
    Baseline,
    Ksm,
    Raw,
    Compressed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Pattern {
    Repeated,
    RandomUnique,
    RandomShared,
}
#[derive(Parser, Clone, Debug, Serialize)]
#[command(
    about = "B-MEMORY-SCALE: bounded real-VM SDK worker; parent must supply a disk output and delegated cgroup"
)]
struct Args {
    /// Frozen pvisor-daemon executable for the daemon-owned pool arm.
    #[arg(long)]
    pool_daemon: Option<PathBuf>,
    #[arg(skip)]
    pool_socket: Option<PathBuf>,
    #[arg(long, default_value_t = 256, value_parser = clap::value_parser!(u32).range(256..=512))]
    memory_mib: u32,
    #[arg(long)]
    rootfs: PathBuf,
    /// Directory containing libkrunfw (same convention as other SDK examples).
    #[arg(long)]
    firmware: PathBuf,
    /// NEW, short directory on a real disk; never reused or overwritten.
    #[arg(long)]
    output: PathBuf,
    #[arg(long, value_parser = parse_vms)]
    vms: usize,
    #[arg(long, value_enum)]
    mode: Mode,
    #[arg(long, value_enum)]
    pattern: Pattern,
    /// Explicit true/false; advice is not evidence of actual KSM merging.
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    dedup: bool,
    #[arg(long, default_value_t = 20261006)]
    seed: u64,
    #[arg(long, default_value_t = 500, value_parser = clap::value_parser!(u64).range(0..=5000))]
    settle_ms: u64,
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(0..=60))]
    ksm_wait_seconds: u64,
    /// Unsupported: requires independently captured backing inodes, not cloned references.
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    private_baselines: bool,
    /// Restore identical sealed bytes through genuinely independent store/RAM inodes.
    #[arg(long, default_value_t = false, action = clap::ArgAction::Set)]
    independent_inodes: bool,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..=2))]
    cpus: u16,
}
fn parse_vms(s: &str) -> Result<usize, String> {
    match s {
        "1" => Ok(1),
        "2" => Ok(2),
        "4" => Ok(4),
        _ => Err("--vms must be 1, 2 or 4 (hard cap 4)".into()),
    }
}
fn validate(a: &Args) -> anyhow::Result<()> {
    ensure!(
        cfg!(all(target_os = "linux", target_arch = "x86_64")),
        "requires Linux x86-64"
    );
    ensure!(
        !a.private_baselines,
        "private-baselines unsupported: independent native captures/inodes are not implemented; no fake private control"
    );
    ensure!(
        !a.independent_inodes || matches!(a.mode, Mode::Baseline | Mode::Ksm),
        "independent inodes require snapshot restore"
    );
    ensure!(
        !(a.dedup && a.mode == Mode::Compressed),
        "dedup and compressed cold RAM are mutually exclusive"
    );
    Ok(())
}

// Deliberately specified word-by-word so Rust computes the entire expected
// SHA-256 independently of Python and never accepts a guest-provided oracle.
fn page(seed: u64, instance: u64, index: usize, random: bool) -> [u8; PAGE] {
    let mut out = [0; PAGE];
    if !random {
        for (i, b) in out.iter_mut().enumerate() {
            *b = (i % 256) as u8;
        }
    } else {
        let mut x =
            seed ^ instance.wrapping_mul(0xd1b54a32d192ed03) ^ (index as u64).wrapping_mul(MIX);
        for word in out.as_chunks_mut::<8>().0 {
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
    let mut baseline = Sha256::new();
    for p in 0..BYTES / PAGE {
        baseline.update(page(a.seed, 0, p, a.pattern != Pattern::Repeated));
    }
    let baseline = hex(&baseline.finalize());
    result.insert((0, 0), baseline.clone());
    for id in 1..=a.vms {
        for &percent in if matches!(
            a.mode,
            Mode::Fresh | Mode::Pool | Mode::Baseline | Mode::Ksm
        ) {
            &[0, 25, 100][..]
        } else {
            &[0][..]
        } {
            if percent == 0 && a.pattern != Pattern::RandomUnique {
                result.insert((id, percent), baseline.clone());
                continue;
            }
            let mut digest = Sha256::new();
            for p in 0..BYTES / PAGE {
                let mutated = p < (BYTES / PAGE) * percent / 100;
                let instance = if mutated {
                    (id as u64) | (1 << 32)
                } else if a.pattern == Pattern::RandomUnique {
                    id as u64
                } else {
                    0
                };
                digest.update(page(
                    a.seed,
                    instance,
                    p,
                    mutated || a.pattern != Pattern::Repeated,
                ));
            }
            result.insert((id, percent), hex(&digest.finalize()));
        }
    }
    result
}
const GUEST: &str = r#"
import hashlib, json, pathlib, time, struct, sys
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
        if op == 'prepare':
            if pattern == 'random-unique' and current_id != req['instance']: fill(req['instance'], 100)
            current_id = req['instance']
        elif op == 'duplicate':
            assert req['instance'] == current_id
            # Always store every page, even when contents are already identical.
            fill(current_id if pattern == 'random-unique' else 0, 100)
            percent = 0
        elif op == 'mutate':
            assert req['instance'] == current_id
            percent = req['percent']
            fill(current_id | (1<<32), percent, True)
        elif op not in ('read', 'exit'): raise RuntimeError('unknown command')
        start = time.perf_counter_ns()
        digest = hashlib.sha256(data).hexdigest()
        atomic('ack', json.dumps(dict(token=req['token'], op=op, instance=current_id,
            percent=percent, digest=digest, heartbeat=n, read_ms=(time.perf_counter_ns()-start)/1e6)))
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
        let req = json!({"token":token,"op":op,"instance":self.id,"percent":percent});
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
                        ack["op"] == op && ack["percent"] == percent,
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
            Pattern::RandomShared => "random-shared",
        }
        .into(),
        a.seed.to_string(),
        id.to_string(),
    ];
    p.cwd = Some("/workspace".into());
    p.stdout = StdioMode::Capture;
    p.stderr = StdioMode::Capture;
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
async fn start(
    a: &Args,
    root: &Path,
    workspace: &Path,
    id: usize,
    checkpoint: Option<&ExecutionCheckpoint>,
) -> anyhow::Result<Worker> {
    let storage = root.join(format!("s{id}"));
    let settings = VmSettings {
        rootfs: Some(a.rootfs.canonicalize()?),
        library_dir: Some(a.firmware.canonicalize()?),
        ram_backing: (checkpoint.is_none() && !matches!(a.mode, Mode::Fresh | Mode::Pool))
            .then(|| root.join(format!("r{id}.ram"))),
        memory_pool: a.pool_socket.clone(),
        ram_compression: a.mode == Mode::Compressed,
        ram_dedup: a.dedup,
        memory_mib: a.memory_mib,
        cpus: a.cpus,
        ..Default::default()
    };
    let (executor, overlay) = if let Some(c) = checkpoint {
        VmExecutor::restore(settings, c.clone(), &storage)?
    } else {
        (
            VmExecutor::new(settings)?,
            OverlayHint {
                lower_dirs: vec![workspace.to_owned()],
                stage_dir: Some(root.join(format!("t{id}"))),
                ..Default::default()
            },
        )
    };
    let upper = protocol_upper(&overlay)?;
    // Copied heartbeat existence is not readiness. Capture its value before run;
    // only guest advancement proves restore validation finished, permitting host
    // protocol writes without changing the authenticated directory metadata early.
    let startup_heartbeat = if checkpoint.is_some() {
        fs::read_to_string(upper.join("heartbeat"))
            .context("read restored startup heartbeat before run")?
            .trim()
            .parse::<u64>()
            .context("invalid copied startup heartbeat")?
    } else {
        0
    };
    let runtime = PVisor::builder()
        .network(network_profile())
        .executors(vec![Arc::new(executor)])
        .storage(storage)
        .overlay(overlay)
        .build();
    let handle = runtime
        .run(spec(
            a,
            workspace,
            if checkpoint.is_some() { 0 } else { id },
        ))
        .await?;
    let cancellation = handle.cancellation();
    Ok(Worker {
        id,
        upper,
        handle: Some(handle),
        waiter: None,
        cancellation,
        startup_heartbeat,
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
    sleep(Duration::from_millis(a.settle_ms.max(100))).await;
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
    if let Some(socket) = &a.pool_socket {
        let mut client = pvisor::ram_backing::ipc::PoolClient::new(
            std::os::unix::net::UnixStream::connect(socket)?,
            Duration::from_secs(5),
        )?;
        let stats = client.stats()?;
        report["pool_observations"].as_array_mut().unwrap().push(
            json!({"phase":name,"encoded_bytes":stats.encoded_bytes,"objects":stats.objects}),
        );
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
    start_time: Instant,
) -> anyhow::Result<()> {
    validate(a)?;
    let workspace = root.join("ws");
    fs::create_dir(&workspace)?;
    let mut checkpoint = None;
    let mut producer_heartbeat = 0;
    if matches!(a.mode, Mode::Baseline | Mode::Ksm) {
        workers.push(start(a, root, &workspace, 0, None).await?);
        let producer = workers.last_mut().unwrap();
        let ack = producer
            .command("producer-ready", "read", 0, &hashes[&(0, 0)])
            .await?;
        check(report, "producer_payload", ack);
        let value = producer
            .handle()?
            .control(OperationKind::RunSuspend {
                request_id: "memory-scale-baseline".into(),
                ram_storage: SnapshotRamStorage::Raw,
            })
            .await?;
        report["producer_suspend_ack"] = serde_json::to_value(value)?;
        let result = producer.reap(root, report).await?;
        let receipt = ExecutionSuspension::from_result(&result)?;
        producer_heartbeat = fs::read_to_string(producer.upper.join("heartbeat"))?
            .trim()
            .parse()?;
        ensure!(
            producer_heartbeat >= producer.heartbeat,
            "producer heartbeat regressed during suspend"
        );
        report["producer_final_heartbeat"] = json!(producer_heartbeat);
        // This is our fresh producer's live file, not the published checkpoint.
        // Reaping must precede unlink so no old producer cache is retained merely
        // by this experiment's explicit backing name during restored measurements.
        fs::remove_file(root.join("r0.ram"))?;
        ensure!(
            receipt.request_id == "memory-scale-baseline",
            "incorrect suspend receipt"
        );
        report["checkpoint"] = serde_json::to_value(&receipt.checkpoint)?;
        check(report, "producer_reaped_before_restore", json!(receipt));
        checkpoint = Some(receipt.checkpoint);
        persist(root, report)?;
    }
    let first = workers.len();
    let mut baseline_inodes = BTreeSet::new();
    for id in 1..=a.vms {
        let mut independent = checkpoint.clone();
        if a.independent_inodes {
            let reference = independent.as_mut().context("missing baseline")?;
            let destination = root.join(format!("b{id}"));
            copy_store(&reference.store, &destination)?;
            reference.store = destination;
            let ram = reference
                .store
                .join("objects")
                .join(&reference.snapshot_id)
                .join("ram.bin");
            let meta = fs::metadata(&ram)?;
            ensure!(
                baseline_inodes.insert((meta.dev(), meta.ino())),
                "independent RAM inode was shared"
            );
            let source = checkpoint
                .as_ref()
                .unwrap()
                .store
                .join("objects")
                .join(&reference.snapshot_id)
                .join("ram.bin");
            let original = fs::metadata(&source)?;
            ensure!(
                (meta.dev(), meta.ino()) != (original.dev(), original.ino()),
                "copy reused source RAM inode"
            );
            ensure!(
                pvisor::environment_snapshot::file_hash(&ram)?
                    == pvisor::environment_snapshot::file_hash(&source)?,
                "independent baseline RAM bytes differ"
            );
            check(
                report,
                "independent_ram_inode",
                json!({"instance":id,"device":meta.dev(),"inode":meta.ino(),"bytes":meta.len()}),
            );
        }
        workers.push(start(a, root, &workspace, id, independent.as_ref()).await?);
    }
    let group = &mut workers[first..];
    for w in group.iter_mut() {
        if checkpoint.is_some() {
            w.heartbeat = producer_heartbeat;
            let ack = w
                .command(&format!("original-{}", w.id), "read", 0, &hashes[&(0, 0)])
                .await?;
            check(report, "restored_original_and_heartbeat", ack);
        }
        let ack = w
            .command(
                &format!("prepare-{}", w.id),
                "prepare",
                0,
                &hashes[&(w.id, 0)],
            )
            .await?;
        check(report, "ready_full_digest", ack);
    }
    // A single bounded scan window for the complete group; never writes KSM sysfs.
    let scan_start = accounting()?;
    sleep(Duration::from_secs(a.ksm_wait_seconds)).await;
    report["ksm_scan_window"] = json!({"seconds":a.ksm_wait_seconds,"before":scan_start,"after":accounting()?,"deadline_is_not_product_failure":true});
    phase(a, group, "ready", false, report, start_time).await?;
    if a.mode == Mode::Ksm {
        for w in group.iter_mut() {
            let ack = w
                .command(
                    &format!("duplicate-{}", w.id),
                    "duplicate",
                    0,
                    &hashes[&(w.id, 0)],
                )
                .await?;
            check(report, "dynamic_private_full_digest", ack);
        }
        phase(
            a,
            group,
            "dynamic_private_before_wait",
            false,
            report,
            start_time,
        )
        .await?;
        let scan_start = accounting()?;
        sleep(Duration::from_secs(a.ksm_wait_seconds)).await;
        report["dynamic_ksm_scan_window"] = json!({"seconds":a.ksm_wait_seconds,"before":scan_start,"after":accounting()?,"deadline_is_not_product_failure":true,"merging_required":false});
        for w in group.iter_mut() {
            let ack = w
                .command(
                    &format!("dynamic-readback-{}", w.id),
                    "read",
                    0,
                    &hashes[&(w.id, 0)],
                )
                .await?;
            check(report, "dynamic_private_after_wait_full_digest", ack);
        }
        phase(
            a,
            group,
            "dynamic_private_after_wait",
            false,
            report,
            start_time,
        )
        .await?;
    }
    if matches!(a.mode, Mode::Baseline | Mode::Ksm) {
        for w in group.iter_mut() {
            let error = w
                .handle()?
                .offload(None)
                .await
                .err()
                .context("restored private RAM offload was incorrectly accepted")?;
            ensure!(
                format!("{error:#}").contains("restored private COW RAM"),
                "offload rejected for an unrelated reason: {error:#}"
            );
            let ack = w
                .command(
                    &format!("rejected-offload-{}", w.id),
                    "read",
                    0,
                    &hashes[&(w.id, 0)],
                )
                .await?;
            check(
                report,
                "private_offload_rejected_and_healthy",
                json!({"instance":w.id,"error":format!("{error:#}"),"ack":ack}),
            );
        }
    }
    if matches!(
        a.mode,
        Mode::Fresh | Mode::Pool | Mode::Baseline | Mode::Ksm
    ) {
        for percent in [25, 100] {
            // Verify untouched peers while each preceding peer is already dirty.
            for index in 0..group.len() {
                let w = &mut group[index];
                let ack = w
                    .command(
                        &format!("cow{percent}-{}", w.id),
                        "mutate",
                        percent,
                        &hashes[&(w.id, percent)],
                    )
                    .await?;
                check(report, "cow_full_digest", ack);
                for peer in &mut group[index + 1..] {
                    let prior = if percent == 25 { 0 } else { 25 };
                    let ack = peer
                        .command(
                            &format!("isolation-{percent}-{index}-{}", peer.id),
                            "read",
                            prior,
                            &hashes[&(peer.id, prior)],
                        )
                        .await?;
                    check(report, "peer_write_isolation", ack);
                }
            }
            phase(
                a,
                group,
                &format!("cow{percent}"),
                false,
                report,
                start_time,
            )
            .await?;
        }
    } else {
        for cycle in 0..2 {
            for w in group.iter() {
                let t = Instant::now();
                let memory = w.handle()?.offload(None).await?;
                ensure!(
                    memory.backed_bytes >= u64::from(a.memory_mib) * 1024 * 1024,
                    "incomplete RAM offload"
                );
                ensure!(
                    w.handle()?.status().state == RunState::Suspended,
                    "offload did not suspend VM {}",
                    w.id
                );
                check(
                    report,
                    "offload_receipt",
                    json!({"cycle":cycle,"instance":w.id,"elapsed_ms":t.elapsed().as_millis(),"memory":memory}),
                );
            }
            phase(
                a,
                group,
                &format!("offloaded{cycle}"),
                true,
                report,
                start_time,
            )
            .await?;
            for w in group.iter_mut() {
                let t = Instant::now();
                let parked = w.heartbeat;
                w.handle()?.resume_vm().await?;
                let ack = w
                    .command(
                        &format!("readback-{cycle}-{}", w.id),
                        "read",
                        0,
                        &hashes[&(w.id, 0)],
                    )
                    .await?;
                let advanced = w.wait_for_progress(parked).await?;
                check(
                    report,
                    "offload_resume_full_digest_and_heartbeat",
                    json!({"cycle":cycle,"elapsed_ms":t.elapsed().as_millis(),"ack":ack,"parked_heartbeat":parked,"resumed_heartbeat":advanced}),
                );
            }
            phase(
                a,
                group,
                &format!("resumed{cycle}"),
                false,
                report,
                start_time,
            )
            .await?;
        }
    }
    group[0].cancellation.cancel();
    let result = group[0].reap(root, report).await?;
    ensure!(
        result.state == RunState::Cancelled,
        "cancel did not reap VM: {:?}",
        result.state
    );
    check(
        report,
        "one_vm_cancelled_and_reaped",
        json!({"instance":group[0].id}),
    );
    let survivors = &mut group[1..];
    let percent = if matches!(
        a.mode,
        Mode::Fresh | Mode::Pool | Mode::Baseline | Mode::Ksm
    ) {
        100
    } else {
        0
    };
    for w in survivors.iter_mut() {
        let ack = w
            .command(
                &format!("survivor-{}", w.id),
                "read",
                percent,
                &hashes[&(w.id, percent)],
            )
            .await?;
        check(report, "survivor_full_digest", ack);
    }
    phase(a, survivors, "after_exit", false, report, start_time).await?;
    for w in survivors.iter_mut() {
        let ack = w
            .command(
                &format!("exit-{}", w.id),
                "exit",
                percent,
                &hashes[&(w.id, percent)],
            )
            .await?;
        check(report, "orderly_exit_digest", ack);
        let result = w.reap(root, report).await?;
        ensure!(
            result.state == RunState::Completed && result.exit_code == Some(0),
            "guest did not terminate orderly"
        );
    }
    Ok(())
}
fn copy_store(source: &Path, destination: &Path) -> anyhow::Result<()> {
    ensure!(!destination.try_exists()?, "independent store must be new");
    // Preserve snapshot metadata and internal reference-marker hard links.
    // Data/RAM inodes must be independent of the original and other copies.
    let status = std::process::Command::new("/usr/bin/cp")
        .args([
            "--archive",
            "--reflink=never",
            "--no-target-directory",
            "--",
        ])
        .arg(source)
        .arg(destination)
        .status()?;
    ensure!(status.success(), "independent snapshot store copy failed");
    Ok(())
}

async fn run(mut a: Args) -> anyhow::Result<()> {
    let start = Instant::now();
    fs::create_dir(&a.output).context("--output must be a NEW directory")?;
    OWNS_OUTPUT.store(true, std::sync::atomic::Ordering::Release);
    let root = a.output.canonicalize()?;
    let mut report = json!({"schema":"pvisor-memory-scale/v1","conditions":a,"profile":{"memory_mib":a.memory_mib,"cpus":a.cpus,"payload_bytes":BYTES,"page_bytes":PAGE,"max_live_vms":4,"deadline_seconds":180,"overlaynet_mode":"off","network_policy":"no-network"},"correctness":"failed","phases":[],"checks":[],"guests":[],"pool_observations":[],"gaps":["independent-inode controls copy identical sealed bytes; independent native captures remain unsupported", "KSM scan results are observations, not guaranteed merging", "random-unique restored preparation dirties all payload pages before ready; only repeated/random-shared ready are unchanged shared payloads", "large_file_fds and smaps expose identities/advice when proc permissions permit; missing reads are explicit", "parent owns group budgets, randomized pairing, repetitions and continuous resource accounting"]});
    // Persist failure evidence even if validation, startup or capture fails.
    persist(&root, &report)?;
    let mut workers = Vec::new();
    let mut pool_child: Option<tokio::process::Child> = None;
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
        if a.mode == Mode::Pool {
            use std::os::unix::fs::DirBuilderExt;
            let daemon = a.pool_daemon.as_ref().context("pool arm requires --pool-daemon")?.canonicalize()?;
            let directory = root.join("pool");
            fs::DirBuilder::new().mode(0o700).create(&directory)?;
            fs::write(directory.join("config.json"), serde_json::to_vec(&json!({"max_bytes":536870912,"max_objects":32768,"max_connections":32,"max_references":32768}))?)?;
            let log = fs::File::create(root.join("pool.log"))?;
            pool_child = Some(tokio::process::Command::new(&daemon).args(["memory-pool", "--directory"]).arg(&directory)
                .stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log).kill_on_drop(true).spawn()?);
            let socket = directory.join("pool.sock");
            for _ in 0..100 {
                ensure!(pool_child.as_mut().unwrap().try_wait()?.is_none(), "daemon pool exited during startup");
                if socket.exists() { break; }
                sleep(Duration::from_millis(20)).await;
            }
            ensure!(socket.exists(), "daemon pool readiness timed out");
            a.pool_socket = Some(socket);
            report["pool_daemon_sha256"] = json!(pvisor::environment_snapshot::file_hash(&daemon)?);
        }
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
        eprintln!("memory-scale: {e:#}");
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
            if w.waiter.is_some()
                && let Err(e) = w.reap(&root, &mut report).await
            {
                errors.push(format!("VM {}: {e:#}", w.id));
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
    if let Some(mut child) = pool_child {
        child.start_kill()?;
        let status = timeout(Duration::from_secs(5), child.wait()).await??;
        report["pool_cleanup"] = json!({"reaped":true,"status":status.to_string()});
    }
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
        json!({"schema":"pvisor-memory-scale/v1","raw_json":root.join("raw.json"),"correctness":report["correctness"]})
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
        eprintln!("memory-scale: hard deadline watchdog (parent must reap cgroup)");
        // Filesystem I/O must not prevent the watchdog itself from exiting.
        if OWNS_OUTPUT.load(std::sync::atomic::Ordering::Acquire) {
            std::thread::spawn(move || {
                if let Ok(bytes) = fs::read(watchdog_output.join("raw.json"))
                    && let Ok(mut report) = serde_json::from_slice::<Value>(&bytes)
                    && report["schema"] == "pvisor-memory-scale/v1"
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
    fn args(extra: &[&str]) -> Result<Args, clap::Error> {
        let mut argv = vec![
            "vm_memory_scale",
            "--rootfs",
            "/r",
            "--firmware",
            "/f",
            "--output",
            "/o",
            "--vms",
            "4",
            "--mode",
            "baseline",
            "--pattern",
            "repeated",
        ];
        argv.extend_from_slice(extra);
        Args::try_parse_from(argv)
    }
    #[test]
    fn independent_store_copy_preserves_bytes_without_sharing_inodes() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("ram.bin"), b"sealed RAM").unwrap();
        std::os::unix::fs::symlink("ram.bin", source.join("alias")).unwrap();
        let target = root.path().join("target");
        copy_store(&source, &target).unwrap();
        assert_eq!(fs::read(target.join("ram.bin")).unwrap(), b"sealed RAM");
        assert_eq!(
            fs::read_link(target.join("alias")).unwrap(),
            PathBuf::from("ram.bin")
        );
        assert_ne!(
            fs::metadata(source.join("ram.bin")).unwrap().ino(),
            fs::metadata(target.join("ram.bin")).unwrap().ino()
        );
        assert!(
            copy_store(&source, &target).is_err(),
            "must not overwrite an existing store"
        );
    }

    #[test]
    fn cli_limits() {
        assert!(parse_vms("3").is_err());
        assert!(parse_vms("5").is_err());
        assert!(args(&["--ksm-wait-seconds", "61"]).is_err());
        assert!(args(&["--settle-ms", "5001"]).is_err());
        let a = args(&[]).unwrap();
        assert_eq!(a.settle_ms, 500);
        assert_eq!(a.ksm_wait_seconds, 2);
        assert!(!a.dedup);
        assert!(!a.private_baselines);
        assert!(validate(&args(&["--private-baselines", "true"]).unwrap()).is_err());
        let mut a = args(&["--dedup", "true"]).unwrap();
        assert!(a.dedup);
        a.mode = Mode::Compressed;
        assert!(validate(&a).is_err());
        a.mode = Mode::from_str("ksm", false).unwrap();
        assert_eq!(serde_json::to_value(a.mode).unwrap(), "ksm");
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            for dedup in [false, true] {
                a.dedup = dedup;
                assert!(validate(&a).is_ok());
            }
        }
    }
    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn profile_is_offline_for_native_checkpoints() {
        let network = network_profile();
        assert_eq!(network.mode, pvisor::OverlayNetMode::Off);
        assert_eq!(network.network.mode, pvisor_core::NetworkMode::NoNetwork);
        assert!(network.network.allowed_hosts.is_empty());
        assert!(network.network.rules.is_empty());
        let root = tempfile::tempdir().unwrap();
        let executor = VmExecutor::new(VmSettings {
            rootfs: Some(root.path().to_owned()),
            memory_mib: 256,
            cpus: 1,
            ..Default::default()
        })
        .unwrap();
        let runtime = PVisor::builder()
            .network(network)
            .executors(vec![Arc::new(executor)])
            .storage(root.path().join("records"))
            .overlay(OverlayHint {
                lower_dirs: vec![root.path().to_owned()],
                stage_dir: Some(root.path().join("stage")),
                ..Default::default()
            })
            .build();
        // Public admission only: no firmware load, VM startup, mount or measurement.
        let a = args(&[]).unwrap();
        let operation = runtime.resolve_operation(spec(&a, root.path(), 0)).unwrap();
        assert!(matches!(
            operation.placements.first(),
            Some(pvisor::Placement::Vm { .. })
        ));
        let network = operation
            .rules
            .iter()
            .find(|r| r.id == "net.aggregate")
            .unwrap();
        assert_eq!(network.action, "deny");
    }
    #[test]
    fn protocol_upper_respects_explicit_restore_projection() {
        let root = tempfile::tempdir().unwrap();
        let stage = root.path().join("stage");
        let relocated = root.path().join("relocated-upper");
        fs::create_dir_all(&relocated).unwrap();
        fs::write(relocated.join("heartbeat"), "1537").unwrap();
        let mut overlay = OverlayHint {
            stage_dir: Some(stage.clone()),
            upper_dir: Some(relocated.clone()),
            ..Default::default()
        };
        let upper = protocol_upper(&overlay).unwrap();
        assert_eq!(upper, relocated);
        assert_eq!(fs::read_to_string(upper.join("heartbeat")).unwrap(), "1537");
        assert!(!stage.join("upper").exists());
        overlay.upper_dir = None;
        assert_eq!(protocol_upper(&overlay).unwrap(), stage.join("upper"));
        overlay.stage_dir = None;
        assert!(protocol_upper(&overlay).is_err());
    }
    #[test]
    fn copied_heartbeat_is_not_restore_readiness() {
        let upper = tempfile::tempdir().unwrap();
        assert!(!heartbeat_advanced(upper.path(), 0));
        fs::write(upper.path().join("heartbeat"), "2").unwrap();
        // Model the immutable pre-run capture, not a read racing guest startup.
        let copied = fs::read_to_string(upper.path().join("heartbeat"))
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert!(upper.path().join("heartbeat").is_file());
        assert!(!heartbeat_advanced(upper.path(), copied));
        for value in ["", "invalid", "1", "2"] {
            fs::write(upper.path().join("heartbeat"), value).unwrap();
            assert!(!heartbeat_advanced(upper.path(), copied));
        }
        fs::write(upper.path().join("heartbeat.tmp"), "3").unwrap();
        fs::rename(
            upper.path().join("heartbeat.tmp"),
            upper.path().join("heartbeat"),
        )
        .unwrap();
        assert_eq!(copied, 2);
        assert!(heartbeat_advanced(upper.path(), copied));
        assert!(heartbeat_advanced(upper.path(), 0));
    }
    #[test]
    fn command_ack_can_complete_in_sampled_heartbeat_iteration() {
        let mut ack = json!({"token":"readback-1-1","op":"read","percent":0,"heartbeat":775});
        // The new token's work occurs after heartbeat publication but before the
        // next tick: equality is valid, whereas restore/resume progress stays strict.
        assert_eq!(ack_heartbeat(&ack, 775).unwrap(), 775);
        ack["heartbeat"] = json!(774);
        assert!(ack_heartbeat(&ack, 775).is_err());
        ack["heartbeat"] = json!(776);
        assert_eq!(ack_heartbeat(&ack, 775).unwrap(), 776);
        ack["heartbeat"] = Value::Null;
        assert!(ack_heartbeat(&ack, 775).is_err());
    }
    #[test]
    fn page_contract() {
        let repeated = page(1, 0, 0, false);
        assert_eq!(&repeated[..256], &(0..=255).collect::<Vec<u8>>());
        let p = page(0, 0, 0, true);
        assert_eq!(&p[..8], &1442695040888963407u64.to_le_bytes());
        assert_ne!(page(1, 1, 0, true), page(1, 2, 0, true));
        assert_ne!(page(1, 1, 0, true), page(1, 1, 1, true));
        assert_ne!(page(1, 1, 0, true), page(1, 0, 1, true));
    }
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
        for pattern in ["repeated", "random-shared", "random-unique"] {
            let dir = tempfile::tempdir().unwrap();
            let guest = GUEST.replace("64*1024*1024", "4*4096");
            let mut child = Child(
                std::process::Command::new("python3")
                    .args(["-c", &guest, pattern, "18446744073709551615", "0"])
                    .current_dir(dir.path())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
            let mut heartbeat = 0;
            for (token, op, percent) in [
                ("original", "read", 0),
                ("prepared", "prepare", 0),
                ("duplicated", "duplicate", 0),
                ("duplicate-reread", "read", 0),
                ("quarter", "mutate", 25),
                ("whole", "mutate", 100),
                ("reread", "read", 100),
                ("exit", "exit", 100),
            ] {
                let req = json!({"token":token,"op":op,"instance":1,"percent":percent});
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
                    } else if pattern == "random-unique" && token != "original" {
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
