//! Whole real Linux guest across two runner processes; internal API experiment.
#![cfg_attr(not(all(target_os = "macos", target_arch = "aarch64")), allow(unused))]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn main() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use krun_vmm::snapshot::{MachineRestore, MachineSnapshot};
    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};
    use std::{
        ffi::CString,
        fs::{File, OpenOptions},
        path::PathBuf,
        sync::Arc,
        time::{Duration, Instant},
    };
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Saved {
        version: u32,
        boot: String,
        binary: String,
        ram_hash: String,
        state_hash: String,
        source_pid: u32,
        state: MachineSnapshot,
    }
    fn hash(path: &std::path::Path) -> anyhow::Result<String> {
        use std::io::Read;
        let mut file = File::open(path)?;
        let mut buffer = [0; 1024 * 1024];
        let mut hash = Sha256::new();
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        Ok(hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }
    fn state_hash(state: &MachineSnapshot) -> anyhow::Result<String> {
        Ok(Sha256::digest(serde_json::to_vec(state)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }
    fn check(value: i32) -> anyhow::Result<()> {
        ensure!(value >= 0, "libkrun error {value}");
        Ok(())
    }
    let mode = std::env::args()
        .nth(1)
        .context("save or restore required")?;
    ensure!(mode == "save" || mode == "restore", "invalid mode");
    let base = PathBuf::from(
        std::env::args()
            .nth(2)
            .context("experiment directory required")?,
    );
    let root = base.join("rootfs");
    let lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(base.join("execution.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lease).context("another runner owns this experiment")?;
    let boot = String::from_utf8(
        std::process::Command::new("sysctl")
            .args(["-n", "kern.bootsessionuuid"])
            .output()?
            .stdout,
    )?
    .trim()
    .to_owned();
    ensure!(!boot.is_empty(), "host boot identity unavailable");
    let binary = hash(&std::env::current_exe()?)?;
    check(krun::krun_set_log_level(5))?;
    let ctx = krun::krun_create_ctx();
    check(ctx)?;
    let ctx = ctx as u32;
    check(krun::krun_set_vm_config(ctx, 2, 256))?;
    check(krun::krun_set_snapshot_profile(ctx))?;
    check(krun::krun_disable_implicit_init(ctx))?;
    check(krun::krun_disable_implicit_vsock(ctx))?;
    check(krun::krun_add_vsock(ctx, 0))?;
    let path = CString::new(root.as_os_str().as_encoded_bytes())?;
    check(unsafe { krun::krun_add_virtiofs2(ctx, c"/dev/root".as_ptr(), path.as_ptr(), 0) })?;
    if mode == "restore" {
        let saved: Saved = serde_json::from_slice(&std::fs::read(base.join("state.json"))?)?;
        ensure!(
            saved.version == 1 && saved.boot == boot && saved.binary == binary,
            "snapshot host/build mismatch"
        );
        ensure!(
            saved.source_pid != std::process::id(),
            "runner process was reused"
        );
        ensure!(
            hash(&base.join("ram.bin"))? == saved.ram_hash,
            "RAM snapshot digest mismatch"
        );
        ensure!(
            state_hash(&saved.state)? == saved.state_hash,
            "machine state digest mismatch"
        );
        krun::krun_set_machine_restore(
            ctx,
            MachineRestore {
                state: saved.state,
                ram_file: Arc::new(File::open(base.join("ram.bin"))?),
            },
        )
        .map_err(anyhow::Error::msg)?;
    }
    let rc = krun::krun_start_enter_with_handle(ctx, move |handle| {
        let mode = mode.clone();
        let base = base.clone();
        let root = root.clone();
        let boot = boot.clone();
        let binary = binary.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                if mode == "restore" {
                    ensure!(
                        handle.is_paused().map_err(anyhow::Error::msg)?,
                        "restored VM was already executing"
                    );
                    handle.resume().map_err(anyhow::Error::msg)?;
                    println!("linux-restore-runner-ready pid={}", std::process::id());
                    return Ok(());
                }
                let deadline = Instant::now() + Duration::from_secs(30);
                while !root.join("ready").exists() {
                    ensure!(Instant::now() < deadline, "guest ready timeout");
                    std::thread::sleep(Duration::from_millis(20));
                }
                std::thread::sleep(Duration::from_millis(150));
                handle
                    .with_snapshot_quiesced(Duration::from_secs(10), |vm| {
                        let capture = (|| -> anyhow::Result<()> {
                            let ram = OpenOptions::new()
                                .create_new(true)
                                .read(true)
                                .write(true)
                                .open(base.join("ram.bin"))?;
                            let state =
                                vm.capture_machine_state(&ram).map_err(anyhow::Error::msg)?;
                            let saved = Saved {
                                version: 1,
                                boot,
                                binary,
                                ram_hash: hash(&base.join("ram.bin"))?,
                                state_hash: state_hash(&state)?,
                                source_pid: std::process::id(),
                                state,
                            };
                            let temporary = base.join("state.pending");
                            let mut file = OpenOptions::new()
                                .create_new(true)
                                .write(true)
                                .open(&temporary)?;
                            use std::io::Write;
                            file.write_all(&serde_json::to_vec(&saved)?)?;
                            file.sync_all()?;
                            std::fs::rename(temporary, base.join("state.json"))?;
                            File::open(&base)?.sync_all()?;
                            println!("linux-save-runner-exiting pid={}", std::process::id());
                            std::process::exit(0);
                        })();
                        capture.map_err(|e| format!("{e:#}"))
                    })
                    .map_err(anyhow::Error::msg)
            })();
            if let Err(error) = result {
                eprintln!("linux-cold-restore-error: {error:#}");
                std::process::exit(1);
            }
        });
        Ok(())
    });
    check(rc)
}
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn main() {
    eprintln!("requires macOS aarch64");
}
