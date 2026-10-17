//! Two real native VMs plus the daemon pool implementation. Correctness only;
//! requires KVM, local guest firmware and (compressed mode) kernel-fault UFFD.
//! Run with a fresh, short output path and PVISOR_CASE_VM_LIBRARY_DIR as needed.
//! Append --physical to check adaptive physical-page scanning and COW instead.
fn main() -> anyhow::Result<()> {
    if pvisor::run_krun_internal_if_requested()? {
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    return linux::entry();
    #[cfg(not(target_os = "linux"))]
    anyhow::bail!("sealed compressed pool validation requires Linux x86_64")
}

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::{Context, ensure};
    use pvisor::{
        OverlayHint, PVisor, VmExecutor, VmSettings,
        ram_backing::ipc::{PoolClient, SealedClient},
    };
    use pvisor_core::{RunInvocation, RunSpec, RunState, StdioMode};
    use pvisor_daemon::memory_pool::{PoolConfig, PoolMode};
    use serde_json::json;
    use std::{
        fs,
        os::unix::{fs::DirBuilderExt, net::UnixStream},
        path::PathBuf,
        process::{Child, Command, Stdio},
        sync::Arc,
        time::{Duration, Instant},
    };

    const GUEST: &str = r#"
import hashlib, mmap, os, sys, time
index = int(sys.argv[1])
size = 16 * 1024 * 1024
shared = mmap.mmap(-1, size)
unique = mmap.mmap(-1, size)
shared[:] = b'\x61' * size
unique[:] = bytes([0x41 + index]) * size
expected = [hashlib.sha256(block).hexdigest() for block in (shared, unique)]
with open('ready.tmp', 'w') as f:
    f.write('ready')
os.replace('ready.tmp', 'ready')
deadline = time.monotonic() + 100
while not os.path.exists('go'):
    assert time.monotonic() < deadline, 'host did not release guest'
    time.sleep(0.05)
# Let the pager observe service loss and record a recovery baseline before
# touching the payload. Write through virtio-fs before a full CPU digest read.
time.sleep(1)
with open('cold-roundtrip', 'wb') as f:
    f.write(shared)
    f.write(unique)
    f.flush()
    os.fsync(f.fileno())
with open('cold-roundtrip', 'rb') as f:
    for digest in expected:
        assert hashlib.sha256(f.read(size)).hexdigest() == digest
    assert f.read(1) == b''
for block, digest in zip((shared, unique), expected):
    assert hashlib.sha256(block).hexdigest() == digest, 'cold RAM changed'
shared[4096:8192] = bytes([0x71 + index]) * 4096
assert shared[:4096] == b'\x61' * 4096
assert shared[4096:8192] == bytes([0x71 + index]) * 4096
assert shared[8192:] == b'\x61' * (size - 8192)
with open('roundtrip', 'wb') as f:
    f.write(shared)
    f.write(unique)
    f.flush()
    os.fsync(f.fileno())
with open('roundtrip', 'rb') as f:
    assert hashlib.sha256(f.read(size)).hexdigest() == hashlib.sha256(shared).hexdigest()
    assert hashlib.sha256(f.read(size)).hexdigest() == expected[1]
    assert f.read(1) == b''
if sys.argv[2] == 'physical':
    # Change a whole formerly shared region so bounded sampling can observe
    # COW churn, then verify each VM's private contents independently.
    shared[:] = bytes([0x71 + index]) * size
    assert shared[:] == bytes([0x71 + index]) * size
# Keep the VM alive for the periodic pager diagnostic to observe refaults.
time.sleep(1)
print('daemon-pool-guest-ok', index, flush=True)
"#;

    struct Service(Child);
    impl Drop for Service {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    pub(super) fn entry() -> anyhow::Result<()> {
        let args: Vec<_> = std::env::args_os().collect();
        // The exact same foreground component used by pvisor-daemon, without
        // requiring a prepared OpenSandbox execd/egress image for this probe.
        if args.get(1).is_some_and(|a| a == "memory-pool") {
            ensure!(
                args.get(2).is_some_and(|a| a == "--directory"),
                "expected --directory"
            );
            return pvisor_daemon::memory_pool::run(&PathBuf::from(
                args.get(3).context("missing pool directory")?,
            ));
        }
        ensure!(
            args.len() == 2 || (args.len() == 3 && args[2] == "--physical"),
            "expected fresh short output directory and optional --physical"
        );
        let mode = if args.len() == 3 {
            PoolMode::Physical
        } else {
            PoolMode::Compressed
        };
        ensure!(
            std::env::var_os("PVISOR_EXPERIMENTAL_MEMORY_METRICS").is_some(),
            "set PVISOR_EXPERIMENTAL_MEMORY_METRICS=1 to verify pager and scan counters"
        );
        let root = PathBuf::from(&args[1]);
        fs::DirBuilder::new().mode(0o700).create(&root)?;
        let root = root.canonicalize()?;
        ensure!(
            root.as_os_str().len() <= 60,
            "output path must be short for native sockets"
        );
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(run(root, mode))
    }

    async fn run(root: PathBuf, mode: PoolMode) -> anyhow::Result<()> {
        let directory = root.join("pool");
        fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let config = PoolConfig {
            mode,
            max_bytes: 32 << 20,
            max_objects: 4096,
            max_connections: 8,
            max_references: 8192,
        };
        fs::write(directory.join("config.json"), serde_json::to_vec(&config)?)?;
        let log = fs::File::create(root.join("pool.log"))?;
        let mut service = Service(
            Command::new(std::env::current_exe()?)
                .args(["memory-pool", "--directory"])
                .arg(&directory)
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log)
                .spawn()?,
        );
        let socket = directory.join("pool.sock");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            ensure!(
                service.0.try_wait()?.is_none(),
                "pool exited before readiness"
            );
            ensure!(Instant::now() < deadline, "pool startup deadline");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let probe = PoolClient::new(UnixStream::connect(&socket)?, Duration::from_secs(2))?;
        let (mut compressed, mut physical) = if mode == PoolMode::Compressed {
            (Some(SealedClient::new(probe)?), None)
        } else {
            let mut probe = probe;
            probe.enable_shared_mapping()?;
            (None, Some(probe))
        };
        let mut workers = Vec::new();
        let mut observation = json!(null);
        let checked = tokio::time::timeout(Duration::from_secs(85), async {
            for index in 0..2 {
                let workspace = root.join(format!("ws{index}"));
                fs::create_dir(&workspace)?;
                let stage = root.join(format!("t{index}"));
                let runtime = PVisor::builder()
                    .executors(vec![Arc::new(VmExecutor::new(VmSettings {
                        rootfs: Some(std::env::var_os("PVISOR_CASE_ROOTFS").map(PathBuf::from).unwrap_or_else(|| "/".into())),
                        library_dir: std::env::var_os("PVISOR_CASE_VM_LIBRARY_DIR").map(PathBuf::from),
                        memory_pool: Some(socket.clone()), memory_mib: 256, cpus: 1,
                        ..Default::default()
                    })?)])
                    .network(pvisor::NetworkDriverConfig::new(pvisor::OverlayNetMode::Off,
                        pvisor_core::NetworkConfig { mode: pvisor_core::NetworkMode::NoNetwork, ..Default::default() }))
                    .storage(root.join(format!("s{index}")))
                    .overlay(OverlayHint { lower_dirs: vec![workspace.clone()], stage_dir: Some(stage.clone()), ..Default::default() })
                    .build();
                let mut spec = RunSpec::process(format!("compressed-pool-{index}"), "pool-case",
                    std::env::var("PVISOR_CASE_VM_PYTHON").unwrap_or_else(|_| "/usr/bin/python3".into()));
                let RunInvocation::Process(process) = &mut spec.invocation;
                process.args = vec!["-c".into(), GUEST.into(), index.to_string(),
                    if mode == PoolMode::Physical { "physical" } else { "compressed" }.into()];
                process.cwd = Some("/workspace".into());
                process.stdout = StdioMode::Capture; process.stderr = StdioMode::Capture;
                spec.runtime.timeout_ms = Some(110_000);
                spec.metadata.insert("pvisor.workspace".into(), json!(workspace));
                spec.metadata.insert("pvisor.vm.overlay_target".into(), json!("/workspace"));
                spec.metadata.insert("pvisor.vm.guest_cwd".into(), json!("/workspace"));
                workers.push((runtime.run(spec).await?, workspace, stage.join("upper")));
            }
            while workers.iter().any(|(_, _, upper)| !upper.join("ready").exists()) {
                ensure!(workers.iter().all(|(h, _, _)| !h.status().state.is_terminal()), "guest exited before readiness");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            if let Some(probe) = &mut physical {
                loop {
                    ensure!(workers.iter().all(|(h, _, _)| !h.status().state.is_terminal()), "guest exited before physical sharing");
                    match probe.put_duplicate(&vec![0x61; 4096]) {
                        Ok(object) => {
                            let stats = probe.stats()?;
                            let shared = stats.cross_session_objects > 0;
                            probe.release(object)?;
                            if shared {
                                observation = json!({"encoded_bytes":stats.encoded_bytes,
                                    "objects":stats.objects,"probe_cross_session_objects":stats.cross_session_objects});
                                break;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {},
                        Err(error) => return Err(error.into()),
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
            if let Some(probe) = &mut compressed {
            // A matching reference outside this probe proves the VM pool has
            // accepted actual payload blocks, rather than merely zero/kernel
            // pages. Require shared bytes and each guest's distinct fill.
            let mut pinned = Vec::new();
            for value in [0x61, 0x41, 0x42] {
                loop {
                    ensure!(workers.iter().all(|(h, _, _)| !h.status().state.is_terminal()), "guest exited before pool admission");
                    let object = probe.put(&vec![value; 65536])?;
                    if probe.stats()?.cross_session_objects > pinned.len() as u64 {
                        pinned.push((value, object)); break;
                    }
                    probe.release(object)?; probe.stats()?;
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
            let stats = probe.stats()?;
            observation = json!({"encoded_bytes":stats.encoded_bytes,"objects":stats.objects,
                "probe_cross_session_objects":stats.cross_session_objects,"payload_fills":[97,65,66]});
            service.0.kill()?; service.0.wait()?;
            for (value, object) in pinned {
                let mut restored = vec![0; 65536];
                probe.restore(&object, &mut restored)?;
                ensure!(restored == vec![value; 65536], "local probe restore changed");
                probe.release(object)?;
            }
            probe.stats()?;
            ensure!(!probe.connected(), "publication survived terminated pool");
            }
            for (_, workspace, _) in &workers { fs::write(workspace.join("go"), b"go")?; }
            Ok::<_, anyhow::Error>(())
        }).await.context("native compressed-pool deadline").and_then(|result| result);
        if checked.is_err() {
            for (handle, _, _) in &workers {
                handle.cancel();
            }
        }
        let mut results = Vec::new();
        for (handle, _, _) in workers {
            results.push(handle.wait().await);
        }
        let recovery: Vec<_> = results
            .iter()
            .map(|result| {
                result
                    .as_ref()
                    .map_err(|error| anyhow::anyhow!(error.to_string()))
                    .and_then(|record| {
                        let stderr = record.output.stderr.as_deref().unwrap_or_default();
                        if mode == PoolMode::Physical {
                            scan_evidence(stderr)
                        } else {
                            recovery_evidence(stderr)
                        }
                    })
            })
            .collect();
        let passed = checked.is_ok()
            && recovery.iter().all(Result::is_ok)
            && results.len() == 2
            && results.iter().all(|r| {
                r.as_ref().is_ok_and(|r| {
                    r.state == RunState::Completed
                        && r.exit_code == Some(0)
                        && r.output
                            .stdout
                            .as_deref()
                            .is_some_and(|s| s.contains("daemon-pool-guest-ok"))
                })
            });
        let records: Vec<_> = results
            .into_iter()
            .map(|r| match r {
                Ok(r) => json!(r),
                Err(e) => json!({"error":e.to_string()}),
            })
            .collect();
        let recovery: Vec<_> = recovery
            .into_iter()
            .map(|result| result.unwrap_or_else(|error| json!({"error":error.to_string()})))
            .collect();
        fs::write(
            root.join("result.json"),
            serde_json::to_vec_pretty(&json!({"passed":passed,
            "check_error":checked.as_ref().err().map(|e| format!("{e:#}")),
            "pool":observation,"guests":records,"recovery":recovery,
            "mode":mode,
            "scope":"correctness; no physical-memory or performance claim"}))?,
        )?;
        checked?;
        ensure!(
            passed,
            "guest validation failed; inspect result.json and runner diagnostics"
        );
        println!("daemon-pool-case-ok mode={mode:?}");
        Ok(())
    }

    fn scan_evidence(stderr: &str) -> anyhow::Result<serde_json::Value> {
        let line = stderr
            .lines()
            .rfind(|line| line.starts_with("pvisor-shared-scan "))
            .context("missing adaptive scanner diagnostics")?;
        let counter = |name: &str| -> anyhow::Result<u64> {
            line.split_whitespace()
                .filter_map(|part| part.split_once('='))
                .find_map(|(key, value)| (key == name).then_some(value))
                .with_context(|| format!("missing {name}"))?
                .parse()
                .with_context(|| format!("invalid {name}"))
        };
        let probes = counter("probed_pages_total")?;
        let merges = counter("successful_merges_total")?;
        let cows = counter("cow_breaks_total")?;
        ensure!(
            probes > 0 && merges > 0 && cows > 0,
            "scan/merge/COW were not all observed"
        );
        Ok(
            json!({"probed_pages_total":probes,"successful_merges_total":merges,"cow_breaks_total":cows}),
        )
    }

    fn recovery_evidence(stderr: &str) -> anyhow::Result<serde_json::Value> {
        let mut disabled = false;
        let mut baseline = None;
        let mut restored = 0;
        let mut discarded = 0;
        for line in stderr.lines() {
            if line.starts_with("pvisor-sealed-pool publication_disabled=true ") {
                disabled = true;
            }
            if disabled && line.starts_with("pvisor-cold-linux ") {
                let counter = |name: &str| -> anyhow::Result<u64> {
                    line.split_whitespace()
                        .filter_map(|part| part.split_once('='))
                        .find_map(|(key, value)| (key == name).then_some(value))
                        .with_context(|| format!("missing {name}"))?
                        .parse()
                        .with_context(|| format!("invalid {name}"))
                };
                restored = counter("restored_bytes_total")?;
                discarded = counter("discarded_bytes_total")?;
                baseline.get_or_insert(restored);
            }
        }
        let baseline = baseline.context("missing post-service-loss pager diagnostics")?;
        ensure!(discarded > 0, "no RAM was discarded");
        ensure!(
            restored > baseline,
            "no observed refault after service loss"
        );
        Ok(json!({"discarded_bytes_total":discarded,
            "restored_bytes_at_first_post_loss_sample":baseline,
            "restored_bytes_at_last_sample":restored}))
    }
}
