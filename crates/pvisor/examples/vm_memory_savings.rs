//! B-VM-MEMORY: default reclaim, live compression and idle offload tradeoffs.
//! memory_savings.py owns fresh groups, pairing, provenance and publication.
use anyhow::{Context, ensure};
use clap::{Parser, ValueEnum};
use pvisor::{OverlayHint, PVisor, VmExecutor, VmSettings};
use pvisor_core::{RunInvocation, RunSpec, StdioMode};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use tokio::time::{Duration, sleep, timeout};

const BYTES: usize = 64 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Mode {
    Default,
    Cold,
    Pause,
    Raw,
    Compressed,
    Release,
}
#[derive(Parser)]
struct Args {
    #[arg(long)]
    rootfs: PathBuf,
    #[arg(long)]
    firmware: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, value_enum)]
    mode: Mode,
    #[arg(long, default_value = "repeated")]
    pattern: String,
    #[arg(long, default_value_t = 60)]
    wait: u64,
}
const GUEST: &str = r#"
import hashlib, json, pathlib, time, struct, sys, os, gc
pattern = sys.argv[1]
SIZE, PAGE, MASK = 64*1024*1024, 4096, (1<<64)-1
def make():
    data = bytearray(SIZE)
    for p in range(SIZE//PAGE):
        if pattern == 'random' or (pattern == 'mixed' and p < 4096):
            x = 42 ^ ((p*0x9e3779b97f4a7c15)&MASK)
            words = []
            for _ in range(512):
                x = (x*6364136223846793005+1442695040888963407)&MASK
                words.append(x)
            page = struct.pack('<512Q', *words)
        elif pattern == 'compressible':
            page = struct.pack('<Q', p)*512
        else: page = bytes(range(256))*16
        data[p*PAGE:(p+1)*PAGE] = page
    return data
data = make()
last, n = '', 0
scratch = bytearray(4096)
def atomic(name, value):
    pathlib.Path(name+'.tmp').write_text(value)
    pathlib.Path(name+'.tmp').replace(name)
while True:
    assert scratch == n.to_bytes(8,'little')*512
    n += 1
    scratch[:] = n.to_bytes(8,'little')*512
    atomic('heartbeat', str(n))
    if pattern == 'mixed' and data:
        hashlib.sha256(memoryview(data)[:16*1024*1024]).digest()
    try: req = json.loads(pathlib.Path('request').read_text())
    except (FileNotFoundError, ValueError): req = None
    if req and req['token'] != last:
        start = time.perf_counter_ns()
        if req['op'] == 'free':
            data = bytearray(); gc.collect()
        elif req['op'] == 'refill': data = make()
        elif req['op'] not in ('read','exit'): raise RuntimeError('unknown operation')
        digest = hashlib.sha256(data).hexdigest()
        size = len(data)
        if req['op'] in ('read','refill'):
            with open('tool-output','wb') as f:
                assert f.write(data) == size
                f.flush(); os.fsync(f.fileno())
            with open('tool-output','rb') as f:
                for off in range(0,size,65536):
                    assert f.read(65536) == memoryview(data)[off:off+65536]
                assert f.read(1) == b''
            os.unlink('tool-output')
        atomic('ack',json.dumps(dict(token=req['token'],op=req['op'],digest=digest,
            bytes=size,heartbeat=n,task_ms=(time.perf_counter_ns()-start)/1e6)))
        last = req['token']
        if req['op'] == 'exit':
            print('guest-memory-ok '+digest,flush=True)
            break
    time.sleep(0.05)
"#;
fn expected(pattern: &str) -> String {
    let mut digest = Sha256::new();
    for p in 0..BYTES / 4096 {
        let mut page = [0u8; 4096];
        if pattern == "random" || (pattern == "mixed" && p < 4096) {
            let mut x = 42 ^ (p as u64).wrapping_mul(0x9e3779b97f4a7c15);
            for word in page.chunks_exact_mut(8) {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                word.copy_from_slice(&x.to_le_bytes());
            }
        } else if pattern == "compressible" {
            for word in page.chunks_exact_mut(8) {
                word.copy_from_slice(&(p as u64).to_le_bytes());
            }
        } else {
            for (i, b) in page.iter_mut().enumerate() {
                *b = (i % 256) as u8;
            }
        }
        digest.update(page);
    }
    format!("{:x}", digest.finalize())
}
fn group() -> anyhow::Result<PathBuf> {
    let membership = fs::read_to_string("/proc/self/cgroup")?;
    Ok(Path::new("/sys/fs/cgroup").join(
        membership
            .lines()
            .find_map(|l| l.strip_prefix("0::"))
            .context("cgroup v2 required")?
            .trim_start_matches('/'),
    ))
}
fn memory() -> anyhow::Result<Value> {
    let root = group()?;
    let map = |name: &str| -> anyhow::Result<Value> {
        let mut values = serde_json::Map::new();
        for line in fs::read_to_string(root.join(name))?.lines() {
            let mut words = line.split_whitespace();
            values.insert(
                words.next().context("counter key")?.into(),
                json!(words.next().context("counter value")?.parse::<u64>()?),
            );
        }
        Ok(values.into())
    };
    Ok(
        json!({"cgroup":root,"current":fs::read_to_string(root.join("memory.current"))?.trim().parse::<u64>()?,
        "peak":fs::read_to_string(root.join("memory.peak"))?.trim().parse::<u64>()?,
        "stat":map("memory.stat")?,"cpu":map("cpu.stat")?,"events":map("memory.events")?}),
    )
}
fn persist(root: &Path, report: &Value) -> anyhow::Result<()> {
    fs::write(
        root.join("raw.json.tmp"),
        serde_json::to_vec_pretty(report)?,
    )?;
    fs::rename(root.join("raw.json.tmp"), root.join("raw.json"))?;
    Ok(())
}
fn phase(root: &Path, report: &mut Value, name: &str, start: Instant) -> anyhow::Result<()> {
    report["phases"].as_array_mut().unwrap().push(
        json!({"name":name,"elapsed_ms":start.elapsed().as_secs_f64()*1000.,"memory":memory()?}),
    );
    persist(root, report)
}
async fn command(
    upper: &Path,
    token: &str,
    op: &str,
    digest: &str,
    bytes: usize,
) -> anyhow::Result<Value> {
    fs::write(
        upper.join("request.tmp"),
        serde_json::to_vec(&json!({"token":token,"op":op}))?,
    )?;
    fs::rename(upper.join("request.tmp"), upper.join("request"))?;
    timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(text) = fs::read_to_string(upper.join("ack"))
                && let Ok(ack) = serde_json::from_str::<Value>(&text)
                && ack["token"] == token
            {
                ensure!(
                    ack["op"] == op && ack["digest"] == digest && ack["bytes"] == bytes,
                    "guest integrity failed: {ack}"
                );
                return Ok(ack);
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("guest command timed out")?
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    let a = Args::parse();
    ensure!(
        matches!(
            a.pattern.as_str(),
            "repeated" | "compressible" | "random" | "mixed"
        ),
        "unknown pattern"
    );
    ensure!((5..=60).contains(&a.wait), "wait must be 5..60 seconds");
    fs::create_dir(&a.output)?;
    let root = a.output.canonicalize()?;
    ensure!(root.as_os_str().len() < 70, "short output required");
    let cg = group()?;
    ensure!(
        fs::read_to_string(cg.join("memory.max"))?.trim() == "2147483648"
            && fs::read_to_string(cg.join("memory.swap.max"))?.trim() == "0",
        "2 GiB/zero swap required"
    );
    let start = Instant::now();
    let mut report = json!({"schema":"pvisor-memory-savings/v1","mode":a.mode,"pattern":a.pattern,"wait":a.wait,"cpus":2,"memory_mib":256,"payload_bytes":BYTES,"correctness":"failed","phases":[],"tasks":[],"cleanup":false});
    persist(&root, &report)?;
    let digest = tokio::task::spawn_blocking({
        let pattern = a.pattern.clone();
        move || expected(&pattern)
    })
    .await?;
    report["expected_digest"] = json!(digest);
    let workspace = root.join("workspace");
    fs::create_dir(&workspace)?;
    let stage = root.join("stage");
    let settings = VmSettings {
        rootfs: Some(a.rootfs.canonicalize()?),
        library_dir: Some(a.firmware.canonicalize()?),
        memory_mib: 256,
        cpus: 2,
        cold_ram_compression: a.mode == Mode::Cold,
        ram_compression: a.mode == Mode::Compressed,
        ..Default::default()
    };
    let runtime = PVisor::builder()
        .executors(vec![Arc::new(VmExecutor::new(settings)?)])
        .network(pvisor::NetworkDriverConfig::new(
            pvisor::OverlayNetMode::Off,
            pvisor_core::NetworkConfig {
                mode: pvisor_core::NetworkMode::NoNetwork,
                ..Default::default()
            },
        ))
        .storage(root.join("jobs"))
        .overlay(OverlayHint {
            lower_dirs: vec![workspace.clone()],
            stage_dir: Some(stage.clone()),
            ..Default::default()
        })
        .build();
    let mut spec = RunSpec::process("memory-savings", "integrity", "/usr/bin/python3");
    spec.runtime.timeout_ms = Some(220_000);
    let RunInvocation::Process(p) = &mut spec.invocation;
    p.args = vec!["-B".into(), "-c".into(), GUEST.into(), a.pattern.clone()];
    p.cwd = Some("/workspace".into());
    p.stdout = StdioMode::Capture;
    p.stderr = StdioMode::Capture;
    spec.metadata
        .insert("pvisor.workspace".into(), json!(workspace));
    spec.metadata
        .insert("pvisor.vm.overlay_target".into(), json!("/workspace"));
    spec.metadata
        .insert("pvisor.vm.guest_cwd".into(), json!("/workspace"));
    phase(&root, &mut report, "before", start)?;
    let handle = runtime.run(spec).await?;
    let upper = stage.join("upper");
    let result = timeout(Duration::from_secs(200), async {
        let ready = command(&upper, "ready", "read", &digest, BYTES).await?;
        report["tasks"].as_array_mut().unwrap().push(ready);
        phase(&root, &mut report, "active", start)?;
        let transition = Instant::now();
        match a.mode {
            Mode::Pause => {
                handle.pause_vm().await?;
            }
            Mode::Raw | Mode::Compressed => {
                handle.offload(None).await?;
            }
            Mode::Release => {
                let empty = format!("{:x}", Sha256::digest([]));
                report["tasks"]
                    .as_array_mut()
                    .unwrap()
                    .push(command(&upper, "free", "free", &empty, 0).await?);
            }
            _ => {}
        }
        report["park_ms"] = json!(transition.elapsed().as_secs_f64() * 1000.);
        phase(&root, &mut report, "parked", start)?;
        for second in 1..=a.wait {
            sleep(Duration::from_secs(1)).await;
            if [5, 20, 60].contains(&second) || second == a.wait {
                phase(&root, &mut report, &format!("idle-{second}"), start)?;
            }
        }
        let restore = Instant::now();
        if matches!(a.mode, Mode::Pause | Mode::Raw | Mode::Compressed) {
            handle.resume_vm().await?;
        }
        report["resume_ack_ms"] = json!(restore.elapsed().as_secs_f64() * 1000.);
        let op = if a.mode == Mode::Release {
            "refill"
        } else {
            "read"
        };
        report["tasks"]
            .as_array_mut()
            .unwrap()
            .push(command(&upper, "restored", op, &digest, BYTES).await?);
        report["resume_to_task_ms"] = json!(restore.elapsed().as_secs_f64() * 1000.);
        phase(&root, &mut report, "restored", start)?;
        report["tasks"]
            .as_array_mut()
            .unwrap()
            .push(command(&upper, "exit", "exit", &digest, BYTES).await?);
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("experiment timeout")
    .and_then(|r| r);
    if result.is_err() {
        handle.cancellation().cancel();
    }
    let end = timeout(Duration::from_secs(20), handle.wait()).await;
    if let Ok(Ok(done)) = &end {
        report["exit_code"] = json!(done.exit_code);
        report["cleanup"] = json!(true);
        fs::write(
            root.join("guest.stdout"),
            done.output.stdout.as_deref().unwrap_or_default(),
        )?;
        fs::write(
            root.join("guest.stderr"),
            done.output.stderr.as_deref().unwrap_or_default(),
        )?;
    }
    report["correctness"] = json!(if result.is_ok()
        && matches!(&end,Ok(Ok(done)) if done.exit_code==Some(0))
    {
        "passed"
    } else {
        "failed"
    });
    if let Err(error) = &result {
        report["error"] = json!(format!("{error:#}"));
    }
    phase(&root, &mut report, "after", start)?;
    persist(&root, &report)?;
    result?;
    ensure!(report["correctness"] == "passed", "guest completion failed");
    Ok(())
}
