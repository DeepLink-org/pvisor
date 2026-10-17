//! B-SWE-RESIDENT-ENG: persistent VM worker for the external SweEval adapter.
use anyhow::{Context, ensure};
use pvisor::{OverlayHint, PVisor, VmExecutor, VmSettings};
use pvisor_core::{RunInvocation, RunSpec, RunState, StdioMode};
use serde::Deserialize;
use std::{collections::BTreeMap, fs, path::PathBuf, sync::Arc};

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
    let runtime = PVisor::builder()
        .executors(vec![Arc::new(VmExecutor::new(VmSettings {
            rootfs: Some(rootfs.clone()),
            library_dir: Some(config.firmware.canonicalize()?),
            ram_backing: Some(root.join("live.ram")),
            memory_mib: config.memory_mib,
            cpus: config.cpus,
            ..Default::default()
        })?)])
        .storage(root.join("records"))
        .overlay(OverlayHint {
            lower_dirs: vec![rootfs.clone()],
            stage_dir: Some(root.join("stage")),
            protect_target: true,
            ..Default::default()
        })
        .build();
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
    fs::write(
        root.join("driver-shutdown.json"),
        serde_json::to_vec_pretty(&serde_json::json!({"signal": shutdown}))?,
    )?;
    fs::write(
        root.join("run-result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    ensure!(
        (result.state == RunState::Completed && result.exit_code == Some(0))
            || (shutdown.is_some() && result.state == RunState::Cancelled),
        "guest worker failed: {result:?}"
    );
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
