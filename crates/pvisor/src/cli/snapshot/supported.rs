use super::{Args, Command, RamStorage};
use crate::environment_snapshot::{
    BaseReference, Compatibility, SnapshotRamMount, SnapshotStore, file_hash,
};
use anyhow::{Context, ensure};
use pvisor_guest::GuestConfig;
use pvisor_vm::api::{MachineRestore, MachineSnapshot, OverlayConfig, PermissionSemantics};
use pvisor_vm::api::{RuntimeSupport, VmConfiguration, VmRuntime};
use pvisor_vm::api::{SnapshotCapture, SnapshotControl, SnapshotState, VmControl};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// Keep IPC cleanup alive across _exit and SIGKILL of the whole VM group.
/// Install before binding so no kill window leaves a socket without an owner.
struct SocketWatchdog(std::process::Child, Option<std::process::ChildStdin>);
impl SocketWatchdog {
    fn start(directory: &Path) -> anyhow::Result<Self> {
        use std::os::unix::process::CommandExt;
        use std::process::Stdio;
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .args(["snapshot", "socket-watchdog"])
            .arg(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0)
            .spawn()?;
        let pipe = child.stdin.take().context("missing socket watchdog pipe")?;
        Ok(Self(child, Some(pipe)))
    }
}
impl Drop for SocketWatchdog {
    fn drop(&mut self) {
        drop(self.1.take());
        if let Err(error) = self.0.wait() {
            tracing::warn!(%error, "cannot wait for snapshot IPC cleanup");
        }
    }
}
fn watch_socket(directory: &Path) -> anyhow::Result<()> {
    ensure!(directory.is_absolute(), "invalid socket watchdog directory");
    let socket = control_socket(directory)?;
    std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink())?;
    match fs::symlink_metadata(&socket) {
        Ok(meta) => {
            ensure!(
                meta.file_type().is_socket(),
                "invalid snapshot control socket"
            );
            match fs::remove_file(socket) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

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
    #[serde(default)]
    base: Option<BaseReference>,
    #[serde(default)]
    eager_ram: bool,
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
const FULL_PROFILE: &str = "pvisor-cli-full-copy-v1";
const STAGE_PROFILE: &str = "pvisor-cli-stage-v1";

fn compatibility(firmware: &Path, profile: &str) -> anyhow::Result<Compatibility> {
    #[cfg(all(target_os = "linux", target_env = "musl", target_arch = "x86_64"))]
    let firmware_hash = {
        let _ = firmware;
        let kernel =
            pvisor_vm::api::VmPlatform::embedded_kernel().context("static VM kernel is missing")?;
        Sha256::digest(&kernel.bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    };
    #[cfg(not(all(target_os = "linux", target_env = "musl", target_arch = "x86_64")))]
    let firmware_hash = file_hash(
        &firmware
            .join(pvisor_vm::api::VmPlatform::firmware_name())
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
        profile: profile.into(),
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
        if let Some(directory) = pvisor_vm::api::VmPlatform::bundled_firmware_directory() {
            return Ok(directory);
        }
        Ok(pvisor_vm::api::VmPlatform::prepare_firmware(None)?)
    }
}
fn launch(spec: Launch) -> anyhow::Result<()> {
    // Restore payload validation belongs to the runner. Keep the launch spec
    // outside the final instance until that validation succeeds. Reuse the
    // pending writer lease so GC can reap a launch interrupted by SIGKILL.
    let pending = if spec.restore.is_some() {
        Some(SnapshotStore::new(&spec.store)?.begin()?)
    } else {
        None
    };
    let path = pending
        .as_ref()
        .map(|pending| pending.directory().join("launch.json"))
        .unwrap_or_else(|| spec.directory.join("launch.json"));
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
    match fs::remove_file(socket) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    ensure!(status.success(), "snapshot VM exited with {status}");
    Ok(())
}
pub(super) fn run(args: Args) -> anyhow::Result<()> {
    if let Command::RamWatchdog { mount } = &args.command {
        return crate::environment_snapshot::watch_mount(mount);
    }
    if let Command::SocketWatchdog { directory } = &args.command {
        return watch_socket(directory);
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
            base,
            cpus,
            memory,
            ram_storage,
            native_init,
            command,
        } => {
            let base = match (rootfs, base) {
                (Some(source), None) => store.import_base(&source)?,
                (None, Some(id)) => store.open_base(&BaseReference { id })?,
                _ => anyhow::bail!("specify exactly one of --rootfs or --base"),
            };
            let guest = if native_init {
                ensure!(
                    base.root().join("init.krun").is_file(),
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
            let stage = directory.join("rootfs");
            fs::create_dir(&stage)?;
            for part in ["upper", "work", "preimages"] {
                fs::create_dir(stage.join(part))?;
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
                base: Some(base.reference().clone()),
                eager_ram: false,
            })
        }
        Command::ImportBase { rootfs } => {
            println!("{}", store.import_base(&rootfs)?.reference().id);
            Ok(())
        }
        Command::VerifyBase { id } => store.open_base(&BaseReference { id })?.verify(),
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
        Command::Restore {
            id,
            name,
            eager_ram,
        } => {
            let firmware = firmware_directory()?;
            // Authenticate startup metadata only. The runner validates the
            // payload under its own lease before allocating an instance.
            let profile = store.profile(&id)?;
            ensure!(
                profile == FULL_PROFILE || profile == STAGE_PROFILE,
                "unsupported CLI snapshot profile"
            );
            let binding = compatibility(&firmware, &profile)?;
            let (manifest, machine) = store.restore_metadata(&id, &binding)?;
            let base = manifest
                .stage_bases
                .as_ref()
                .map(|bases| {
                    ensure!(bases.len() == 1, "CLI stage requires exactly one base");
                    Ok(bases[0].clone())
                })
                .transpose()?;
            let saved: Saved = serde_json::from_slice(&machine)?;
            ensure!(
                saved.exclusions == exclusions(),
                "unsupported resource contract mismatch"
            );
            let directory = instance(&root, &name)?;
            launch(Launch {
                store: root,
                directory,
                firmware,
                restore: Some(id),
                cpus: saved.cpus,
                memory: saved.memory,
                ram_storage: saved.ram_storage,
                guest: saved.guest,
                base,
                eager_ram,
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
        Command::Runner { .. } | Command::RamWatchdog { .. } | Command::SocketWatchdog { .. } => {
            unreachable!()
        }
    }
}

fn runner(spec: Launch) -> anyhow::Result<()> {
    let store = SnapshotStore::new(&spec.store)?;
    let binding = compatibility(
        &spec.firmware,
        if spec.base.is_some() {
            STAGE_PROFILE
        } else {
            FULL_PROFILE
        },
    )?;
    let published = spec
        .restore
        .as_deref()
        .map(|id| {
            if spec.base.is_some() {
                store.open_owned_stage_for_restore(id, &binding)
            } else {
                store.open_for_restore(id, &binding)
            }
        })
        .transpose()?;
    let saved = published
        .as_ref()
        .map(|snapshot| -> anyhow::Result<Saved> {
            ensure!(
                snapshot.manifest().stage_bases.as_deref()
                    == spec.base.as_ref().map(std::slice::from_ref),
                "snapshot base binding mismatch"
            );
            let saved: Saved = serde_json::from_slice(&snapshot.machine_bytes()?)?;
            ensure!(
                saved.cpus == spec.cpus
                    && saved.memory == spec.memory
                    && saved.ram_storage == spec.ram_storage
                    && serde_json::to_value(&saved.guest)? == serde_json::to_value(&spec.guest)?
                    && saved.exclusions == exclusions(),
                "snapshot launch contract mismatch"
            );
            Ok(saved)
        })
        .transpose()?;
    if spec.restore.is_some() {
        let name = spec
            .directory
            .file_name()
            .and_then(OsStr::to_str)
            .context("invalid restore instance name")?;
        ensure!(
            instance(&spec.store, name)? == spec.directory,
            "invalid restore instance path"
        );
        new_instance(&spec.store, name)?;
        fs::write(
            spec.directory.join("launch.json"),
            serde_json::to_vec(&spec)?,
        )?;
    }
    let lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(spec.directory.join("execution.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lease).context("instance already running")?;
    let root = spec.directory.join("rootfs");
    let bases = if let Some(snapshot) = &published {
        if spec.base.is_some() {
            snapshot.materialize_owned_stage(&root)?;
        } else {
            snapshot.materialize(&root)?;
        }
        snapshot.base_leases()?
    } else {
        spec.base
            .as_ref()
            .map(|reference| store.open_base(reference))
            .transpose()?
            .into_iter()
            .collect::<Vec<_>>()
    };
    // Retain the mount across the entire blocking VMM call, including after
    // the guest-ready callback drops the published object's store-wide gate.
    let mut ram_mount = None;
    let restore = if let Some(snapshot) = &published {
        let mut saved = saved.context("missing checked snapshot state")?;
        let old = Path::new(OsStr::from_bytes(&snapshot.manifest().source_root));
        let count = if bases.is_empty() {
            saved
                .state
                .rebind_filesystem_copy("/dev/root", old, &root)?
        } else {
            let copies =
                ["upper", "work", "preimages"].map(|part| (old.join(part), root.join(part)));
            saved.state.rebind_filesystem_stage(
                "/dev/root",
                &copies,
                &bases.iter().map(|base| base.root()).collect::<Vec<_>>(),
            )?
        };
        ensure!(count == 1, "snapshot requires one root filesystem");
        let ram_file = if spec.eager_ram {
            snapshot
                .ram_file()
                .context("verify/materialize snapshot RAM")?
        } else {
            let (mut mount, ram_file) =
                SnapshotRamMount::new(snapshot.ram_reader()?, &spec.directory)
                    .context("mount on-demand snapshot RAM")?;
            mount.watch_runner_exit(&std::env::current_exe()?)?;
            ram_mount = Some(mount);
            ram_file
        };
        Some(MachineRestore {
            state: saved.state,
            ram_file: Arc::new(ram_file),
        })
    } else {
        None
    };
    let socket = control_socket(&spec.directory)?;
    let _socket_watchdog = SocketWatchdog::start(&spec.directory)?;
    // Instance names are single-use; no stale socket is removed or rebound.
    let listener = UnixListener::bind(&socket)?;
    pvisor_vm::api::VmPlatform::init_logging("error");
    let mut vm = pvisor_vm::api::VmBuilder::new(spec.cpus, spec.memory)?;
    vm.snapshot_profile()?;
    if spec.guest.is_none() {
        vm.disable_implicit_init()?;
    }
    if bases.is_empty() {
        vm.filesystem("/dev/root", &root, 0)?;
    } else {
        let baseline_content_index = bases.last().and_then(|base| {
            base.content_index()
                .map(|(file, sha256)| pvisor_vm::api::BaselineContentIndex {
                    root: base.root(),
                    file,
                    sha256,
                })
        });
        vm.overlay(
            "/dev/root",
            OverlayConfig {
                lower_dirs: bases.iter().map(|base| base.root()).collect(),
                upper_dir: root.join("upper"),
                work_dir: Some(root.join("work")),
                preimage_dir: Some(root.join("preimages")),
                apply_target: None,
                baseline_lower: None,
                baseline_content_index,
                excluded_paths: Vec::new(),
                access_policy: Default::default(),
                semantics: PermissionSemantics::LinuxComplete,
            },
            0,
        )?;
    }
    if let Some(guest) = &spec.guest {
        vm.virtual_file(
            "/dev/root",
            "/.pvisor-guest.json",
            serde_json::to_vec(guest)?,
            0o400,
            true,
        )?;
    }
    if let Some(state) = restore {
        vm.machine_restore(state)?;
    }
    let result = vm.run(move |handle| {
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
                            let pending = store.begin().context("begin snapshot staging")?;
                            let ram = pending.create_ram().context("create capture RAM")?;
                            let state = vm
                                .capture_machine_state(&ram)
                                .map_err(anyhow::Error::msg)
                                .context("capture complete VM state")?;
                            let saved = Saved {
                                cpus: spec.cpus,
                                memory: spec.memory,
                                ram_storage: spec.ram_storage,
                                guest: spec.guest.clone(),
                                exclusions: exclusions(),
                                state,
                            };
                            let machine = serde_json::to_vec(&saved)?;
                            if !bases.is_empty() {
                                return pending
                                    .publish_stage(
                                        &root,
                                        &bases,
                                        &machine,
                                        binding.clone(),
                                        spec.ram_storage == RamStorage::Compressed,
                                    )
                                    .context("publish frozen stage and RAM");
                            }
                            match spec.ram_storage {
                                RamStorage::Raw => {
                                    pending.publish(&root, &machine, binding.clone())
                                }
                                RamStorage::Compressed => {
                                    pending.publish_compressed(&root, &machine, binding.clone())
                                }
                            }
                            .context("publish frozen filesystem and RAM")
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
    });
    drop(ram_mount);
    result.map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_payload_validation_precedes_instance_creation() {
        let temp = tempfile::tempdir().unwrap();
        let firmware = temp.path().join("firmware");
        fs::create_dir(&firmware).unwrap();
        fs::write(
            firmware.join(pvisor_vm::api::VmPlatform::firmware_name()),
            b"fixture",
        )
        .unwrap();
        let binding = compatibility(&firmware, STAGE_PROFILE).unwrap();
        let root = temp.path().join("store");
        let store = SnapshotStore::new(&root).unwrap();
        let base_source = temp.path().join("base");
        fs::create_dir(&base_source).unwrap();
        let base = store.import_base(&base_source).unwrap();
        let base_reference = base.reference().clone();
        let stage = temp.path().join("stage");
        for part in ["upper", "work", "preimages"] {
            fs::create_dir_all(stage.join(part)).unwrap();
        }
        let pending = store.begin().unwrap();
        pending.create_ram().unwrap().write_all(b"RAM").unwrap();
        let id = pending
            .publish_stage(&stage, &[base], b"unparsed-machine", binding.clone(), false)
            .unwrap();
        fs::write(
            root.join("objects").join(&id).join("rootfs/upper/extra"),
            b"unexpected",
        )
        .unwrap();
        // Preflight is deliberately not a payload-validation capability.
        assert!(store.restore_metadata(&id, &binding).is_ok());
        let directory = instance(&root, "rejected").unwrap();
        let error = runner(Launch {
            store: root,
            directory: directory.clone(),
            firmware,
            restore: Some(id),
            cpus: 2,
            memory: 256,
            ram_storage: RamStorage::Raw,
            guest: None,
            base: Some(base_reference),
            eager_ram: true,
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("unexpected file in cloned tree"),
            "{error:#}"
        );
        assert!(!directory.exists());
    }
}
