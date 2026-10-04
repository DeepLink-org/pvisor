//! One freeze transaction spans native capture and host-side publication.
//! The host seals filesystem inventories outside the runner's user namespace.
use crate::environment_snapshot::{Compatibility, SnapshotStore, file_hash};
use anyhow::{Context, ensure};
use pvisor_core::operation::{ExecutionCheckpoint, OperationKind, SnapshotRamStorage};
use pvisor_vm::api::{RuntimeSupport, SnapshotCapture, SnapshotState};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LaunchBinding {
    pub store: PathBuf,
    pub compatibility: Compatibility,
    pub run_id: String,
    pub attempt_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaptureRequest {
    pub operation: OperationKind,
    pub directory: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaptureReady {
    pub directory: PathBuf,
    pub run_id: String,
    pub attempt_id: String,
    pub created_at_unix_ms: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CommitReply {
    pub checkpoint: Option<ExecutionCheckpoint>,
    pub error: Option<String>,
}

pub(super) fn binding(
    store: PathBuf,
    run_id: String,
    attempt_id: String,
    firmware: Option<&Path>,
) -> anyhow::Result<LaunchBinding> {
    crate::util::create_dir_all_durable(&store)?;
    let store = store.canonicalize()?;
    SnapshotStore::new(&store)?;
    crate::util::create_dir_all_durable(&store.join("captures"))?;
    #[cfg(all(target_os = "linux", target_env = "musl", target_arch = "x86_64"))]
    let firmware_hash = {
        use sha2::{Digest, Sha256};
        let _ = firmware;
        let kernel =
            pvisor_vm::api::VmPlatform::embedded_kernel().context("static VM kernel is missing")?;
        crate::util::encode_hex(&Sha256::digest(&kernel.bytes))
    };
    #[cfg(not(all(target_os = "linux", target_env = "musl", target_arch = "x86_64")))]
    let firmware_hash = file_hash(
        &firmware
            .context("checkpoint capture requires a bound firmware directory")?
            .join(pvisor_vm::api::VmPlatform::firmware_name())
            .canonicalize()?,
    )?;
    #[cfg(target_os = "linux")]
    let host_boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    #[cfg(target_os = "macos")]
    let host_boot = {
        let output = std::process::Command::new("sysctl")
            .args(["-n", "kern.bootsessionuuid"])
            .output()?;
        ensure!(output.status.success(), "host boot identity unavailable");
        String::from_utf8(output.stdout)?
    };
    ensure!(
        !host_boot.trim().is_empty(),
        "host boot identity unavailable"
    );
    Ok(LaunchBinding {
        store,
        compatibility: Compatibility {
            host_boot: host_boot.trim().into(),
            build: file_hash(&std::env::current_exe()?)?,
            firmware: firmware_hash,
            profile: "pvisor-job-owned-overlay-v1".into(),
        },
        run_id,
        attempt_id,
    })
}

/// The runner remains frozen until the caller sends CommitReply and consumes
/// the final acknowledgement. Publication errors must also receive that reply.
pub(super) fn publish(
    binding: &LaunchBinding,
    ready: CaptureReady,
    directory: &Path,
    storage: SnapshotRamStorage,
) -> anyhow::Result<ExecutionCheckpoint> {
    ensure!(
        ready.directory == directory
            && ready.run_id == binding.run_id
            && ready.attempt_id == binding.attempt_id
            && ready.created_at_unix_ms > 0,
        "native checkpoint capture binding mismatch"
    );
    let machine_path = directory.join("machine.json");
    ensure!(
        fs::symlink_metadata(&machine_path)?.is_file()
            && fs::metadata(&machine_path)?.len() <= 128 * 1024 * 1024,
        "invalid native checkpoint machine payload"
    );
    let machine = fs::read(&machine_path)?;
    let store = SnapshotStore::new(&binding.store)?;
    let pending = store.begin()?;
    let mut ram = pending.create_ram()?;
    std::io::copy(
        &mut fs::File::open(directory.join("capture.ram"))?,
        &mut ram,
    )?;
    ram.sync_all()?;
    drop(ram);
    let source = directory.join("rootfs");
    let snapshot_id = match storage {
        SnapshotRamStorage::Raw => {
            pending.publish(&source, &machine, binding.compatibility.clone())?
        }
        SnapshotRamStorage::Compressed => {
            pending.publish_compressed(&source, &machine, binding.compatibility.clone())?
        }
    };
    let checkpoint = ExecutionCheckpoint {
        snapshot_id,
        store: binding.store.clone(),
        source_run_id: binding.run_id.clone(),
        source_attempt_id: binding.attempt_id.clone(),
        created_at_unix_ms: ready.created_at_unix_ms,
        ram_storage: storage,
    };
    checkpoint.validate()?;
    Ok(checkpoint)
}

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
pub(super) mod native {
    use super::super::supported::{OverlayDeviceSpec, RunnerSpec};
    use super::*;
    use crate::environment_snapshot::copy_owned_tree;
    use pvisor_vm::api::MachineSnapshot;
    use std::{collections::BTreeMap, fs::OpenOptions, os::unix::fs::OpenOptionsExt};

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct SavedExecution {
        pub version: u32,
        pub run_id: String,
        pub attempt_id: String,
        pub created_at_unix_ms: u64,
        pub ram_storage: SnapshotRamStorage,
        pub root: OverlayDeviceSpec,
        pub workspace: Option<OverlayDeviceSpec>,
        pub workspace_target: Option<PathBuf>,
        pub guest: pvisor_guest::GuestConfig,
        pub cpus: u8,
        pub memory_mib: u32,
        pub state: MachineSnapshot,
    }

    fn collect(
        device: &OverlayDeviceSpec,
        directory: &Path,
        copies: &mut BTreeMap<PathBuf, PathBuf>,
    ) -> anyhow::Result<OverlayDeviceSpec> {
        let mut relocate = |source: &Path| -> anyhow::Result<PathBuf> {
            ensure!(
                source.is_absolute()
                    && source.canonicalize()? == source
                    && source != Path::new("/"),
                "checkpoint requires canonical, owned backing directories; host root is unsupported"
            );
            ensure!(
                !directory.starts_with(source),
                "checkpoint store overlaps source backing"
            );
            if let Some(copied) = copies.get(source) {
                return Ok(copied.clone());
            }
            ensure!(
                copies.len() < 128,
                "too many checkpoint backing directories"
            );
            let copied = directory.join(format!("layer-{:03}", copies.len()));
            copy_owned_tree(source, &copied)
                .with_context(|| format!("copy frozen backing {}", source.display()))?;
            copies.insert(source.to_owned(), copied.clone());
            Ok(copied)
        };
        let mut copied = device.clone();
        copied.lowers = device
            .lowers
            .iter()
            .map(|p| relocate(p))
            .collect::<anyhow::Result<_>>()?;
        copied.upper = relocate(&device.upper)?;
        copied.work = device.work.as_deref().map(&mut relocate).transpose()?;
        copied.preimages = device.preimages.as_deref().map(&mut relocate).transpose()?;
        copied.apply_target = device
            .apply_target
            .as_deref()
            .map(&mut relocate)
            .transpose()?;
        copied.baseline_lower = device
            .baseline_lower
            .as_deref()
            .map(&mut relocate)
            .transpose()?;
        Ok(copied)
    }

    pub(super) fn capture(
        spec: &RunnerSpec,
        vm: &mut pvisor_vm::api::FrozenMachine<'_>,
        directory: &Path,
        storage: SnapshotRamStorage,
    ) -> anyhow::Result<CaptureReady> {
        let binding = spec
            .checkpoint
            .as_ref()
            .context("Job has no durable checkpoint binding")?;
        ensure!(
            directory.parent() == Some(binding.store.join("captures").as_path())
                && directory.canonicalize()? == directory,
            "capture directory is outside Job checkpoint store"
        );
        let ram = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(directory.join("capture.ram"))?;
        let mut state = vm.capture_machine_state(&ram).map_err(anyhow::Error::msg)?;
        ram.sync_all()?;
        let created_at_unix_ms = crate::unix_now_ms();
        let rootfs = directory.join("rootfs");
        fs::create_dir(&rootfs)?;
        let mut copies = BTreeMap::new();
        let root = collect(&spec.root, &rootfs, &mut copies)?;
        let workspace = spec
            .workspace
            .as_ref()
            .map(|device| collect(device, &rootfs, &mut copies))
            .transpose()?;
        let copies = copies.into_iter().collect::<Vec<_>>();
        let mut tags = vec!["/dev/root"];
        if workspace.is_some() {
            tags.push("pvisor-workspace");
        }
        for name in tags {
            let count = state.rebind_filesystem_layers(name, &copies)?;
            ensure!(
                count == 1,
                "checkpoint must bind exactly one {name} filesystem"
            );
        }
        let saved = SavedExecution {
            version: 1,
            run_id: binding.run_id.clone(),
            attempt_id: binding.attempt_id.clone(),
            created_at_unix_ms,
            ram_storage: storage,
            root,
            workspace,
            workspace_target: spec.workspace_target.clone(),
            guest: spec.guest.clone(),
            cpus: spec.cpus,
            memory_mib: spec.memory_mib,
            state,
        };
        crate::util::write_private_json(&directory.join("machine.json"), &saved)?;
        Ok(CaptureReady {
            directory: directory.to_owned(),
            run_id: binding.run_id.clone(),
            attempt_id: binding.attempt_id.clone(),
            created_at_unix_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn fixture() -> (tempfile::TempDir, LaunchBinding, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let store = temp.path().join("store");
        SnapshotStore::new(&store).unwrap();
        let directory = temp.path().join("capture");
        fs::create_dir(&directory).unwrap();
        let binding = LaunchBinding {
            store,
            compatibility: Compatibility {
                host_boot: "boot".into(),
                build: "build".into(),
                firmware: "firmware".into(),
                profile: "pvisor-job-owned-overlay-v1".into(),
            },
            run_id: "run".into(),
            attempt_id: "attempt".into(),
        };
        (temp, binding, directory)
    }

    fn ready(binding: &LaunchBinding, directory: &Path) -> CaptureReady {
        CaptureReady {
            directory: directory.into(),
            run_id: binding.run_id.clone(),
            attempt_id: binding.attempt_id.clone(),
            created_at_unix_ms: 1,
        }
    }

    #[test]
    fn publication_rejects_another_attempt_before_creating_objects() {
        let (_temp, binding, directory) = fixture();
        let mut capture = ready(&binding, &directory);
        capture.attempt_id = "another-attempt".into();
        let error = publish(&binding, capture, &directory, SnapshotRamStorage::Raw).unwrap_err();
        assert!(error.to_string().contains("capture binding mismatch"));
        for name in ["objects", "pending", "content"] {
            assert_eq!(fs::read_dir(binding.store.join(name)).unwrap().count(), 0);
        }
    }

    #[test]
    fn publication_preserves_binding_and_owned_payload_in_both_ram_formats() {
        for storage in [SnapshotRamStorage::Raw, SnapshotRamStorage::Compressed] {
            let (_temp, binding, directory) = fixture();
            let ram = vec![0x51; 65536 + 17];
            fs::write(directory.join("capture.ram"), &ram).unwrap();
            fs::write(directory.join("machine.json"), b"machine-state").unwrap();
            fs::create_dir(directory.join("rootfs")).unwrap();
            fs::write(directory.join("rootfs/file"), b"owned-file").unwrap();
            let checkpoint =
                publish(&binding, ready(&binding, &directory), &directory, storage).unwrap();
            assert_eq!(checkpoint.source_run_id, binding.run_id);
            assert_eq!(checkpoint.source_attempt_id, binding.attempt_id);
            assert_eq!(checkpoint.ram_storage, storage);
            fs::remove_dir_all(&directory).unwrap();
            let saved = SnapshotStore::new(&binding.store)
                .unwrap()
                .open_for_restore(&checkpoint.snapshot_id, &binding.compatibility)
                .unwrap();
            assert_eq!(saved.machine_bytes().unwrap(), b"machine-state");
            let mut restored_ram = Vec::new();
            saved
                .ram_file()
                .unwrap()
                .read_to_end(&mut restored_ram)
                .unwrap();
            assert_eq!(restored_ram, ram);
            let restored_tree = binding.store.parent().unwrap().join("restored");
            saved.materialize(&restored_tree).unwrap();
            assert_eq!(fs::read(restored_tree.join("file")).unwrap(), b"owned-file");
        }
    }
}
