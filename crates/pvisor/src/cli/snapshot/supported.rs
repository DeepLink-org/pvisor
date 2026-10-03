use super::{Args, Command, RamStorage};
use crate::environment_snapshot::{
    Compatibility, SnapshotRamMount, SnapshotStore, copy_owned_tree, file_hash,
};
use anyhow::{Context, ensure};
use devices::snapshot::BusDeviceSnapshot;
use krun_vmm::snapshot::{MachineRestore, MachineSnapshot};
use pvisor_guest::GuestConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::{CString, OsStr},
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Launch {
    store: PathBuf,
    directory: PathBuf,
    firmware: PathBuf,
    restore: Option<String>,
    cpus: u8,
    memory: u32,
    #[serde(default)]
    ram_storage: RamStorage,
    guest: Option<GuestConfig>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    cpus: u8,
    memory: u32,
    #[serde(default)]
    ram_storage: RamStorage,
    guest: Option<GuestConfig>,
    exclusions: Vec<String>,
    state: MachineSnapshot,
}
fn exclusions() -> Vec<String> {
    [
        "active connections/listeners",
        "DAX/writeback",
        "unlink-open",
        "nested virtualization",
        "active RAM pager",
        "external host writers",
        "special files/external hardlinks",
        "cross-host/boot/build",
    ]
    .map(str::to_owned)
    .into()
}
fn check(value: i32) -> anyhow::Result<()> {
    ensure!(value >= 0, "libkrun error {value}");
    Ok(())
}
fn store_path(value: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let path = value.unwrap_or(
        dirs::data_local_dir()
            .context("user state directory unavailable")?
            .join("pvisor/snapshots"),
    );
    fs::create_dir_all(&path)?;
    Ok(path.canonicalize()?)
}
fn instance(root: &Path, name: &str) -> anyhow::Result<PathBuf> {
    ensure!(
        !name.is_empty()
            && name.len() <= 48
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "instance name must contain 1–48 ASCII letters, numbers, '-' or '_'"
    );
    Ok(root.join("runs").join(name))
}
fn new_instance(root: &Path, name: &str) -> anyhow::Result<PathBuf> {
    fs::create_dir_all(root.join("runs"))?;
    let path = instance(root, name)?;
    fs::create_dir(&path).context("instance name already exists; use a new name")?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}
fn control_socket(directory: &Path) -> anyhow::Result<PathBuf> {
    // macOS sockaddr_un has a short path limit. Keep IPC outside long user
    // store paths, inside a stable private directory owned by this UID.
    let parent = PathBuf::from(format!("/tmp/pvisor-snapshots-{}", unsafe {
        libc::geteuid()
    }));
    match fs::DirBuilder::new().mode(0o700).create(&parent) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let meta = fs::symlink_metadata(&parent)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "snapshot IPC directory has unsafe ownership or permissions"
    );
    let digest = Sha256::digest(directory.as_os_str().as_bytes());
    let name: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(parent.join(format!("{name}.sock")))
}
fn compatibility(firmware: &Path) -> anyhow::Result<Compatibility> {
    #[cfg(all(target_os = "linux", target_env = "musl", target_arch = "x86_64"))]
    let firmware_hash = {
        use crate::executor::vm::embedded_kernel;
        let _ = firmware;
        Sha256::digest(embedded_kernel::KERNEL.as_slice())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    };
    #[cfg(not(all(target_os = "linux", target_env = "musl", target_arch = "x86_64")))]
    let firmware_hash = file_hash(
        &firmware
            .join(crate::executor::vm::firmware_name())
            .canonicalize()?,
    )?;
    #[cfg(target_os = "macos")]
    let host_boot = {
        let output = std::process::Command::new("sysctl")
            .args(["-n", "kern.bootsessionuuid"])
            .output()?;
        ensure!(output.status.success(), "host boot identity unavailable");
        String::from_utf8(output.stdout)?
    };
    #[cfg(target_os = "linux")]
    let host_boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    ensure!(
        !host_boot.trim().is_empty(),
        "host boot identity unavailable"
    );
    Ok(Compatibility {
        host_boot: host_boot.trim().into(),
        build: file_hash(&std::env::current_exe()?)?,
        firmware: firmware_hash,
        profile: "pvisor-cli-full-copy-v1".into(),
    })
}
fn firmware_directory() -> anyhow::Result<PathBuf> {
    #[cfg(all(target_os = "linux", target_env = "musl", target_arch = "x86_64"))]
    {
        // Static runners use the shared embedded kernel module, with no loader path.
        Ok(PathBuf::new())
    }
    #[cfg(not(all(target_os = "linux", target_env = "musl", target_arch = "x86_64")))]
    {
        if let Some(directory) = crate::executor::vm::bundled_firmware_dir() {
            return Ok(directory);
        }
        crate::executor::vm::firmware::FirmwareStore::new()?.prepare()
    }
}
fn launch(spec: Launch) -> anyhow::Result<()> {
    let path = spec.directory.join("launch.json");
    fs::write(&path, serde_json::to_vec(&spec)?)?;
    let status = std::process::Command::new(std::env::current_exe()?)
        .args(["snapshot", "runner"])
        .arg(&path)
        .env(
            if cfg!(target_os = "macos") {
                "DYLD_LIBRARY_PATH"
            } else {
                "LD_LIBRARY_PATH"
            },
            &spec.firmware,
        )
        .status()?;
    let socket = control_socket(&spec.directory)?;
    if socket.exists() {
        fs::remove_file(socket)?;
    }
    ensure!(status.success(), "snapshot VM exited with {status}");
    Ok(())
}
pub(super) fn run(args: Args) -> anyhow::Result<()> {
    if let Command::RamWatchdog { mount } = &args.command {
        return crate::environment_snapshot::watch_mount(mount);
    }
    if let Command::Runner { spec } = &args.command {
        return runner(serde_json::from_slice(&fs::read(spec)?)?);
    }
    let root = store_path(args.store)?;
    let store = SnapshotStore::new(&root)?;
    match args.command {
        Command::Run {
            name,
            rootfs,
            cpus,
            memory,
            ram_storage,
            native_init,
            command,
        } => {
            let guest = if native_init {
                ensure!(
                    rootfs.join("init.krun").is_file(),
                    "native rootfs requires /init.krun"
                );
                None
            } else {
                let config = GuestConfig {
                    argv: command,
                    cwd: "/".into(),
                    env: [(
                        "PATH".into(),
                        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into(),
                    )]
                    .into(),
                    ..Default::default()
                };
                config.command()?;
                Some(config)
            };
            let firmware = firmware_directory()?;
            let directory = new_instance(&root, &name)?;
            if let Err(error) = copy_owned_tree(&rootfs, &directory.join("rootfs")) {
                fs::remove_dir_all(&directory)?;
                return Err(error);
            }
            launch(Launch {
                store: root,
                directory,
                firmware,
                restore: None,
                cpus,
                memory,
                ram_storage,
                guest,
            })
        }
        Command::Save { name } => {
            let directory = instance(&root, &name)?;
            let mut stream = UnixStream::connect(control_socket(&directory)?)
                .context("snapshot-capable VM is not running")?;
            stream.set_read_timeout(Some(Duration::from_secs(180)))?;
            stream.write_all(b"save\n")?;
            let mut bytes = Vec::new();
            stream.take(8192).read_to_end(&mut bytes)?;
            let response: Result<String, String> = serde_json::from_slice(&bytes)?;
            println!("{}", response.map_err(anyhow::Error::msg)?);
            Ok(())
        }
        Command::Restore { id, name } => {
            let firmware = firmware_directory()?;
            // Validate before allocating a new instance directory.
            let published = store.open_for_restore(&id, &compatibility(&firmware)?)?;
            let saved: Saved = serde_json::from_slice(&published.machine_bytes()?)?;
            ensure!(
                saved.exclusions == exclusions(),
                "unsupported resource contract mismatch"
            );
            let directory = new_instance(&root, &name)?;
            drop(published);
            launch(Launch {
                store: root,
                directory,
                firmware,
                restore: Some(id),
                cpus: saved.cpus,
                memory: saved.memory,
                ram_storage: saved.ram_storage,
                guest: saved.guest,
            })
        }
        Command::List => {
            let mut ids = fs::read_dir(root.join("objects"))?
                .map(|entry| entry.map(|e| e.file_name()))
                .collect::<std::io::Result<Vec<_>>>()?;
            ids.sort();
            for id in ids {
                println!("{}", id.to_string_lossy());
            }
            Ok(())
        }
        Command::Delete { id } => store.delete(&id),
        Command::Gc => {
            println!("{}", store.collect_abandoned()?);
            Ok(())
        }
        Command::Runner { .. } | Command::RamWatchdog { .. } => unreachable!(),
    }
}

fn runner(spec: Launch) -> anyhow::Result<()> {
    let lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(spec.directory.join("execution.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lease).context("instance already running")?;
    let store = SnapshotStore::new(&spec.store)?;
    let binding = compatibility(&spec.firmware)?;
    let published = spec
        .restore
        .as_deref()
        .map(|id| store.open_for_restore(id, &binding))
        .transpose()?;
    let root = spec.directory.join("rootfs");
    if let Some(snapshot) = &published {
        snapshot.materialize(&root)?;
    }
    // Retain the mount across the entire blocking VMM call, including after
    // the guest-ready callback drops the published object's store-wide gate.
    let mut ram_mount = None;
    let restore = if let Some(snapshot) = &published {
        let mut saved: Saved = serde_json::from_slice(&snapshot.machine_bytes()?)?;
        ensure!(
            saved.cpus == spec.cpus
                && saved.memory == spec.memory
                && saved.ram_storage == spec.ram_storage
                && saved.exclusions == exclusions(),
            "snapshot launch contract mismatch"
        );
        let old = Path::new(OsStr::from_bytes(&snapshot.manifest().source_root));
        let mut tag = [0; 36];
        tag[..9].copy_from_slice(b"/dev/root");
        let mut count = 0;
        for mapping in &mut saved.state.devices {
            if let BusDeviceSnapshot::Virtio(device) = &mut mapping.device {
                count += usize::from(device.rebind_filesystem_copy(&tag, old, &root)?);
            }
        }
        ensure!(count == 1, "snapshot requires one root filesystem");
        let (mut mount, ram_file) = SnapshotRamMount::new(snapshot.ram_reader()?, &spec.directory)
            .context("mount on-demand snapshot RAM")?;
        mount.watch_runner_exit(&std::env::current_exe()?)?;
        ram_mount = Some(mount);
        Some(MachineRestore {
            state: saved.state,
            ram_file: Arc::new(ram_file),
        })
    } else {
        None
    };
    let socket = control_socket(&spec.directory)?;
    // Instance names are single-use; no stale socket is removed or rebound.
    let listener = UnixListener::bind(&socket)?;
    check(krun::krun_set_log_level(1))?;
    let ctx = krun::krun_create_ctx();
    check(ctx)?;
    let ctx = ctx as u32;
    check(krun::krun_set_vm_config(ctx, spec.cpus, spec.memory))?;
    #[cfg(all(target_os = "linux", target_env = "musl", target_arch = "x86_64"))]
    {
        use crate::executor::vm::embedded_kernel;
        check(unsafe {
            krun::krun_set_embedded_kernel(
                ctx,
                embedded_kernel::KERNEL.as_ptr(),
                embedded_kernel::KERNEL.len(),
                embedded_kernel::GUEST_ADDR,
                embedded_kernel::ENTRY_ADDR,
            )
        })?;
    }
    check(krun::krun_set_snapshot_profile(ctx))?;
    if spec.guest.is_none() {
        check(krun::krun_disable_implicit_init(ctx))?;
    }
    check(krun::krun_disable_implicit_vsock(ctx))?;
    check(krun::krun_add_vsock(ctx, 0))?;
    let native = CString::new(root.as_os_str().as_bytes())?;
    check(unsafe { krun::krun_add_virtiofs2(ctx, c"/dev/root".as_ptr(), native.as_ptr(), 0) })?;
    // libkrun borrows virtual file bytes for the VM lifetime.
    let guest_bytes = spec.guest.as_ref().map(serde_json::to_vec).transpose()?;
    if let Some(bytes) = &guest_bytes {
        check(unsafe {
            krun::krun_fs_add_overlay_file(
                ctx,
                c"/dev/root".as_ptr(),
                c"/.pvisor-guest.json".as_ptr(),
                bytes.as_ptr(),
                bytes.len(),
                0o400,
                true,
            )
        })?;
    }
    if let Some(state) = restore {
        krun::krun_set_machine_restore(ctx, state).map_err(anyhow::Error::msg)?;
    }
    let result = check(krun::krun_start_enter_with_handle(ctx, move |handle| {
        // Preparation is complete and the pager owns its independent pins.
        // Release the store gate before the first resumed guest heartbeat.
        drop(published);
        if spec.restore.is_some() {
            handle.resume().map_err(std::io::Error::other)?;
        }
        println!("snapshot VM ready: {}", spec.directory.display());
        std::thread::spawn(move || {
            for connection in listener.incoming() {
                let Ok(mut stream) = connection else {
                    break;
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut command = [0; 5];
                if stream.read_exact(&mut command).is_err() || &command != b"save\n" {
                    continue;
                }
                let result: Result<(), String> =
                    handle.with_snapshot_quiesced(Duration::from_secs(30), |vm| {
                        let result = (|| -> anyhow::Result<String> {
                            let pending = store.begin()?;
                            let ram = pending.create_ram()?;
                            let state =
                                vm.capture_machine_state(&ram).map_err(anyhow::Error::msg)?;
                            let saved = Saved {
                                cpus: spec.cpus,
                                memory: spec.memory,
                                ram_storage: spec.ram_storage,
                                guest: spec.guest.clone(),
                                exclusions: exclusions(),
                                state,
                            };
                            let machine = serde_json::to_vec(&saved)?;
                            match spec.ram_storage {
                                RamStorage::Raw => {
                                    pending.publish(&root, &machine, binding.clone())
                                }
                                RamStorage::Compressed => {
                                    pending.publish_compressed(&root, &machine, binding.clone())
                                }
                            }
                        })();
                        match result {
                            Ok(id) => {
                                // Reply and exit while STILL frozen. Never resume
                                // the source after publishing a cold snapshot.
                                let bytes = serde_json::to_vec(&Ok::<_, String>(id))
                                    .expect("snapshot identity is serializable");
                                if let Err(error) = stream.write_all(&bytes) {
                                    eprintln!("snapshot published but reply failed: {error}");
                                }
                                std::process::exit(0);
                            }
                            Err(error) => Err(format!("{error:#}")),
                        }
                    });
                if let Err(error) = result {
                    let _ =
                        stream.write_all(&serde_json::to_vec(&Err::<String, _>(error)).unwrap());
                }
            }
        });
        Ok(())
    }));
    drop(ram_mount);
    result
}
