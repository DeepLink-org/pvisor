//! Linux/KVM one/two-vCPU engineering probe; local model fixture, real guest/NIC.
//! Usage: CASE_ROOT=... PVISOR_CASE_ROOTFS=... IDLE_FIXTURE_HOST=10.0.0.2
//! cargo run -p pvisor --features gateway --example vm_idle_handoff -- timer 10
#[cfg(feature = "gateway")]
mod probe {
    use anyhow::{Context, ensure};
    use axum::{Json, Router, extract::State, routing::post};
    use pvisor::{
        CooperativeModelWait, GatewayDriverConfig, MemoryEventSink, OverlayHint, PVisor,
        VmExecutor, VmSettings,
    };
    use pvisor_core::{RunInvocation, RunSpec, RunState, StdioMode};
    use serde_json::{Value, json};
    use std::{path::PathBuf, sync::Arc, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::{Notify, watch},
    };

    const GUEST: &str = r#"
import hashlib, json, os, socket, sys, threading, time, urllib.request, multiprocessing as mp
mode, rounds, address, port = sys.argv[1], int(sys.argv[2]), sys.argv[3], int(sys.argv[4])
base = os.environ['OPENAI_BASE_URL'].rstrip('/')
key = os.environ['OPENAI_API_KEY']
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
data = bytes(range(256))*4096
expected = hashlib.sha256(data).hexdigest()
def busy(stop, ready):
    hashlib.sha256(data).digest()
    ready.send(True)
    while not stop.value: hashlib.sha256(data).digest()
print(json.dumps(dict(kernel=os.uname().release, cmdline=open('/proc/cmdline').read(), mode=mode)), flush=True)
for i in range(rounds):
    times = {}
    worker = None
    stop = None
    if mode in ('network', 'race', 'timer', 'short'):
        sock = socket.create_connection((address, port), timeout=5)
        assert sock.recv(1)==b'R'
        def receive():
            assert sock.recv(1)==b'W'
            if mode in ('timer','short'):
                timer_start=time.monotonic()
                if mode=='timer': time.sleep(.08)
                else:
                    gaps=[]
                    for _ in range(400):
                        before=time.monotonic(); time.sleep(.001); gaps.append((time.monotonic()-before)*1000)
                    times['max_timer_gap_ms']=max(gaps)
                times['timer_elapsed_ms']=(time.monotonic()-timer_start)*1000
            times['event_ms']=(time.monotonic()-started)*1000
            sock.close()
        worker=threading.Thread(target=receive); worker.start()
    elif mode=='busy':
        ctx=mp.get_context('fork')
        stop=ctx.RawValue('b',False)
        parent,child=ctx.Pipe()
        worker=ctx.Process(target=busy,args=(stop,child)); worker.start()
        assert parent.poll(5) and parent.recv()
    started=time.monotonic()
    req=urllib.request.Request(base+'/chat/completions', data=json.dumps(dict(model='fixture',messages=[dict(role='user',content=str(i))],stream=False)).encode(),headers={'Content-Type':'application/json','Authorization':'Bearer '+key})
    try:
        with opener.open(req,timeout=10) as response: result=json.load(response)
    finally:
        if stop is not None: stop.value=True
    times['model_ms']=(time.monotonic()-started)*1000
    assert result['choices'][0]['message']['content']=='idle-ok'
    if worker:
        worker.join(timeout=5); assert not worker.is_alive()
        if mode=='busy': assert worker.exitcode==0
    if mode in ('timer','short','network','race'): assert 'event_ms' in times, times
    if mode=='timer': assert times['timer_elapsed_ms']<120, times
    if mode=='short': assert times['max_timer_gap_ms']<30, times
    if mode in ('timer','network'): assert times['event_ms']<times['model_ms']-30, times
    assert hashlib.sha256(data).hexdigest()==expected
    print(json.dumps(dict(cycle=i,mode=mode,**times)), flush=True)
    time.sleep(.03)
print('idle-guest-ok',flush=True)
"#;
    #[derive(Clone)]
    struct Fixture {
        mode: String,
        cpus: u16,
        paused: watch::Receiver<usize>,
        network: Arc<Notify>,
        skipped: Arc<std::sync::Mutex<Vec<String>>>,
    }
    async fn model(State(mut f): State<Fixture>, Json(body): Json<Value>) -> Json<Value> {
        if matches!(f.mode.as_str(), "timer" | "short") {
            f.network.notify_one();
        }
        if f.mode == "boundary" {
            let cycle = body["messages"][0]["content"]
                .as_str()
                .unwrap()
                .parse::<usize>()
                .unwrap();
            tokio::time::sleep(Duration::from_millis(
                [0, 15, 20, 21, 25, 80, 400][cycle % 7],
            ))
            .await;
        }
        if f.mode == "ingress" {
            let cycle: usize = body["messages"][0]["content"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap();
            // Deliver real NIC traffic independently of pause publication,
            // including before, during and after the 20 ms acquisition grace.
            tokio::time::sleep(Duration::from_millis(
                [0, 10, 19, 20, 21, 25, 50][cycle % 7],
            ))
            .await;
            f.network.notify_one();
        }
        if matches!(f.mode.as_str(), "network" | "race") || (f.mode == "timer" && f.cpus == 1) {
            let previous = *f.paused.borrow_and_update();
            let observed = tokio::time::timeout(Duration::from_millis(700), async {
                while *f.paused.borrow_and_update() <= previous {
                    f.paused.changed().await?;
                }
                Ok::<_, anyhow::Error>(())
            })
            .await;
            if !matches!(observed, Ok(Ok(()))) {
                f.skipped
                    .lock()
                    .unwrap()
                    .push("no native pause during model wait".into());
            }
            if matches!(f.mode.as_str(), "network" | "race") {
                f.network.notify_one();
            }
        }
        if f.mode == "ipi" {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        if !matches!(f.mode.as_str(), "race" | "boundary") {
            tokio::time::sleep(Duration::from_millis(220)).await;
        }
        Json(
            json!({"id":"fixture", "object":"chat.completion", "created":0,"model":"fixture",
            "choices":[{"index":0,"message":{"role":"assistant","content":"idle-ok"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}),
        )
    }
    pub async fn run() -> anyhow::Result<()> {
        let cpus: u16 = std::env::var("PVISOR_PROBE_CPUS")
            .unwrap_or("1".into())
            .parse()?;
        ensure!((1..=2).contains(&cpus), "probe supports one or two CPUs");
        let mode = std::env::args()
            .nth(1)
            .context("mode: timer/short/network/race/busy/boundary; dual also ipi/ingress")?;
        ensure!(
            ["timer", "short", "network", "race", "busy", "boundary"].contains(&mode.as_str())
                || (cpus == 2 && matches!(mode.as_str(), "ipi" | "ingress")),
            "unknown mode"
        );
        ensure!(
            cpus == 1 || mode != "short",
            "dual short-timer checks use timer mode"
        );
        let rounds: usize = std::env::args().nth(2).unwrap_or("5".into()).parse()?;
        ensure!((1..=100).contains(&rounds), "rounds 1..100");
        let root = PathBuf::from(std::env::var_os("CASE_ROOT").context("CASE_ROOT required")?);
        std::fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace)?;
        let host = std::env::var("IDLE_FIXTURE_HOST").context("IDLE_FIXTURE_HOST required")?;
        let network = tokio::net::TcpListener::bind((host.as_str(), 0)).await?;
        let port = network.local_addr()?.port();
        let trigger = Arc::new(Notify::new());
        let trigger_worker = trigger.clone();
        let net_task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = network.accept().await?;
                socket.write_all(b"R").await?;
                trigger_worker.notified().await;
                socket.write_all(b"W").await?;
                let mut byte = [0];
                let _ = socket.read(&mut byte).await;
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        });
        let model_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let model_port = model_listener.local_addr()?.port();
        let (paused, paused_rx) = watch::channel(0);
        let skipped = Arc::new(std::sync::Mutex::new(Vec::new()));
        let fixture = Fixture {
            mode: mode.clone(),
            cpus,
            paused: paused_rx,
            network: trigger,
            skipped: skipped.clone(),
        };
        let model_task = tokio::spawn(async move {
            axum::serve(
                model_listener,
                Router::new()
                    .route("/v1/chat/completions", post(model))
                    .with_state(fixture),
            )
            .await
        });
        let proxy = pvisor_gateway::config::ProxyConfig::from_toml_str(&format!(
            r#"
listen="127.0.0.1:0"
admin_listen="127.0.0.1:0"
[[models]]
name="fixture"
upstream="http://127.0.0.1:{model_port}/v1"
provider="openai"
api_key="local-fixture"
"#
        ))?;
        let sink = Arc::new(MemoryEventSink::default());
        let settings = VmSettings {
            rootfs: Some(
                std::env::var_os("PVISOR_CASE_ROOTFS")
                    .context("rootfs required")?
                    .into(),
            ),
            library_dir: std::env::var_os("PVISOR_CASE_VM_LIBRARY_DIR").map(Into::into),
            cpus,
            memory_mib: 512,
            ..Default::default()
        };
        let vm = VmExecutor::new(settings)?.with_guest_idle_handoff()?;
        let runtime = PVisor::builder()
            .executors(vec![Arc::new(vm)])
            .storage(root.join("run"))
            .event_sink(sink.clone())
            .overlay(OverlayHint {
                lower_dirs: vec![workspace.clone()],
                stage_dir: Some(root.join("stage")),
                ..Default::default()
            })
            .network(pvisor::NetworkDriverConfig::new(
                pvisor::OverlayNetMode::Auto,
                pvisor_core::NetworkConfig {
                    mode: pvisor_core::NetworkMode::Allowlist,
                    allowed_hosts: vec![host.clone()],
                    ..Default::default()
                },
            ))
            .gateway(
                GatewayDriverConfig::new(proxy).cooperative_model_wait(CooperativeModelWait {
                    require_guest_idle: true,
                    downloads: false,
                    filesystem_reads: false,
                    stream_buffer: None,
                    rounds: (cpus == 2).then(pvisor::ModelWaitRounds::default),
                    pause_after: Duration::from_millis(20),
                    offload_after: None,
                }),
            )
            .build();
        let mut spec = RunSpec::process(
            "idle-handoff",
            &mode,
            std::env::var("PVISOR_CASE_VM_PYTHON").unwrap_or("/opt/miniconda3/bin/python".into()),
        );
        spec.runtime.timeout_ms = Some(60_000 + rounds as u64 * 2000);
        let RunInvocation::Process(p) = &mut spec.invocation;
        p.args = vec![
            "-B".into(),
            "-u".into(),
            "-c".into(),
            if cpus == 2 {
                include_str!("vm_dual_idle_guest.py").into()
            } else {
                GUEST.into()
            },
            mode.clone(),
            rounds.to_string(),
            host,
            port.to_string(),
        ];
        p.cwd = Some("/workspace".into());
        p.stdout = StdioMode::Capture;
        p.stderr = StdioMode::Capture;
        spec.metadata
            .insert("pvisor.workspace".into(), json!(workspace));
        spec.metadata
            .insert("pvisor.vm.overlay_target".into(), json!("/workspace"));
        spec.metadata
            .insert("pvisor.vm.guest_cwd".into(), json!("/workspace"));
        spec.runtime.max_output_bytes = 16 * 1024 * 1024;
        let handle = runtime.run(spec).await?;
        let monitor_sink = sink.clone();
        let monitor = tokio::spawn(async move {
            loop {
                let count = monitor_sink
                    .events()
                    .iter()
                    .filter(|e| {
                        e.name() == "vm.control_completed"
                            && e.observation_payload()
                                .is_some_and(|v| v["value"]["vm"]["state"] == "paused")
                    })
                    .count();
                if count > *paused.borrow() {
                    paused.send_replace(count);
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
        let result = handle.wait().await;
        monitor.abort();
        net_task.abort();
        model_task.abort();
        let events = sink.events();
        std::fs::write(
            root.join("events.json"),
            serde_json::to_vec_pretty(&events)?,
        )?;
        let result = result?;
        std::fs::write(
            root.join("result.json"),
            serde_json::to_vec_pretty(&result)?,
        )?;
        let stdout = result.output.stdout.as_deref().unwrap_or_default();
        println!("{stdout}");
        let controls: Vec<_> = events
            .iter()
            .filter(|e| e.name() == "vm.control_completed")
            .filter_map(|e| e.observation_payload())
            .collect();
        let pauses = controls
            .iter()
            .filter(|v| v["value"]["vm"]["state"] == "paused")
            .count();
        let resumes = controls
            .iter()
            .filter(|v| {
                v["value"]["vm"]["state"] == "running"
                    && v["operation"]["kind"]["op"] == "run.resume"
            })
            .count();
        let paused_epochs: std::collections::BTreeSet<_> = controls
            .iter()
            .filter(|v| v["value"]["vm"]["state"] == "paused")
            .filter_map(|v| v["guest_idle_epoch"].as_u64())
            .collect();
        let wake_reasons: Vec<_> = events
            .iter()
            .filter(|e| e.name() == "vm.guest_idle")
            .filter_map(|e| e.observation_payload())
            .filter(|v| {
                v["epoch"]
                    .as_u64()
                    .is_some_and(|epoch| paused_epochs.contains(&epoch))
            })
            .filter_map(|v| v.get("wake_reason").and_then(Value::as_str))
            .collect();
        let report = json!({"cpus":cpus,"mode":mode,"rounds":rounds,"pauses":pauses,"resumes":resumes,"wake_reasons":wake_reasons,"unpaused_calls":*skipped.lock().unwrap()});
        std::fs::write(
            root.join("summary.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        println!("{report}");
        ensure!(
            result.state == RunState::Completed && result.exit_code == Some(0),
            "guest failed: {result:?}"
        );
        ensure!(stdout.contains("idle-guest-ok"), "missing guest completion");
        ensure!(pauses == resumes, "unbalanced pause/resume");
        if cpus == 2 && matches!(mode.as_str(), "network" | "race") {
            ensure!(
                skipped.lock().unwrap().is_empty(),
                "dual-CPU handoff missed a model-wait window"
            );
        }
        if matches!(mode.as_str(), "timer" | "network" | "race" | "ipi") {
            ensure!(pauses > 0, "positive test did not exercise native pause");
        }
        if mode == "timer" {
            ensure!(
                wake_reasons.contains(&"guest_timer"),
                "no timer wake while paused"
            );
        }
        if mode == "network" {
            ensure!(
                wake_reasons.contains(&"network_rx"),
                "no network RX wake observed"
            );
        }
        if mode == "busy" {
            ensure!(pauses == 0, "busy guest incorrectly accepted idle");
        }
        ensure!(
            !controls
                .iter()
                .any(|v| v["value"]["vm"]["state"] == "offloaded"),
            "M1 must never offload"
        );
        Ok(())
    }
}
fn main() -> anyhow::Result<()> {
    if pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    #[cfg(feature = "gateway")]
    {
        tokio::runtime::Runtime::new()?.block_on(probe::run())
    }
    #[cfg(not(feature = "gateway"))]
    {
        anyhow::bail!("requires --features gateway")
    }
}
