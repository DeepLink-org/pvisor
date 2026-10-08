//! B-VCPU-IDLE-ENG / EXP-001 M0: real guest, observe-only public SDK.
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
mod experiment {
    use anyhow::{Context, ensure};
    use clap::Parser;
    use pvisor_vm::api::{
        RuntimeSupport, VcpuObservationControl, VcpuObservationSnapshot, VmBuilder,
        VmConfiguration, VmPlatform, VmRuntime,
    };
    use serde_json::{Value, json};
    use std::{
        fs,
        io::Write,
        path::PathBuf,
        time::{Duration, Instant},
    };

    #[derive(Parser)]
    struct Args {
        #[arg(long)]
        rootfs: PathBuf,
        #[arg(long)]
        init: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        workload: String,
        #[arg(long, value_parser=clap::value_parser!(u8).range(1..=2))]
        cpus: u8,
        #[arg(long, action=clap::ArgAction::Set)]
        observer: bool,
        #[arg(long, default_value_t=3, value_parser=clap::value_parser!(u64).range(1..=30))]
        seconds: u64,
        #[arg(long, default_value_t=10, value_parser=clap::value_parser!(u64).range(1..=1000))]
        interval_ms: u64,
        #[arg(long, default_value = "/usr/bin/python3")]
        python: String,
    }
    // No guest kernel change or idle hypercall. Files delimit work, not vCPU idle.
    const GUEST: &str = r#"
import hashlib, json, multiprocessing as mp, os, pathlib, sys, time
mode, cpus, seconds = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
root = pathlib.Path('/')
def atomic(name, value):
    p = root / (name + '.tmp')
    p.write_text(json.dumps(value)); p.replace(root / name)
def wait(name):
    end = time.monotonic() + 90
    while not (root / name).exists():
        if time.monotonic() > end: raise RuntimeError('handshake timeout: '+name)
        time.sleep(.01)
def worker(cpu, q):
    os.sched_setaffinity(0, {cpu})
    payload = bytes(range(256))*256
    expected = hashlib.sha256(payload).hexdigest()
    n = 0
    start = time.monotonic_ns()
    cpu_start = time.process_time_ns()
    deadline = start + seconds*1000000000
    busy = mode == 'busy' or (mode == 'smp-one-busy' and cpu == 0)
    if not busy and mode != 'short-timer':
        time.sleep(seconds); n = 1
    else:
        while time.monotonic_ns() < deadline:
            assert hashlib.sha256(payload).hexdigest() == expected
            n += 1
            if not busy: time.sleep(.002)
    assert hashlib.sha256(payload).hexdigest() == expected
    q.put(dict(cpu=cpu, affinity=sorted(os.sched_getaffinity(0)), iterations=n,
               digest=expected, elapsed_ns=time.monotonic_ns()-start,
               started_ns=start, finished_ns=time.monotonic_ns(), busy=busy,
               cpu_ns=time.process_time_ns()-cpu_start))
assert set(range(cpus)).issubset(os.sched_getaffinity(0)), 'guest CPU topology unavailable'
atomic('vcpu-ready', dict(cpus=cpus, mode=mode))
wait('vcpu-go')
start = time.monotonic_ns()
# This Linux guest parent has no threads before Process.start; avoid Python
# 3.14's default forkserver re-importing this handshake script during bootstrap.
ctx = mp.get_context('fork')
q = ctx.Queue()
workers = [ctx.Process(target=worker, args=(cpu,q)) for cpu in range(cpus)]
for p in workers: p.start()
rows = [q.get(timeout=seconds+15) for p in workers]
for p in workers:
    p.join(timeout=5); assert p.exitcode == 0, 'worker failed'
atomic('vcpu-done', dict(mode=mode, cpus=cpus, start_method=ctx.get_start_method(),
                       elapsed_ns=time.monotonic_ns()-start,
                       workers=sorted(rows,key=lambda x:x['cpu'])))
wait('vcpu-release')
print('vcpu-guest-ok', flush=True)
"#;

    fn snapshot(s: VcpuObservationSnapshot) -> Value {
        json!({"hypervisor":format!("{:?}",s.hypervisor), "enabled":s.enabled,
            "session":s.session,"topology_generation":s.topology_generation,"sequence":s.sequence,
            "sampled_at_ns":s.sampled_at.as_nanos(),"all_waiting":s.all_waiting,"idle_epoch":s.idle_epoch,
            "all_waiting_since_ns":s.all_waiting_since.map(|d|d.as_nanos()),
            "completed_all_waiting_ns":s.completed_all_waiting.as_nanos(),"rejection":format!("{:?}",s.rejection),
            "vcpus":s.vcpus.into_iter().map(|v|json!({"id":v.id,"online":v.online,
                "state":format!("{:?}",v.state),"sequence":v.sequence,"since_ns":v.since.as_nanos(),
                "transitions":v.transitions,"wait_entries":v.wait_entries,"wait_exits":v.wait_exits,
                "completed_wait_ns":v.completed_wait.as_nanos()})).collect::<Vec<_>>()})
    }
    pub fn run() -> anyhow::Result<()> {
        if std::env::args().nth(1).as_deref() == Some("--describe") {
            use sha2::{Digest, Sha256};
            let kernel = VmPlatform::embedded_kernel().map(|k| json!({
                "sha256": Sha256::digest(&k.bytes).iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "bytes": k.bytes.len(), "guest_address": k.guest_address, "entry_address": k.entry_address}));
            println!(
                "{}",
                json!({"firmware_name": VmPlatform::firmware_name(),
                "embedded_kernel": kernel, "capabilities": format!("{:?}", VmPlatform::capabilities())})
            );
            return Ok(());
        }
        let a = Args::parse();
        ensure!(
            ["sleep", "busy", "short-timer", "smp-one-busy"].contains(&a.workload.as_str()),
            "invalid workload"
        );
        ensure!(
            a.workload != "smp-one-busy" || a.cpus == 2,
            "SMP requires two CPUs"
        );
        let root = a.rootfs.canonicalize()?;
        ensure!(a.output.is_dir(), "output must be created by coordinator");
        for name in ["vcpu-ready", "vcpu-go", "vcpu-done", "vcpu-release"] {
            ensure!(!root.join(name).exists(), "stale handshake file: {name}");
        }
        let mut vm = VmBuilder::new(a.cpus, 256)?;
        vm.disable_implicit_init()?;
        vm.filesystem("/dev/root", &root, 0)?;
        vm.virtual_file("/dev/root", "/init.krun", fs::read(&a.init)?, 0o755, true)?;
        vm.virtual_file("/dev/root", "/vcpu-work.py", GUEST.as_bytes(), 0o444, false)?;
        let launch = json!({"argv":[a.python,"/vcpu-work.py",a.workload,a.cpus.to_string(),a.seconds.to_string()],
            "env":{},"cwd":"/"});
        vm.virtual_file(
            "/dev/root",
            "/.pvisor-guest.json",
            serde_json::to_vec(&launch)?,
            0o400,
            true,
        )?;
        vm.run(move |handle| {
            std::thread::spawn(move || {
                let result = (|| -> anyhow::Result<()> {
                    #[cfg(target_os = "linux")]
                    fs::write(a.output.join("ready-maps.txt"), fs::read("/proc/self/maps")?)?;
                    let deadline = Instant::now() + Duration::from_secs(a.seconds + 60);
                    while !root.join("vcpu-ready").exists() {
                        ensure!(Instant::now()<deadline,"guest ready timeout");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    handle.set_vcpu_observation(a.observer).map_err(anyhow::Error::msg)?;
                    let initial = snapshot(handle.vcpu_observation().map_err(anyhow::Error::msg)?);
                    fs::write(a.output.join("initial.json"), serde_json::to_vec(&initial)?)?;
                    let mut samples = fs::File::create(a.output.join("samples.jsonl"))?;
                    let mut count = 0u64;
                    let limit = (a.seconds + 60)*1000/a.interval_ms + 2;
                    fs::write(root.join("vcpu-go"), b"go")?;
                    loop {
                        ensure!(Instant::now()<deadline && count<limit,"bounded sampling timeout/cap");
                        if a.observer {
                            let start = Instant::now();
                            let s = snapshot(handle.vcpu_observation().map_err(anyhow::Error::msg)?);
                            let call_ns = start.elapsed().as_nanos();
                            writeln!(samples,"{}",json!({"snapshot":s,"sample_call_and_encode_ns":call_ns}))?;
                            count += 1;
                        }
                        if root.join("vcpu-done").exists() { break; }
                        std::thread::sleep(Duration::from_millis(a.interval_ms));
                    }
                    samples.sync_all()?;
                    let done: Value = serde_json::from_slice(&fs::read(root.join("vcpu-done"))?)?;
                    fs::write(a.output.join("guest.json"),serde_json::to_vec(&done)?)?;
                    fs::write(a.output.join("observer.json"),serde_json::to_vec(&json!({"observer":a.observer,"samples":count,"limit":limit,"complete":true}))?)?;
                    fs::write(root.join("vcpu-release"),b"release")?;
                    Ok(())
                })();
                if let Err(e) = result {
                    let _ = fs::write(a.output.join("observer-error.txt"), format!("{e:#}"));
                    eprintln!("observer failed: {e:#}");
                    std::process::exit(1);
                }
            });
            Ok(())
        }).context("VM run failed")?;
        Ok(())
    }
}
fn main() -> anyhow::Result<()> {
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    {
        experiment::run()
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        anyhow::bail!("unsupported VM platform")
    }
}
