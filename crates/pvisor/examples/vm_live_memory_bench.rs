//! B-VM-MEMORY helper: fresh-VM Linux SDK offload, complete cgroup accounting.
//! Use one sample and no warmups per invocation; live_vm_memory.py owns pairing.
use anyhow::{Context, ensure};
use clap::{Parser, ValueEnum};
use pvisor::{OverlayHint, PVisor, VmExecutor, VmSettings};
use pvisor_core::{RunInvocation, RunSpec, RunState, StdioMode};
use serde_json::{Value, json};
use std::{os::unix::fs::MetadataExt, path::PathBuf, sync::Arc, time::Instant};
use tokio::time::{Duration, sleep};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "repeated")]
    pattern: String,
    #[arg(long, default_value_t = 20261006)]
    seed: u64,
    #[arg(long, default_value_t = 2000)]
    settle_ms: u64,
    #[arg(long)]
    rootfs: PathBuf,
    #[arg(long)]
    firmware: PathBuf,
    /// New directory on disk, outside the guest workspace (avoid tmpfs).
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 256)]
    memory: u32,
    #[arg(long, default_value_t = 2)]
    cpus: u8,
    #[arg(long, default_value_t = 30)]
    samples: usize,
    #[arg(long, default_value_t = 2)]
    warmups: usize,
    #[arg(long)]
    compressed: bool,
    #[arg(long, default_value_t = 10)]
    long_pause_seconds: u64,
    /// Race controls, abandon a caller, reject an occupied path and check recovery.
    #[arg(long)]
    stress: bool,
    /// End by cancelling the VM in this state instead of releasing the guest.
    #[arg(long, value_enum)]
    cancel_while: Option<CancelState>,
}

#[derive(Clone, Copy, Debug, ValueEnum, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum CancelState {
    Running,
    Paused,
    Offloaded,
}

const GUEST: &str = r#"
import hashlib, json, pathlib, time
import random, sys
pattern, seed = sys.argv[1], int(sys.argv[2])
data = bytearray(random.Random(seed).randbytes(64 * 1024 * 1024) if pattern == "random" else bytes(range(256)) * (64 * 1024 * 1024 // 256))
expected = hashlib.sha256(data).hexdigest()
scratch = bytearray(1024 * 1024)
n, last = 0, ''
while not pathlib.Path('release').exists():
    assert scratch == n.to_bytes(8, 'little') * (len(scratch) // 8)
    n += 1
    scratch[:] = n.to_bytes(8, 'little') * (len(scratch) // 8)
    pathlib.Path('heartbeat.tmp').write_text(str(n))
    pathlib.Path('heartbeat.tmp').replace('heartbeat')
    if pathlib.Path('request').exists():
        request = pathlib.Path('request').read_text()
        if request != last:
            start = time.perf_counter_ns()
            digest = hashlib.sha256(data).hexdigest()
            elapsed = time.perf_counter_ns() - start
            assert digest == expected, 'immutable RAM corrupted'
            pathlib.Path('ack.tmp').write_text(json.dumps(dict(request=request, read_ms=elapsed/1e6)))
            pathlib.Path('ack.tmp').replace('ack')
            last = request
    time.sleep(0.01)
assert hashlib.sha256(data).hexdigest() == expected
assert scratch == n.to_bytes(8, 'little') * (len(scratch) // 8)
print('guest-memory-ok', flush=True)
"#;

fn accounting() -> anyhow::Result<Value> {
    let membership = std::fs::read_to_string("/proc/self/cgroup")?;
    let relative = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .context("requires Linux unified cgroup")?;
    let root = std::path::Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
    let map = |name: &str| -> anyhow::Result<Value> {
        let raw = std::fs::read_to_string(root.join(name))?;
        let mut values = serde_json::Map::new();
        for line in raw.lines() {
            let mut parts = line.split_whitespace();
            let key = parts.next().context("counter key")?;
            let number: u64 = parts.next().context("counter value")?.parse()?;
            values.insert(key.into(), json!(number));
        }
        Ok(Value::Object(values))
    };
    Ok(
        json!({"cgroup":root, "current_bytes":std::fs::read_to_string(root.join("memory.current"))?.trim().parse::<u64>()?,
        "peak_bytes":std::fs::read_to_string(root.join("memory.peak"))?.trim().parse::<u64>()?,
        "stat":map("memory.stat")?, "cpu":map("cpu.stat")?, "events":map("memory.events")?}),
    )
}

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn main() -> anyhow::Result<()> {
    if pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    let args = Args::parse();
    tokio::runtime::Runtime::new()?.block_on(run(args))
}

async fn changed(
    handle: &pvisor::RunHandle,
    path: &std::path::Path,
    previous: &[u8],
) -> anyhow::Result<Vec<u8>> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            ensure!(!handle.status().state.is_terminal(), "guest exited early");
            if let Ok(bytes) = std::fs::read(path)
                && !bytes.is_empty()
                && bytes != previous
            {
                return Ok(bytes);
            }
            sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .context("guest response timeout")?
}

async fn read_check(
    handle: &pvisor::RunHandle,
    upper: &std::path::Path,
    id: &str,
) -> anyhow::Result<f64> {
    std::fs::write(upper.join("request"), id)?;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            ensure!(
                !handle.status().state.is_terminal(),
                "guest exited during read"
            );
            if let Ok(bytes) = std::fs::read(upper.join("ack"))
                && let Ok(ack) = serde_json::from_slice::<Value>(&bytes)
                && ack["request"] == id
            {
                return ack["read_ms"].as_f64().context("missing guest read timing");
            }
            sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .context("guest read timeout")?
}

async fn run(args: Args) -> anyhow::Result<()> {
    ensure!(
        args.samples == 1
            && args.warmups == 0
            && matches!(args.pattern.as_str(), "repeated" | "random"),
        "invalid sample count"
    );
    std::fs::create_dir(&args.output).context("output must not exist")?;
    let root = args.output.canonicalize()?;
    let ws = root.join("workspace");
    std::fs::create_dir(&ws)?;
    let stage = root.join("stage");
    let upper = stage.join("upper");
    let backing = root.join("live.ram");
    let runtime = PVisor::builder()
        .executors(vec![Arc::new(VmExecutor::new(VmSettings {
            rootfs: Some(args.rootfs.canonicalize()?),
            library_dir: Some(args.firmware.canonicalize()?),
            ram_backing: Some(backing.clone()),
            ram_compression: args.compressed,
            memory_mib: args.memory,
            cpus: u16::from(args.cpus),
            ..Default::default()
        })?)])
        .storage(root.join("records"))
        .overlay(OverlayHint {
            lower_dirs: vec![ws.clone()],
            stage_dir: Some(stage),
            ..Default::default()
        })
        .build();
    let mut spec = RunSpec::process("lifecycle-benchmark", "integrity", "/usr/bin/python3");
    spec.runtime.timeout_ms = Some(900_000);
    let RunInvocation::Process(p) = &mut spec.invocation;
    p.args = vec![
        "-c".into(),
        GUEST.into(),
        args.pattern.clone(),
        args.seed.to_string(),
    ];
    p.cwd = Some("/workspace".into());
    p.stdout = StdioMode::Capture;
    p.stderr = StdioMode::Capture;
    spec.metadata.insert("pvisor.workspace".into(), json!(ws));
    spec.metadata
        .insert("pvisor.vm.overlay_target".into(), json!("/workspace"));
    spec.metadata
        .insert("pvisor.vm.guest_cwd".into(), json!("/workspace"));
    let before = accounting()?;
    let handle = runtime.run(spec).await?;
    let check = async {
        let heartbeat = upper.join("heartbeat");
        changed(&handle, &heartbeat, &[]).await?;
        let mut rows = Vec::new();
        for cycle in 0..args.warmups + args.samples {
            if args.stress {
                let occupied = root.join("occupied.ram");
                std::fs::write(&occupied, b"must survive rejected offload")?;
                ensure!(handle.offload(Some(occupied.clone())).await.is_err(), "occupied destination accepted");
                ensure!(std::fs::read(&occupied)? == b"must survive rejected offload", "rejection overwrote destination");
                let (first, second, third) = tokio::join!(handle.pause(), handle.resume(), handle.pause());
                first?; second?; third?;
                // Pin the resulting state after an intentionally unordered race.
                handle.resume().await?;
                let old = std::fs::read(&heartbeat)?;
                changed(&handle, &heartbeat, &old).await?;
                // Dropping a waiting caller must not misalign the control reply
                // stream. A later resume must consume its own acknowledgement.
                let mut abandoned = Box::pin(handle.offload(None));
                tokio::select! {
                    result = &mut abandoned => { result?; }
                    _ = sleep(Duration::from_millis(1)) => {}
                }
                drop(abandoned);
                handle.resume().await?;
                read_check(&handle, &upper, &format!("after-abandoned-{cycle}")).await?;
            }
            let baseline_read = read_check(&handle, &upper, &format!("before-{cycle}")).await?;
            sleep(Duration::from_millis(args.settle_ms)).await;
            let active = accounting()?;
            let start = Instant::now();
            handle.pause().await?;
            let pause_ms = ms(start);
            ensure!(handle.status().state == RunState::Suspended, "pause state");
            handle.pause().await?;
            sleep(Duration::from_millis(100)).await;
            let parked = std::fs::read(&heartbeat)?;
            sleep(Duration::from_millis(if cycle == 0 { args.long_pause_seconds * 1000 } else { 100 })).await;
            ensure!(std::fs::read(&heartbeat)? == parked, "paused guest progressed");
            let start = Instant::now();
            handle.resume().await?;
            let resume_ms = ms(start);
            handle.resume().await?;
            changed(&handle, &heartbeat, &parked).await?;
            let start = Instant::now();
            let memory = handle.offload(None).await?;
            let offload_ms = ms(start);
            ensure!(memory.backed_bytes >= u64::from(args.memory) * 1024 * 1024, "missing RAM range");
            ensure!(handle.status().state == RunState::Suspended, "offload state");
            sleep(Duration::from_millis(100)).await;
            let parked = std::fs::read(&heartbeat)?;
            sleep(Duration::from_millis(100)).await;
            ensure!(std::fs::read(&heartbeat)? == parked, "offloaded guest progressed");
            sleep(Duration::from_millis(args.settle_ms)).await;
            let offloaded = accounting()?;
            let allocated_bytes = if args.compressed {
                pvisor::ram_backing::CompressedRam::open(std::fs::File::open(&backing)?)?.allocated_bytes()?
            } else { std::fs::metadata(&backing)?.blocks() * 512 };
            let start = Instant::now();
            handle.resume().await?;
            let offload_resume_ms = ms(start);
            changed(&handle, &heartbeat, &parked).await?;
            let wake_heartbeat_ms = ms(start);
            let read_ms = read_check(&handle, &upper, &format!("after-{cycle}")).await?;
            let restored = accounting()?;
            rows.push(json!({"active":active, "offloaded":offloaded, "restored":restored, "cycle":cycle, "warmup":cycle < args.warmups,
                "pause_ms":pause_ms, "resume_ms":resume_ms, "offload_ms":offload_ms,
                "offload_resume_ms":offload_resume_ms, "wake_heartbeat_ms":wake_heartbeat_ms,
                "baseline_read_ms":baseline_read, "restored_read_ms":read_ms,
                "backed_bytes":memory.backed_bytes, "resident_before_bytes":memory.resident_before_bytes,
                "resident_after_bytes":memory.resident_after_bytes, "allocated_bytes":allocated_bytes}));
        }
        if let Some(state) = args.cancel_while {
            match state {
                CancelState::Running => {},
                CancelState::Paused => handle.pause().await?,
                CancelState::Offloaded => { handle.offload(None).await?; },
            }
            handle.cancel();
        } else { std::fs::write(upper.join("release"), "go")?; }
        Ok::<_, anyhow::Error>(rows)
    }.await;
    if check.is_err() {
        handle.cancel();
    }
    let result = tokio::time::timeout(Duration::from_secs(30), handle.wait())
        .await
        .context("VM did not terminate after release/cancellation")??;
    std::fs::write(
        root.join("guest.stderr"),
        result.output.stderr.as_deref().unwrap_or_default(),
    )?;
    let rows = check?;
    if args.cancel_while.is_some() {
        ensure!(
            result.state == RunState::Cancelled,
            "VM cancellation failed: {result:?}"
        );
    } else {
        ensure!(
            result.state == RunState::Completed && result.exit_code == Some(0),
            "guest failed: {result:?}"
        );
        ensure!(
            result
                .output
                .stdout
                .as_deref()
                .unwrap_or_default()
                .contains("guest-memory-ok"),
            "missing integrity check"
        );
    }
    let report = json!({"schema":"pvisor-live-offload/v1", "before":before, "pattern":args.pattern, "seed":args.seed, "settle_ms":args.settle_ms, "memory_mib":args.memory,
        "cpus":args.cpus, "compressed":args.compressed, "samples":args.samples,
        "warmups":args.warmups, "long_pause_seconds":args.long_pause_seconds,
        "stress":args.stress, "cancel_while":args.cancel_while,
        "guest_data_bytes":64*1024*1024, "correctness":"passed", "rows":rows});
    std::fs::write(root.join("raw.json"), serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}
