//! B-SWE-RESIDENT-ENG: persistent VM worker for the external SweEval adapter.
use anyhow::{Context, ensure};
use pvisor::{
    CooperativeModelWait, EventSink, GatewayDriverConfig, MemoryEventSink, ModelWaitRounds,
    OverlayHint, PVisor, VmExecutor, VmSettings,
};
use pvisor_core::{RunInvocation, RunSpec, RunState, StdioMode};
use serde::Deserialize;
use std::{collections::BTreeMap, fs, path::PathBuf, sync::Arc, time::Duration};

#[path = "sweeval_support/event_log.rs"]
mod event_log;

#[derive(Deserialize)]
struct Config {
    rootfs: PathBuf,
    firmware: PathBuf,
    output: PathBuf,
    python: String,
    token: String,
    cpus: u16,
    memory_mib: u32,
    environment: BTreeMap<String, String>,
    #[serde(default)]
    management: Option<String>,
    #[serde(default)]
    gateway_listen: Option<String>,
    #[serde(default)]
    gateway_upstream: Option<String>,
    #[serde(default)]
    gateway_model: Option<String>,
}

async fn shutdown_signal() -> std::io::Result<&'static str> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => { result?; Ok("SIGINT") },
            _ = terminate.recv() => Ok("SIGTERM"),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
        Ok("SIGINT")
    }
}

async fn run(config: Config) -> anyhow::Result<()> {
    ensure!(
        config.cpus > 0 && config.memory_mib >= 512,
        "invalid VM size"
    );
    let root = config.output.canonicalize()?;
    let rootfs = config.rootfs.canonicalize()?;
    ensure!(
        rootfs != std::path::Path::new("/"),
        "task rootfs must not be host /"
    );
    let policy = config.management.as_deref().unwrap_or("baseline");
    ensure!(
        ["baseline", "pause", "offload"].contains(&policy),
        "invalid management policy"
    );
    if config.management.is_some() {
        ensure!(config.cpus == 1, "single-vCPU cohort only");
    }
    let mut vm = VmExecutor::new(VmSettings {
        rootfs: Some(rootfs.clone()),
        library_dir: Some(config.firmware.canonicalize()?),
        ram_backing: Some(root.join("live.ram")),
        memory_mib: config.memory_mib,
        cpus: config.cpus,
        ..Default::default()
    })?;
    // Same kernel profile across all groups; baseline never arms the gate.
    if config.management.is_some() {
        vm = vm.with_guest_idle_handoff_policy(true, Duration::from_secs(1))?;
    }
    let sink = Arc::new(MemoryEventSink::default());
    let live_events = sink.subscribe().context("event subscription required")?;
    let event_path = root.join("runtime-events.jsonl");
    let monitor = event_log::EventLog::start(live_events, move || fs::File::create(event_path));
    let mut builder = PVisor::builder()
        .executors(vec![Arc::new(vm)])
        .event_sink(sink.clone())
        .storage(root.join("records"))
        .overlay(OverlayHint {
            lower_dirs: vec![rootfs.clone()],
            stage_dir: Some(root.join("stage")),
            protect_target: true,
            ..Default::default()
        });
    if let Some(listen) = &config.gateway_listen {
        let proxy: pvisor_gateway::config::ProxyConfig = serde_json::from_value(
            serde_json::json!({
                "listen": listen, "admin_listen": "127.0.0.1:0", "capture_level": "full",
                "models": [{"name":config.gateway_model.as_deref().context("model required")?,
                    "provider":"openai", "upstream":config.gateway_upstream.as_deref().context("upstream required")?,
                    "api_key_env":"OPENAI_API_KEY"}]
            }),
        )?;
        proxy.validate()?;
        let mut gateway = GatewayDriverConfig::new(proxy);
        if policy != "baseline" {
            gateway = gateway.cooperative_model_wait(CooperativeModelWait {
                require_guest_idle: true,
                downloads: false,
                filesystem_reads: false,
                pause_after: Duration::from_millis(20),
                offload_after: (policy == "offload").then(|| Duration::from_millis(50)),
                stream_buffer: None,
                rounds: Some(ModelWaitRounds::default()),
            });
        }
        builder = builder.gateway(gateway);
    }
    let runtime = builder.build();
    let mut spec = RunSpec::process("sweeval-resident", "worker", config.python);
    spec.runtime.timeout_ms = Some(18_000_000);
    let RunInvocation::Process(process) = &mut spec.invocation;
    process.args = vec![
        "-u".into(),
        "-c".into(),
        include_str!("../../../benchmark/pvisor/sweeval/guest.py").into(),
        config.token,
    ];
    process.inherit_env = false;
    process.env = config.environment;
    process.cwd = Some("/".into());
    process.stdout = StdioMode::Capture;
    process.stderr = StdioMode::Capture;
    spec.metadata
        .insert("pvisor.workspace".into(), serde_json::json!(rootfs));
    spec.metadata
        .insert("pvisor.vm.guest_cwd".into(), serde_json::json!("/"));
    let handle = runtime.run(spec).await?;
    let cancellation = handle.cancellation();
    // This is the native runner identity, not the outer driver/systemd process.
    // Publish once before the adapter admits tools; pidfds then observe exit
    // without waiting for event logging or runtime/overlay finalization.
    let mut controls = handle.controls();
    let identity_path = root.join("runner-identity.json");
    let identity_cancel = cancellation.clone();
    let identity = tokio::spawn(async move {
        let result: anyhow::Result<()> = async {
            controls.wait_ready().await?;
            let sample = controls.cpu_sample().await;
            let usage = sample.usage.context(
                sample
                    .error
                    .unwrap_or_else(|| "runner identity unavailable".into()),
            )?;
            tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                let temp = identity_path.with_extension("tmp");
                fs::write(
                    &temp,
                    serde_json::to_vec(&serde_json::json!({
                        "pid": usage.pid, "start_time_ticks": usage.start_time_ticks,
                    }))?,
                )?;
                fs::rename(temp, identity_path)?;
                Ok(())
            })
            .await??;
            Ok(())
        }
        .await;
        if result.is_err() {
            identity_cancel.cancel();
        }
        result
    });
    let completion = handle.wait();
    tokio::pin!(completion);
    let mut shutdown = None;
    let result = tokio::select! {
        result = &mut completion => result?,
        signal = shutdown_signal() => {
            shutdown = Some(signal?);
            cancellation.cancel();
            completion.await?
        }
    };
    let identity_result = identity.await.context("runner identity task")?;
    let monitor_result = monitor.finish().await;
    let events = sink.events();
    let successful = (result.state == RunState::Completed && result.exit_code == Some(0))
        || (shutdown.is_some() && result.state == RunState::Cancelled);
    let failed_result = format!("guest worker failed: {result:?}");
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        fs::write(
            root.join("runtime-events.json"),
            serde_json::to_vec(&events)?,
        )?;
        fs::write(
            root.join("driver-shutdown.json"),
            serde_json::to_vec_pretty(&serde_json::json!({"signal": shutdown}))?,
        )?;
        fs::write(
            root.join("run-result.json"),
            serde_json::to_vec_pretty(&result)?,
        )?;
        Ok(())
    })
    .await??;
    identity_result.context("native runner identity publication failed")?;
    monitor_result.context("live event recording failed; final event array retained")?;
    ensure!(successful, "{failed_result}");
    Ok(())
}

fn main() -> anyhow::Result<()> {
    if pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    let path = std::env::args()
        .nth(1)
        .context("expected resident config.json")?;
    let config = serde_json::from_slice(&fs::read(path)?)?;
    tokio::runtime::Runtime::new()?.block_on(run(config))
}
