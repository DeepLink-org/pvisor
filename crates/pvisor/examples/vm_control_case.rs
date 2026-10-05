//! Black-box SDK driver for S-DOC-059..062. Invoke via `just vm-cases`.
use anyhow::{Context, ensure};
use pvisor::{MemoryEventSink, OverlayHint, PVisor, RunHandle, VmExecutor, VmSettings};
use pvisor_core::{RunInvocation, RunSpec, RunState};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

const COMPRESSED_CYCLES: usize = 10;

const GUEST: &str = r#"
import hashlib, pathlib, time
data = bytearray(bytes(range(256)) * (8 * 1024 * 1024 // 256))
expected = hashlib.sha256(data).hexdigest()
pathlib.Path('ready').write_text(expected)
n = 0
scratch = bytearray(1024 * 1024)
while not pathlib.Path('release').exists():
    assert scratch == n.to_bytes(8, 'little') * (len(scratch) // 8), 'mutable guest RAM corrupted'
    n += 1
    scratch[:] = n.to_bytes(8, 'little') * (len(scratch) // 8)
    pathlib.Path('heartbeat').write_text(str(n))
    time.sleep(0.05)
assert hashlib.sha256(data).hexdigest() == expected, 'guest RAM corrupted'
assert scratch == n.to_bytes(8, 'little') * (len(scratch) // 8), 'mutable guest RAM corrupted'
print('guest-memory-ok', flush=True)
"#;

fn main() -> anyhow::Result<()> {
    // VmExecutor re-execs its host executable. Enter the runner before creating
    // a Tokio runtime or parsing the parent's test-driver arguments.
    if pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    tokio::runtime::Runtime::new()?.block_on(run())
}

async fn await_heartbeat(
    handle: &RunHandle,
    path: &Path,
    previous: Option<&[u8]>,
) -> anyhow::Result<Vec<u8>> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            ensure!(
                !handle.status().state.is_terminal(),
                "guest exited before heartbeat: {:?}",
                handle.status()
            );
            if let Ok(bytes) = std::fs::read(path)
                && !bytes.is_empty()
                && previous != Some(bytes.as_slice())
            {
                ensure!(
                    handle.status().state == RunState::Running,
                    "resumed VM must be running"
                );
                return Ok(bytes);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("guest heartbeat timed out")?
}

async fn parked(handle: &RunHandle, heartbeat: &Path) -> anyhow::Result<Vec<u8>> {
    ensure!(
        handle.status().state == RunState::Suspended,
        "VM must be suspended"
    );
    // Let already submitted virtio-fs writes finish before sampling. This checks
    // guest execution, not a promise that device workers are paused.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = std::fs::read(heartbeat)?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    ensure!(
        std::fs::read(heartbeat)? == before,
        "guest progressed while paused"
    );
    Ok(before)
}

async fn run() -> anyhow::Result<()> {
    let mode = std::env::args()
        .nth(1)
        .context("expected pause, offload, reject or compressed")?;
    ensure!(
        ["pause", "offload", "reject", "compressed"].contains(&mode.as_str()),
        "unknown case"
    );
    let root = std::env::var_os("CASE_ROOT").context("CASE_ROOT required")?;
    let root = Path::new(&root).canonicalize()?;
    let workspace = root.join("ws");
    let stage = root.join("vm-stage");
    let explicit = mode != "offload";
    let backing = root.join("startup.ram");
    let library_dir = std::env::var_os("PVISOR_CASE_VM_LIBRARY_DIR").map(Into::into);
    #[cfg(not(any(
        all(target_os = "linux", target_env = "musl", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "x86_64")
    )))]
    let library_dir = if library_dir.is_none() {
        use pvisor_vm::api::RuntimeSupport;
        Some(
            tokio::task::spawn_blocking(|| pvisor_vm::api::VmPlatform::prepare_firmware(None))
                .await
                .context("VM firmware preparation task failed")??,
        )
    } else {
        library_dir
    };
    let settings = VmSettings {
        rootfs: Some(
            std::env::var_os("PVISOR_CASE_ROOTFS")
                .context("PVISOR_CASE_ROOTFS required")?
                .into(),
        ),
        library_dir,
        ram_backing: Some(backing.clone()),
        ram_compression: mode == "compressed",
        memory_mib: 256,
        cpus: 2,
        ..Default::default()
    };
    let sink = Arc::new(MemoryEventSink::default());
    let runtime = PVisor::builder()
        .executors(vec![Arc::new(VmExecutor::new(settings)?)])
        .storage(root.join("sdk-run"))
        .event_sink(sink.clone())
        .overlay(OverlayHint {
            lower_dirs: vec![workspace.clone()],
            stage_dir: Some(stage.clone()),
            ..Default::default()
        })
        .build();
    let mut spec = RunSpec::process(
        "vm-control-case",
        "ram-check",
        std::env::var("PVISOR_CASE_VM_PYTHON").unwrap_or_else(|_| "/usr/bin/python3".into()),
    );
    spec.runtime.timeout_ms = Some(240_000);
    let RunInvocation::Process(process) = &mut spec.invocation;
    process.args = vec!["-c".into(), GUEST.into()];
    process.cwd = Some("/workspace".into());
    process.stdout = pvisor_core::StdioMode::Capture;
    process.stderr = pvisor_core::StdioMode::Capture;
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
    let handle = runtime.run(spec).await?;
    let cancellation = handle.cancellation();
    let heartbeat = stage.join("upper/heartbeat");
    let check = async {
        await_heartbeat(&handle, &heartbeat, None).await?;
        if mode == "reject" {
            let occupied = root.join("occupied.ram");
            std::fs::write(&occupied, b"keep")?;
            let before = std::fs::read(&heartbeat)?;
            ensure!(
                handle.offload(Some(occupied.clone())).await.is_err(),
                "existing file accepted"
            );
            ensure!(std::fs::read(occupied)? == b"keep", "existing file changed");
            ensure!(
                handle.status().state == RunState::Running,
                "rejection changed state"
            );
            ensure!(!cancellation.is_cancelled(), "rejection cancelled VM");
            await_heartbeat(&handle, &heartbeat, Some(&before)).await?;
            handle.pause().await?;
            handle.resume().await?;
        } else {
            handle.pause().await?;
            handle.pause().await?;
            let before = parked(&handle, &heartbeat).await?;
            handle.resume().await?;
            handle.resume().await?;
            await_heartbeat(&handle, &heartbeat, Some(&before)).await?;
            let selected = root.join("offloaded.ram");
            let cycles = if mode == "compressed" { COMPRESSED_CYCLES } else { 2 };
            let mut heads = BTreeSet::new();
            let mut previous_depth = 0;
            let mut compacted = false;
            for cycle in 0..cycles {
                let offload_started = Instant::now();
                let report = handle
                    .offload((cycle == 0 && !explicit).then(|| selected.clone()))
                    .await?;
                let offload_ms = offload_started.elapsed().as_millis();
                let mut compressed_metrics = None;
                ensure!(report.backed_bytes >= 256 * 1024 * 1024, "RAM range missing");
                ensure!(
                    report.backing_file == if explicit { backing.clone() } else { selected.clone() },
                    "wrong backing path"
                );
                if mode == "compressed" {
                    let reader = pvisor::ram_backing::CompressedRam::open(std::fs::File::open(
                        &report.backing_file,
                    )?)?;
                    ensure!(reader.logical_bytes() >= report.backed_bytes, "compressed RAM range missing");
                    let allocated = reader.allocated_bytes()?;
                    ensure!(allocated < report.backed_bytes, "compressed backing saved no disk space");
                    let head = reader.head_id().context("missing committed RAM generation")?;
                    ensure!(heads.insert(head), "resumed guest writes did not produce a new generation");
                    let depth = reader.chain_depth();
                    ensure!((1..=8).contains(&depth), "unbounded delta chain");
                    compacted |= previous_depth == 8 && depth == 1;
                    previous_depth = depth;
                    let mut layers = backing.as_os_str().to_os_string();
                    layers.push(".layers");
                    let files = std::fs::read_dir(Path::new(&layers))?
                        .collect::<Result<Vec<_>, _>>()?
                        .iter()
                        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "pvdelta"))
                        .count();
                    ensure!(files == depth, "unreachable generations were not collected");
                    let mut sample = [0; 4096];
                    ensure!(reader.read_at(0, &mut sample)? == sample.len(), "delta chain cannot restore RAM");
                    compressed_metrics = Some((depth, files, allocated));
                } else {
                    ensure!(std::fs::metadata(&report.backing_file)?.len() >= report.backed_bytes, "backing too short");
                }
                let before = parked(&handle, &heartbeat).await?;
                let resume_started = Instant::now();
                handle.resume().await?;
                let resume_ms = resume_started.elapsed().as_millis();
                if let Some((depth, files, allocated)) = compressed_metrics {
                    println!("compressed-cycle={} depth={depth} layers={files} allocated={allocated} backed={} offload_ms={offload_ms} resume_ms={resume_ms} resident_before={:?} resident_after={:?}", cycle + 1, report.backed_bytes, report.resident_before_bytes, report.resident_after_bytes);
                }
                await_heartbeat(&handle, &heartbeat, Some(&before)).await?;
            }
            ensure!(mode != "compressed" || compacted, "compaction was not exercised");
        }
        std::fs::write(stage.join("upper/release"), b"go")?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if check.is_err() {
        handle.cancel();
    }
    let result = handle.wait().await?;
    check.with_context(|| format!("VM run result: {result:?}"))?;
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
        "missing guest integrity check"
    );
    let events = sink.events();
    let controls = events
        .iter()
        .filter(|event| event.name().starts_with("vm.control_"))
        .collect::<Vec<_>>();
    let operations = if mode == "reject" {
        vec![
            ("run.offload", "failed"),
            ("run.pause", "paused"),
            ("run.resume", "running"),
        ]
    } else {
        let mut operations = vec![
            ("run.pause", "paused"),
            ("run.pause", "paused"),
            ("run.resume", "running"),
            ("run.resume", "running"),
        ];
        for _ in 0..if mode == "compressed" {
            COMPRESSED_CYCLES
        } else {
            2
        } {
            operations.extend([("run.offload", "offloaded"), ("run.resume", "running")]);
        }
        operations
    };
    ensure!(
        controls.len() == operations.len() * 2,
        "missing or duplicate control events"
    );
    for (pair, (operation, state)) in controls.as_chunks::<2>().0.iter().zip(&operations) {
        ensure!(
            pair[0].name() == "vm.control_requested",
            "missing request event"
        );
        ensure!(
            pair[0].observation_payload().unwrap()["operation"]["kind"]["op"] == *operation,
            "wrong request primitive"
        );
        let name = if *state == "failed" {
            "vm.control_failed"
        } else {
            "vm.control_completed"
        };
        ensure!(pair[1].name() == name, "wrong completion event");
        if *state != "failed" {
            ensure!(
                pair[1].observation_payload().unwrap()["value"]["vm"]["state"] == *state,
                "wrong completion state"
            );
        }
    }
    println!("vm-control-case-ok {mode}");
    Ok(())
}
