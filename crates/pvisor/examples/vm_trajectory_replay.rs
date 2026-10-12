//! One B-TRAJECTORY-ENG sample. External coordinator owns budgets and sampling.
use anyhow::{Context, ensure};
use pvisor::{
    MemoryEventSink, NetworkDriverConfig, OverlayHint, OverlayNetMode, PVisor, RunHandle,
    VmExecutor, VmSettings,
};
use pvisor_core::operation::{OperationKind, Value as ControlValue, VmState};
use pvisor_core::{RunInvocation, RunSpec, RunState, StdioMode};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[derive(Deserialize)]
struct Config {
    rootfs: PathBuf,
    firmware: PathBuf,
    output: PathBuf,
    environment: BTreeMap<String, String>,
    commands_sha256: String,
    run_token: String,
    mode: String,
    kind: String,
    barriers: Vec<u64>,
    wait_seconds: f64,
    scenario: String,
}

fn now() -> u64 {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) },
        0
    );
    value.tv_sec as u64 * 1_000_000_000 + value.tv_nsec as u64
}

fn event(file: &mut File, name: &str, step: u64, data: Value) -> anyhow::Result<()> {
    writeln!(
        file,
        "{}",
        json!({"host_monotonic_ns":now(),"event":name,"step":step,"data":data})
    )?;
    file.flush()?;
    Ok(())
}

fn publish(path: &Path, message: &Value) -> anyhow::Result<()> {
    let temporary = path.with_extension("tmp");
    let mut file = File::create(&temporary)?;
    serde_json::to_writer(&mut file, message)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    File::open(path.parent().context("message parent")?)?.sync_all()?;
    Ok(())
}

async fn receive(
    handle: &RunHandle,
    directory: &Path,
    name: &str,
    timeout: u64,
) -> anyhow::Result<Value> {
    tokio::time::timeout(Duration::from_secs(timeout), async {
        loop {
            if let Ok(raw) = fs::read(directory.join(name)) {
                return serde_json::from_slice(&raw).context("invalid committed guest message");
            }
            if let Ok(error) = fs::read_to_string(directory.join("error.json")) {
                anyhow::bail!("guest error: {error}");
            }
            ensure!(
                !handle.status().state.is_terminal(),
                "guest ended before {name}: {:?}",
                handle.status()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .with_context(|| format!("waiting for {name}"))?
}

fn validate(value: &Value, boot: &Value, kind: &str, step: u64) -> anyhow::Result<()> {
    for key in ["schema", "run_token", "execution_token", "pid"] {
        ensure!(value[key] == boot[key], "wrong {key} in {kind}");
    }
    ensure!(
        value["kind"] == kind && value["step"] == step,
        "wrong message kind or sequence"
    );
    Ok(())
}

async fn run(config: Config) -> anyhow::Result<()> {
    ensure!(
        ["resident", "pause", "offload"].contains(&config.mode.as_str()),
        "bad mode"
    );
    ensure!(
        ["preflight", "handshake", "replay"].contains(&config.kind.as_str()),
        "bad kind"
    );
    ensure!(
        ["normal", "wrong-token", "no-go", "cancel"].contains(&config.scenario.as_str()),
        "bad scenario"
    );
    ensure!(
        config.wait_seconds.is_finite() && config.wait_seconds >= 0.0,
        "bad wait"
    );
    fs::create_dir(&config.output).context("sample VM output must be new")?;
    let root = config.output.canonicalize()?;
    let stage = root.join("stage");
    let control = stage.join("upper/replay-control");
    let output = stage.join("upper/replay-output");
    let mut events = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(root.join("events.jsonl"))?;
    event(
        &mut events,
        "driver_started",
        0,
        json!({"pid":std::process::id(),"control":control,"guest_control":"/replay-control"}),
    )?;
    let sink = Arc::new(MemoryEventSink::default());
    let runtime = PVisor::builder()
        .executors(vec![Arc::new(VmExecutor::new(VmSettings {
            rootfs: Some(config.rootfs.canonicalize()?),
            library_dir: Some(config.firmware.canonicalize()?),
            ram_backing: Some(root.join("live.ram")),
            memory_mib: 2048,
            cpus: 2,
            ram_compression: false,
            cold_ram_compression: false,
            ram_dedup: false,
            ..Default::default()
        })?)])
        .storage(root.join("records"))
        .event_sink(sink.clone())
        .network(NetworkDriverConfig::new(
            OverlayNetMode::Off,
            pvisor_overlaynet::NetworkConfig {
                mode: pvisor_overlaynet::NetworkMode::NoNetwork,
                ..Default::default()
            },
        ))
        .overlay(OverlayHint {
            lower_dirs: vec![config.rootfs.canonicalize()?],
            stage_dir: Some(stage.clone()),
            protect_target: true,
            ..Default::default()
        })
        .build();
    let mut spec = RunSpec::process(
        "trajectory-replay",
        "fixed-tools",
        "/opt/miniconda3/envs/testbed/bin/python",
    );
    spec.runtime.timeout_ms = Some(3_600_000);
    let RunInvocation::Process(process) = &mut spec.invocation;
    process.args = vec![
        "/replay-input/trajectory_worker.py".into(),
        "--run-token".into(),
        config.run_token.clone(),
        "--kind".into(),
        config.kind.clone(),
        "--barriers".into(),
        config
            .barriers
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(","),
        "--barrier-timeout".into(),
        if config.scenario == "no-go" {
            "2"
        } else {
            "600"
        }
        .into(),
    ];
    process.inherit_env = false;
    process.env = config.environment.clone();
    process.cwd = Some("/testbed".into());
    process.stdout = StdioMode::Capture;
    process.stderr = StdioMode::Capture;
    spec.metadata.insert(
        "pvisor.workspace".into(),
        json!(config.rootfs.canonicalize()?),
    );
    spec.metadata
        .insert("pvisor.vm.guest_cwd".into(), json!("/testbed"));
    let handle = runtime.run(spec).await?;
    let mut cancelled = false;
    let checked = async {
        let boot = receive(&handle, &control, "boot.json", 600).await?;
        ensure!(boot["schema"] == "pvisor-trajectory-barrier/v1" && boot["run_token"] == config.run_token, "boot identity");
        ensure!(boot["commands_sha256"] == config.commands_sha256, "guest input differs");
        event(&mut events, "guest_boot_received", 0, boot.clone())?;
        let barriers = if config.kind == "handshake" { vec![1, 2, 3] }
                       else if config.kind == "preflight" { vec![] } else { config.barriers.clone() };
        for step in barriers {
            let ready_name = format!("ready-{step:04}.json");
            let ready = receive(&handle, &control, &ready_name, 900).await?;
            validate(&ready, &boot, "ready", step)?;
            ensure!(ready["next_step"] == step + 1 && ready["completed"] == step, "ready sequence");
            let result = fs::read(output.join(format!("step-{step:04}.json")))?;
            let result_hash: String = Sha256::digest(&result).iter().map(|byte| format!("{byte:02x}")).collect();
            ensure!(ready["result_sha256"] == result_hash, "result not visible or corrupt");
            event(&mut events, "ready_received", step, ready.clone())?;
            // A repeated observation is not another control operation.
            ensure!(receive(&handle, &control, &ready_name, 1).await? == ready, "ready changed");
            tokio::time::sleep(Duration::from_millis(250)).await;
            ensure!(!control.join(format!("started-{:04}.json", step + 1)).exists(), "advanced without go");
            event(&mut events, "no_early_advance_verified", step, Value::Null)?;
            let primitive = match config.mode.as_str() {
                "pause" => Some(OperationKind::RunPause),
                "offload" => Some(OperationKind::RunOffload { file: None }),
                _ => None,
            };
            if let Some(primitive) = primitive {
                event(&mut events, "control_begin", step, json!(config.mode))?;
                let value = handle.control(primitive).await?;
                let expected = if config.mode == "pause" { VmState::Paused } else { VmState::Offloaded };
                ensure!(matches!(&value, ControlValue::Vm { state, .. } if *state == expected), "wrong native control state");
                event(&mut events, "control_done", step, serde_json::to_value(value)?)?;
                if config.scenario == "cancel" {
                    handle.cancel();
                    cancelled = true;
                    event(&mut events, "cancel_requested", step, Value::Null)?;
                    return Ok::<(), anyhow::Error>(());
                }
            }
            tokio::time::sleep(Duration::from_secs_f64(config.wait_seconds)).await;
            event(&mut events, "wait_end", step, Value::Null)?;
            if config.mode != "resident" {
                event(&mut events, "resume_begin", step, Value::Null)?;
                let value = handle.control(OperationKind::RunResume).await?;
                ensure!(matches!(value, ControlValue::Vm { state: VmState::Running, .. }), "wrong resume state");
                event(&mut events, "resume_done", step, serde_json::to_value(value)?)?;
            }
            if config.scenario == "no-go" {
                let error = receive(&handle, &control, "error.json", 10).await?;
                ensure!(error["error"].as_str().unwrap_or("").contains("no valid go"), "wrong negative failure");
                event(&mut events, "no_go_timeout_verified", step, error)?;
                return Ok(());
            }
            let mut go = json!({"schema":boot["schema"],"run_token":boot["run_token"],
                "execution_token":boot["execution_token"],"pid":boot["pid"],"kind":"go","step":step,"next_step":step+1});
            let go_path = control.join(format!("go-{step:04}.json"));
            if config.scenario == "wrong-token" {
                go["run_token"] = json!("incorrect-token");
                publish(&go_path, &go)?;
                let rejected = receive(&handle, &control, &format!("rejected-{step:04}.json"), 10).await?;
                validate(&rejected, &boot, "rejected", step)?;
                ensure!(!control.join(format!("resumed-{step:04}.json")).exists(), "bad token released guest");
                event(&mut events, "wrong_token_rejected", step, rejected)?;
                go["run_token"] = boot["run_token"].clone();
            }
            publish(&go_path, &go)?;
            event(&mut events, "go_sent", step, go)?;
            let resumed = receive(&handle, &control, &format!("resumed-{step:04}.json"), 30).await?;
            validate(&resumed, &boot, "resumed", step)?;
            ensure!(resumed["memory_ok"] == true && resumed["completed"] == step, "guest state corrupt");
            event(&mut events, "resumed_received", step, resumed)?;
            if config.kind == "replay" || step < 3 {
                let started = receive(&handle, &control, &format!("started-{:04}.json", step+1), 30).await?;
                validate(&started, &boot, "started", step+1)?;
                event(&mut events, "next_step_started_received", step, started)?;
                let next = receive(&handle, &output, &format!("step-{:04}.json", step+1), 70).await?;
                event(&mut events, "next_step_completed_received", step, next)?;
            }
        }
        let done = receive(&handle, &control, "done.json", 900).await?;
        for key in ["schema", "run_token", "execution_token", "pid"] { ensure!(done[key] == boot[key], "done identity"); }
        ensure!(done["completed"] == if config.kind == "preflight" { 0 } else if config.kind == "handshake" { 3 } else { 44 }, "incomplete guest");
        event(&mut events, "guest_done_received", 0, done)?;
        Ok(())
    }.await;
    if checked.is_err() {
        handle.cancel();
    }
    let result = handle.wait().await;
    fs::write(
        root.join("native-events.json"),
        serde_json::to_vec_pretty(&sink.events())?,
    )?;
    event(
        &mut events,
        "run_exited",
        0,
        json!({"result":format!("{result:?}"),"check":format!("{checked:?}")}),
    )?;
    let result = result?;
    fs::write(
        root.join("run-result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    checked?;
    if cancelled {
        ensure!(
            result.state == RunState::Cancelled,
            "cancel did not terminate"
        );
    } else if config.scenario == "no-go" {
        ensure!(
            result.exit_code.is_some_and(|code| code != 0),
            "no-go unexpectedly succeeded"
        );
    } else {
        ensure!(
            result.state == RunState::Completed && result.exit_code == Some(0),
            "guest failed: {result:?}"
        );
    }
    fs::write(
        root.join("driver-ok.json"),
        serde_json::to_vec_pretty(
            &json!({"correctness":"passed","scenario":config.scenario,"mode":config.mode,"kind":config.kind}),
        )?,
    )?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    if pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    let config = std::env::args().nth(1).context("expected config.json")?;
    let config = serde_json::from_slice(&fs::read(config)?)?;
    tokio::runtime::Runtime::new()?.block_on(run(config))
}
