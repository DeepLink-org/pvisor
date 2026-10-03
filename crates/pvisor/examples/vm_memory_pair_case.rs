//! Two full Linux VMs: baseline, whole-file offload and opt-in cold pager cases.
use anyhow::{Context, ensure};
use pvisor::{OverlayHint, PVisor, RunHandle, VmExecutor, VmSettings};
use pvisor_core::{RunInvocation, RunSpec, RunState, StdioMode};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

fn main() -> anyhow::Result<()> {
    if pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    if std::env::var_os("PVISOR_EXPERIMENTAL_MEMORY_PROCESS_INVENTORY").is_some() {
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(25));
            if let Err(error) = pvisor::ram_backing::record_process_inventory_if_requested() {
                eprintln!("experimental SDK process inventory failed: {error}");
            }
        });
    }
    tokio::runtime::Runtime::new()?.block_on(run())
}
async fn heartbeat(
    handle: &RunHandle,
    path: &Path,
    previous: Option<&[u8]>,
) -> anyhow::Result<Vec<u8>> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            ensure!(
                !handle.status().state.is_terminal(),
                "VM terminated: {:?}",
                handle.status()
            );
            if let Ok(bytes) = std::fs::read(path)
                && !bytes.is_empty()
                && previous != Some(bytes.as_slice())
            {
                return Ok(bytes);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("VM heartbeat timed out")?
}
async fn run() -> anyhow::Result<()> {
    // Register before launching VMs so an early SIGTERM is retained.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let aborted = tokio_util::sync::CancellationToken::new();
    let mode = std::env::args().nth(1).unwrap_or_else(|| "baseline".into());
    ensure!(
        [
            "baseline",
            "offload",
            "cold",
            "cold-baseline",
            "cold-stress",
            "cold-stress-baseline",
            "cold-device",
            "cold-net",
            "cold-net-baseline",
            "cold-pool-loss"
        ]
        .contains(&mode.as_str()),
        "unknown mode"
    );
    let guest = std::env::var_os("PVISOR_MEMORY_GUEST").context("PVISOR_MEMORY_GUEST required")?;
    let firmware =
        std::env::var_os("PVISOR_CASE_VM_LIBRARY_DIR").context("firmware directory required")?;
    let temporary = tempfile::tempdir()?;
    let rootfs = temporary.path().join("rootfs");
    std::fs::create_dir(&rootfs)?;
    std::fs::copy(guest, rootfs.join("memory-case"))?;
    let stress_rounds = std::env::var("PVISOR_MEMORY_STRESS_ROUNDS")
        .map(|value| value.parse::<u64>())
        .unwrap_or(Ok(3))?;
    anyhow::ensure!(
        (3..=100).contains(&stress_rounds),
        "stress rounds must be 3..=100"
    );
    let mut handles = Vec::new();
    let network_port = if mode.starts_with("cold-net") {
        let port = std::env::var("PVISOR_MEMORY_NETWORK_PORT")?.parse::<u16>()?;
        ensure!(port != 0, "network fixture port must be nonzero");
        port
    } else {
        0
    };
    let mut stages = Vec::new();
    // Separate SDKs give each VM independent overlay and RAM lifetime.
    let mut runtimes = Vec::new();
    for identity in [100u64, 200] {
        let root = temporary.path().join(identity.to_string());
        let workspace = root.join("workspace");
        let stage = root.join("stage");
        std::fs::create_dir_all(&workspace)?;
        let builder = PVisor::builder()
            .executors(vec![Arc::new(VmExecutor::new(VmSettings {
                memory_pool: std::env::var_os("PVISOR_CASE_MEMORY_POOL")
                    .or_else(|| std::env::var_os("PVISOR_EXPERIMENTAL_MEMORY_POOL"))
                    .map(PathBuf::from),
                rootfs: Some(rootfs.clone()),
                library_dir: Some(firmware.clone().into()),
                ram_backing: Some(root.join("live.ram")),
                memory_mib: 256,
                cpus: 2,
                ..Default::default()
            })?)])
            .storage(root.join("sdk"))
            .overlay(OverlayHint {
                lower_dirs: vec![workspace.clone()],
                stage_dir: Some(stage.clone()),
                ..Default::default()
            });
        let runtime = if mode.starts_with("cold-net") {
            builder.network(pvisor::NetworkDriverConfig::new(
                pvisor::OverlayNetMode::Auto,
                pvisor_overlaynet::NetworkConfig {
                    mode: pvisor_overlaynet::NetworkMode::Allowlist,
                    rules: vec![pvisor_core::NetworkAccessRule {
                        host: "localhost".into(),
                        ports: vec![network_port],
                        transports: vec![pvisor_core::NetworkTransport::TcpTunnel],
                        allow_private_ips: true,
                    }],
                    ..Default::default()
                },
            ))
        } else {
            builder
        }
        .build();
        let mut spec = RunSpec::process(format!("memory-{identity}"), "baseline", "/memory-case");
        spec.runtime.timeout_ms = Some((stress_rounds * 12 + 120) * 1000);
        spec.runtime.max_output_bytes = 8 * 1024 * 1024;
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec![
            identity.to_string(),
            if mode.starts_with("cold-stress") {
                "12"
            } else if mode.starts_with("cold") {
                "35"
            } else {
                "2"
            }
            .into(),
            if mode == "cold-device" {
                "device"
            } else if mode.starts_with("cold-stress") {
                "stress"
            } else if mode.starts_with("cold-net") {
                "network"
            } else {
                "normal"
            }
            .into(),
            stress_rounds.to_string(),
            network_port.to_string(),
        ];
        process.cwd = Some("/workspace".into());
        process.stdout = StdioMode::Capture;
        process.stderr = StdioMode::Capture;
        spec.metadata
            .insert("pvisor.workspace".into(), serde_json::json!(workspace));
        spec.metadata.insert(
            "pvisor.vm.overlay_target".into(),
            serde_json::json!("/workspace"),
        );
        spec.metadata.insert(
            "pvisor.vm.guest_cwd".into(),
            serde_json::json!("/workspace"),
        );
        handles.push(runtime.run(spec).await?);
        stages.push(stage.join("upper"));
        runtimes.push(runtime);
    }
    let cancellation_handles: Vec<_> = handles.iter().map(RunHandle::cancellation).collect();
    let cancellation = aborted.clone();
    let signal_task = tokio::spawn(async move {
        terminate.recv().await;
        for handle in cancellation_handles {
            handle.cancel();
        }
        cancellation.cancel();
    });
    let check = tokio::select! {
        _ = aborted.cancelled() => Err(anyhow::anyhow!("experiment cancelled by SIGTERM")),
        result = async {
        heartbeat(&handles[0], &stages[0].join("heartbeat"), None).await?;
        let second = heartbeat(&handles[1], &stages[1].join("heartbeat"), None).await?;
        println!("vm-memory-pair-ready");
        {
            use std::io::Write;
            std::io::stdout().flush()?;
        }
        if !mode.starts_with("cold") {
        handles[0].pause().await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let parked = std::fs::read(stages[0].join("heartbeat"))?;
        heartbeat(&handles[1], &stages[1].join("heartbeat"), Some(&second)).await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        ensure!(
            std::fs::read(stages[0].join("heartbeat"))? == parked,
            "paused VM progressed"
        );
        handles[0].resume().await?;
        heartbeat(&handles[0], &stages[0].join("heartbeat"), Some(&parked)).await?;
        }
        if mode == "offload" {
            for cycle in 0..3 {
                for vm in 0..2 {
                    let report = handles[vm].offload(None).await?;
                    ensure!(report.backed_bytes >= 256 * 1024 * 1024, "RAM range missing");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let parked = std::fs::read(stages[vm].join("heartbeat"))?;
                    let other = 1 - vm;
                    let before = std::fs::read(stages[other].join("heartbeat"))?;
                    heartbeat(&handles[other], &stages[other].join("heartbeat"), Some(&before)).await?;
                    ensure!(std::fs::read(stages[vm].join("heartbeat"))? == parked, "offloaded VM progressed");
                    handles[vm].resume().await?;
                    heartbeat(&handles[vm], &stages[vm].join("heartbeat"), Some(&parked)).await?;
                    println!("offload vm={vm} cycle={cycle} backed={} resident_before={:?} resident_after={:?}", report.backed_bytes, report.resident_before_bytes, report.resident_after_bytes);
                }
            }
        }
        if mode == "cold-pool-loss" {
            tokio::time::sleep(Duration::from_secs(20)).await;
            println!("pool-loss-ready");
            use std::io::Write;
            std::io::stdout().flush()?;
            tokio::time::timeout(Duration::from_secs(10), async {
                while handles.iter().any(|handle| !handle.status().state.is_terminal()) {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }).await.context("VMs did not fail promptly after pool loss")?;
        } else if mode.starts_with("cold") {
            tokio::time::sleep(Duration::from_secs(30)).await;
            if !matches!(mode.as_str(), "cold-baseline" | "cold-net-baseline" | "cold-stress-baseline") {
                ensure!(handles[0].offload(None).await.is_err(), "cold pager accepted incompatible whole-VM offload");
                ensure!(handles[0].status().state == RunState::Running, "rejected offload changed VM state");
            }
        }
        for stage in &stages {
            std::fs::write(stage.join("release"), b"go")?;
        }
        Ok::<_, anyhow::Error>(())
        } => result,
    };
    if check.is_err() {
        for handle in &handles {
            handle.cancel();
        }
    }
    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.wait().await?);
    }
    signal_task.abort();
    if aborted.is_cancelled() {
        eprintln!(
            "vm-memory-pair-cancelled states={:?}",
            results
                .iter()
                .map(|result| &result.state)
                .collect::<Vec<_>>()
        );
        anyhow::bail!("experiment cancelled by SIGTERM after VM cleanup");
    }
    if mode == "cold-pool-loss" {
        for (vm, result) in results.iter().enumerate() {
            println!(
                "pool-loss-result vm={vm} state={:?} exit_code={:?}",
                result.state, result.exit_code
            );
            eprintln!("vm-memory-result-{vm}");
            eprint!("{}", result.output.stderr.as_deref().unwrap_or_default());
        }
        check?;
        ensure!(
            results
                .iter()
                .all(|result| result.state == RunState::Failed),
            "pool loss was not reported as failure"
        );
        println!("vm-memory-pair-pool-loss-ok");
        return Ok(());
    }
    check?;
    for (vm, result) in results.into_iter().enumerate() {
        ensure!(
            !result.output.stdout_truncated && !result.output.stderr_truncated,
            "VM trace truncated"
        );
        ensure!(
            result.state == RunState::Completed && result.exit_code == Some(0),
            "guest failed: {result:?}"
        );
        let output = result.output.stdout.unwrap_or_default();
        ensure!(
            output.contains("guest-memory-ok"),
            "missing guest content check: {output}"
        );
        print!("{output}");
        if let Some(stderr) = result.output.stderr {
            if mode != "cold-baseline" && mode.starts_with("cold") {
                eprintln!("vm-memory-result-{vm}");
            }
            eprint!("{stderr}");
        }
    }
    println!("vm-memory-pair-{mode}-ok");
    Ok(())
}
