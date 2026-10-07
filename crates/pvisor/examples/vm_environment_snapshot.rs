//! Independent, full-copy Linux environment snapshots on macOS HVF.
//! Internal executor integration; not a product CLI or cross-host migration.
#![cfg_attr(not(all(target_os = "macos", target_arch = "aarch64")), allow(unused))]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn main() -> anyhow::Result<()> {
    use pvisor_vm::api::{RuntimeSupport, VmConfiguration, VmRuntime};
    use pvisor_vm::api::{SnapshotCapture, SnapshotControl, SnapshotState, VmControl};

    use anyhow::{Context, ensure};
    use pvisor::environment_snapshot::{Compatibility, SnapshotStore, file_hash};
    use pvisor_vm::api::{MachineRestore, MachineSnapshot};
    use serde::{Deserialize, Serialize};
    use std::{
        ffi::OsStr,
        fs::{File, OpenOptions},
        os::unix::ffi::OsStrExt,
        path::PathBuf,
        sync::Arc,
        time::{Duration, Instant},
    };
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Machine {
        source_pid: u32,
        // Capturing the full actual bus vetoes unsupported devices/resources.
        excluded_resources: Vec<String>,
        state: MachineSnapshot,
    }
    fn exclusions() -> Vec<String> {
        [
            "active external connections/listeners",
            "DAX/writeback",
            "unlinked-open files",
            "nested virtualization",
            "active cold RAM pager",
            "external host writers",
            "special files or external hardlinks",
            "cross-host/cross-boot/cross-build restore",
        ]
        .map(str::to_owned)
        .into()
    }
    let mode = std::env::args()
        .nth(1)
        .context("save, restore or delete required")?;
    ensure!(
        ["save", "restore", "delete"].contains(&mode.as_str()),
        "invalid mode"
    );
    let base = PathBuf::from(
        std::env::args()
            .nth(2)
            .context("environment directory required")?,
    )
    .canonicalize()?;
    let store = SnapshotStore::new(&base.join("store"))?;
    if mode == "delete" {
        store.delete(std::fs::read_to_string(base.join("snapshot.id"))?.trim())?;
        return Ok(());
    }
    let lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(base.join("execution.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lease).context("another runner owns this environment")?;
    let output = std::process::Command::new("sysctl")
        .args(["-n", "kern.bootsessionuuid"])
        .output()?;
    ensure!(output.status.success(), "host boot identity unavailable");
    let boot = String::from_utf8(output.stdout)?.trim().to_owned();
    let directory = std::env::var_os("PVISOR_CASE_VM_LIBRARY_DIR").map(PathBuf::from);
    let firmware = pvisor_vm::api::VmPlatform::resolve_firmware_path(directory.as_deref())?;
    let compatibility = Compatibility {
        host_boot: boot,
        build: file_hash(&std::env::current_exe()?)?,
        firmware: file_hash(&firmware)?,
        profile: "macos-hvf-full-copy-v1-software-gic-2cpu-256m".into(),
    };
    let published = if mode == "restore" {
        Some(store.open(
            std::fs::read_to_string(base.join("snapshot.id"))?.trim(),
            &compatibility,
        )?)
    } else {
        None
    };
    // Private destination persists for the experiment's host-side assertions.
    // A failed attempt never reuses its tree; callers remove it before retry.
    let root = if let Some(snapshot) = &published {
        let root = base.join("restored-rootfs");
        snapshot.materialize(&root)?;
        root.canonicalize()?
    } else {
        base.join("rootfs").canonicalize()?
    };
    let restored = if let Some(snapshot) = &published {
        let mut saved: Machine = serde_json::from_slice(&snapshot.machine_bytes()?)?;
        ensure!(
            saved.source_pid != std::process::id(),
            "source runner was reused"
        );
        ensure!(
            saved.excluded_resources == exclusions(),
            "unsupported resource contract mismatch"
        );
        let old = std::path::Path::new(OsStr::from_bytes(&snapshot.manifest().source_root));
        let bindings = saved
            .state
            .rebind_filesystem_copy("/dev/root", old, &root)?;
        ensure!(
            bindings == 1,
            "environment requires exactly one root filesystem binding"
        );
        Some(MachineRestore {
            state: saved.state,
            ram_file: Arc::new(snapshot.ram_file()?),
        })
    } else {
        None
    };
    pvisor_vm::api::VmPlatform::init_logging("trace");
    let mut vm = pvisor_vm::api::VmBuilder::new(2, 256)?;
    vm.set_firmware_path(firmware)?;
    vm.snapshot_profile()?;
    vm.disable_implicit_init()?;
    vm.filesystem("/dev/root", &root, 0)?;
    if let Some(restore) = restored {
        vm.machine_restore(restore)?;
    }
    let result = vm.run(move |handle| {
        std::thread::spawn(move || {
            // Keep the reference alive until fresh VM RAM/devices are installed.
            let _published = published;
            let result = (|| -> anyhow::Result<()> {
                if mode == "restore" {
                    ensure!(
                        handle.is_paused().map_err(anyhow::Error::msg)?,
                        "restore executed before installation completed"
                    );
                    handle.resume().map_err(anyhow::Error::msg)?;
                    println!("environment-restore-ready pid={}", std::process::id());
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
                        let result = (|| -> anyhow::Result<()> {
                            let pending = store.begin()?;
                            let ram = pending.create_ram()?;
                            let state =
                                vm.capture_machine_state(&ram).map_err(anyhow::Error::msg)?;
                            let machine = Machine {
                                source_pid: std::process::id(),
                                excluded_resources: exclusions(),
                                state,
                            };
                            // Filesystem copy and publication are inside the SAME
                            // CPU/device/RAM freeze as machine-state capture.
                            let id = pending.publish(
                                &root,
                                &serde_json::to_vec(&machine)?,
                                compatibility,
                            )?;
                            let mut marker = tempfile::NamedTempFile::new_in(&base)?;
                            use std::io::Write;
                            marker.write_all(id.as_bytes())?;
                            marker.as_file().sync_all()?;
                            marker.persist_noclobber(base.join("snapshot.id"))?;
                            File::open(&base)?.sync_all()?;
                            println!(
                                "environment-save-exiting pid={} snapshot={id}",
                                std::process::id()
                            );
                            std::process::exit(0);
                        })();
                        result.map_err(|error| format!("{error:#}"))
                    })
                    .map_err(anyhow::Error::msg)
            })();
            if let Err(error) = result {
                eprintln!("environment-snapshot-error: {error:#}");
                std::process::exit(1);
            }
        });
        Ok(())
    });
    result.map_err(Into::into)
}
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
fn main() {
    eprintln!("requires macOS aarch64");
}
