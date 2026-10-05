//! One freeze transaction spans native capture and host-side publication.
//! The host seals filesystem inventories outside the runner's user namespace.
use crate::environment_snapshot::{Compatibility, SnapshotStore, file_hash};
use anyhow::{Context, ensure};
use pvisor_core::operation::{ExecutionCheckpoint, OperationKind, SnapshotRamStorage};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LaunchBinding {
    pub store: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filesystem_pool: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readonly_lowers: Vec<LowerBinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub private_roots: Vec<LowerBinding>,
    pub compatibility: Compatibility,
    pub run_id: String,
    pub attempt_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LowerBinding {
    pub slot: u8,
    pub source: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FilesystemReuse {
    pub slot: u8,
    pub id: String,
}

#[derive(Debug)]
pub(super) struct RetainedLower {
    pub slot: u8,
    pub source: PathBuf,
    pub owner: std::sync::Arc<crate::environment_snapshot::SharedFilesystemLayer>,
}

impl RetainedLower {
    pub fn reuse(&self) -> FilesystemReuse {
        FilesystemReuse {
            slot: self.slot,
            id: self.owner.id.clone(),
        }
    }
}

impl LaunchBinding {
    fn snapshot_store(&self) -> anyhow::Result<SnapshotStore> {
        match &self.filesystem_pool {
            Some(pool) => SnapshotStore::with_filesystem_pool(&self.store, pool),
            None => SnapshotStore::new(&self.store),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CaptureRequest {
    pub operation: OperationKind,
    pub directory: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filesystem_reuse: Vec<FilesystemReuse>,
    #[cfg(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_delta: Option<krun_vmm::snapshot::RamDeltaSpec>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CaptureReady {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RestoreLaunch {
    pub machine: PathBuf,
    pub copies: Vec<(PathBuf, PathBuf)>,
    #[serde(default)]
    pub shared_readonly_lowers: Vec<PathBuf>,
}

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
pub(super) struct PreparedRestore {
    pub cpu_qos: Option<pvisor_core::CpuQosClass>,
    pub checkpoint: ExecutionCheckpoint,
    pub launch: RestoreLaunch,
    pub root: super::supported::OverlayDeviceSpec,
    pub workspace: Option<super::supported::OverlayDeviceSpec>,
    pub workspace_target: Option<PathBuf>,
    pub guest: pvisor_guest::GuestConfig,
    pub ram: std::sync::Arc<fs::File>,
    pub ram_path: PathBuf,
    pub _ram_owner: std::sync::Arc<super::restore_ram::SharedRam>,
    pub _filesystem_owners: Vec<std::sync::Arc<crate::environment_snapshot::SharedFilesystemLayer>>,
    pub _private_files: Option<std::sync::Arc<crate::environment_snapshot::PrivateFilesystemOwner>>,
}

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
impl std::fmt::Debug for PreparedRestore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedRestore")
            .field("checkpoint", &self.checkpoint)
            .finish_non_exhaustive()
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
pub(super) fn binding(
    store: PathBuf,
    run_id: String,
    attempt_id: String,
    firmware: Option<&Path>,
    filesystem_pool: Option<&Path>,
) -> anyhow::Result<LaunchBinding> {
    crate::util::create_dir_all_durable(&store)?;
    let store = store.canonicalize()?;
    let filesystem_pool = filesystem_pool
        .map(|pool| -> anyhow::Result<_> {
            ensure!(
                cfg!(all(target_os = "linux", target_arch = "x86_64")),
                "native filesystem pool requires Linux x86-64"
            );
            crate::util::create_dir_all_durable(pool)?;
            let pool = pool.canonicalize()?;
            ensure!(
                pool != store,
                "native filesystem pool cannot be the Job's writable store"
            );
            SnapshotStore::with_filesystem_pool(&store, &pool)?;
            Ok(pool)
        })
        .transpose()?;
    if filesystem_pool.is_none() {
        SnapshotStore::new(&store)?;
    }
    crate::util::create_dir_all_durable(&store.join("captures"))?;
    Ok(LaunchBinding {
        store,
        filesystem_pool,
        readonly_lowers: vec![],
        private_roots: vec![],
        compatibility: compatibility(firmware)?,
        run_id,
        attempt_id,
    })
}

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
pub(super) fn compatibility(firmware: Option<&Path>) -> anyhow::Result<Compatibility> {
    #[cfg(all(target_os = "linux", target_env = "musl", target_arch = "x86_64"))]
    let firmware_hash = {
        use sha2::{Digest, Sha256};
        let _ = firmware;
        crate::util::encode_hex(&Sha256::digest(&*super::embedded_kernel::KERNEL))
    };
    #[cfg(not(all(target_os = "linux", target_env = "musl", target_arch = "x86_64")))]
    let firmware_hash = file_hash(
        &firmware
            .context("checkpoint capture requires a bound firmware directory")?
            .join(super::firmware_name())
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
    Ok(Compatibility {
        host_boot: host_boot.trim().into(),
        build: file_hash(&std::env::current_exe()?)?,
        firmware: firmware_hash,
        profile: "pvisor-job-owned-overlay-v1".into(),
    })
}

#[derive(Debug)]
pub(super) enum Publication {
    Rejected(anyhow::Error),
    Sealed {
        checkpoint: ExecutionCheckpoint,
        lowers: Vec<RetainedLower>,
        private_files: Option<std::sync::Arc<crate::environment_snapshot::PrivateFilesystemOwner>>,
    },
}

/// Rejected preparation receives CommitReply and a final acknowledgement.
/// An outer error is an uncertain seal: publication may have renamed an object
/// before a directory fsync failed. The supervisor must close IPC and terminate
/// the frozen attempt without allowing a second capture under the same key.
pub(super) fn publish(
    binding: &LaunchBinding,
    ready: CaptureReady,
    directory: &Path,
    storage: SnapshotRamStorage,
    base: Option<&crate::environment_snapshot::PinnedRamBlocks>,
    allowed_filesystems: &[RetainedLower],
) -> anyhow::Result<Publication> {
    let prepared = (|| -> anyhow::Result<_> {
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
        #[allow(unused_mut)]
        let mut machine = fs::read(&machine_path)?;
        use std::os::unix::fs::OpenOptionsExt;
        let source_ram = std::sync::Arc::new(
            fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(directory.join("capture.ram"))?,
        );
        ensure!(
            source_ram.metadata()?.is_file(),
            "capture RAM is not a regular file"
        );
        #[cfg(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))]
        let (delta, filesystem_layers, private_sources, _created_lowers, _source_references) = {
            let mut saved: native::SavedExecution = serde_json::from_slice(&machine)?;
            ensure!(
                (saved.version == 1 && saved.private_sources.is_empty()
                    || saved.version == 2 && binding.filesystem_pool.is_some())
                    && saved.run_id == binding.run_id
                    && saved.attempt_id == binding.attempt_id
                    && saved.created_at_unix_ms == ready.created_at_unix_ms
                    && saved.ram_storage == storage
                    && (1..=8).contains(&saved.cpus)
                    && saved.memory_mib > 0,
                "captured machine belongs to another Job/Attempt or RAM contract"
            );
            native::validate_private_sources(&saved, binding)?;
            let source_references =
                if binding.filesystem_pool.is_some() && !saved.filesystem_layers.is_empty() {
                    Some(
                        tempfile::Builder::new()
                            .prefix("lower-seals-")
                            .tempdir_in(binding.store.join("captures"))?,
                    )
                } else {
                    None
                };
            let created_lowers = native::finalize_captured_lowers(
                &mut saved,
                binding,
                &directory.join("rootfs"),
                allowed_filesystems,
                source_references
                    .as_ref()
                    .map(|references| references.path()),
            )?;
            if saved.version == 2 {
                for name in std::iter::once("/dev/root")
                    .chain(saved.workspace.as_ref().map(|_| "pvisor-workspace"))
                {
                    let mut tag = [0; 36];
                    tag[..name.len()].copy_from_slice(name.as_bytes());
                    let mut count = 0;
                    for mapping in &saved.state.devices {
                        if let devices::snapshot::BusDeviceSnapshot::Virtio(device) =
                            &mapping.device
                        {
                            count += usize::from(device.verify_frozen_filesystem_backing(&tag)?);
                        }
                    }
                    ensure!(
                        count == 1,
                        "direct capture requires one verified frozen {name} filesystem"
                    );
                }
            }
            machine = serde_json::to_vec(&saved)?;
            ensure!(
                machine.len() <= 128 * 1024 * 1024,
                "relocated machine payload exceeds limit"
            );
            krun_vmm::snapshot::MachineRestore {
                state: saved.state,
                ram_file: source_ram.clone(),
            }
            .validate(usize::from(saved.cpus))
            .map_err(anyhow::Error::msg)?;
            if let Some(delta) = &saved.ram_delta {
                delta.validate().map_err(anyhow::Error::msg)?;
                ensure!(
                    storage == SnapshotRamStorage::Compressed
                        && base.is_some_and(|base| base.blocks.length == delta.length
                            && base.sha256 == delta.base_sha256)
                        && delta.block_bytes as usize == crate::ram_backing::BLOCK_BYTES,
                    "incremental capture has no matching pinned baseline"
                );
            }
            (
                saved.ram_delta,
                saved.filesystem_layers,
                saved.private_sources,
                created_lowers,
                source_references,
            )
        };
        #[cfg(not(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        )))]
        let (delta, filesystem_layers, private_sources): (
            Option<()>,
            Vec<crate::environment_snapshot::CapturedFilesystemLayer>,
            Vec<crate::environment_snapshot::CapturedFilesystemSource>,
        ) = (None, Vec::new(), Vec::new());
        let store = binding.snapshot_store()?;
        let pending = store.begin()?;
        #[cfg(not(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        )))]
        let (_created_lowers, _source_references): (
            Vec<RetainedLower>,
            Option<tempfile::TempDir>,
        ) = (vec![], None);
        Ok((
            pending,
            machine,
            source_ram,
            delta,
            filesystem_layers,
            private_sources,
            _created_lowers,
            _source_references,
        ))
    })();
    let (
        pending,
        machine,
        source_ram,
        delta,
        filesystem_layers,
        private_sources,
        _created_lowers,
        _source_references,
    ) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return Ok(Publication::Rejected(error)),
    };
    // From this point an error does not establish that no object was exposed.
    let source = directory.join("rootfs");
    #[cfg(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    ))]
    let publication = {
        let delta = delta
            .as_ref()
            .map(|delta| -> anyhow::Result<_> {
                Ok((
                    base.context("missing pinned incremental RAM baseline")?,
                    crate::environment_snapshot::RamDelta {
                        version: delta.version,
                        length: delta.length,
                        block_bytes: delta.block_bytes,
                        base_sha256: &delta.base_sha256,
                        changed_blocks: &delta.changed_blocks,
                    },
                ))
            })
            .transpose()?;
        pending.publish_native_retained(
            &source,
            &source_ram,
            &machine,
            binding.compatibility.clone(),
            storage == SnapshotRamStorage::Compressed,
            crate::environment_snapshot::NativeLayerCapture {
                layers: &filesystem_layers,
                private_sources: &private_sources,
                delta,
            },
        )
    };
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    )))]
    let publication = {
        let _ = (delta, base, allowed_filesystems, private_sources);
        pending
            .publish_native_capture(
                &source,
                &source_ram,
                &machine,
                binding.compatibility.clone(),
                storage == SnapshotRamStorage::Compressed,
            )
            .map(|id| crate::environment_snapshot::NativePublication {
                id,
                lowers: vec![],
                private_files: None,
                #[cfg(target_os = "linux")]
                filesystem_capture_stats: None,
            })
    };
    let publication =
        publication.context("checkpoint seal outcome is uncertain; terminating frozen attempt")?;
    #[cfg(target_os = "linux")]
    if let Some(stats) = publication.filesystem_capture_stats {
        crate::util::startup_detail_run(
            "checkpoint.filesystem",
            &binding.run_id,
            format_args!(
                "payload_bytes={} encoded_frames={} reused_frames={} zero_frames={}",
                stats.payload_bytes, stats.encoded_frames, stats.reused_frames, stats.zero_frames,
            ),
        );
    }
    #[allow(unused_mut)]
    let mut lowers = Vec::new();
    ensure!(
        publication.lowers.len() == filesystem_layers.len(),
        "sealed lower ownership mismatch"
    );
    for (captured, owner) in filesystem_layers.iter().zip(publication.lowers) {
        #[cfg(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        ))]
        {
            let slot = native::lower_slot(&captured.path)?;
            let source = &binding
                .readonly_lowers
                .iter()
                .find(|lower| lower.slot == slot)
                .context("sealed lower has no live launch binding")?
                .source;
            lowers.push(RetainedLower {
                slot,
                source: source.clone(),
                owner: std::sync::Arc::new(owner),
            });
        }
        #[cfg(not(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64")
        )))]
        let _ = (captured, owner);
    }
    let checkpoint = ExecutionCheckpoint {
        snapshot_id: publication.id,
        store: binding.store.clone(),
        source_run_id: binding.run_id.clone(),
        source_attempt_id: binding.attempt_id.clone(),
        created_at_unix_ms: ready.created_at_unix_ms,
        ram_storage: storage,
    };
    checkpoint.validate()?;
    Ok(Publication::Sealed {
        checkpoint,
        lowers,
        private_files: publication.private_files,
    })
}

fn receipt_path(binding: &LaunchBinding, request: &str) -> anyhow::Result<PathBuf> {
    use sha2::{Digest, Sha256};
    let key = crate::util::encode_hex(&Sha256::digest(serde_json::to_vec(&(
        &binding.run_id,
        &binding.attempt_id,
        request,
    ))?));
    Ok(binding.store.join("requests").join(format!("{key}.json")))
}

pub(super) fn remember_request(
    binding: &LaunchBinding,
    request: &str,
    checkpoint: &ExecutionCheckpoint,
) -> anyhow::Result<()> {
    crate::util::create_dir_all_durable(&binding.store.join("requests"))?;
    crate::util::write_private_json(&receipt_path(binding, request)?, checkpoint)
}

pub(super) fn lookup_request(
    binding: &LaunchBinding,
    request: &str,
    storage: SnapshotRamStorage,
) -> anyhow::Result<Option<ExecutionCheckpoint>> {
    let path = receipt_path(binding, request)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_file() && metadata.len() <= 16 * 1024,
        "invalid checkpoint request receipt"
    );
    let checkpoint: ExecutionCheckpoint = serde_json::from_slice(&fs::read(path)?)?;
    checkpoint.validate()?;
    ensure!(
        checkpoint.store == binding.store
            && checkpoint.source_run_id == binding.run_id
            && checkpoint.source_attempt_id == binding.attempt_id
            && checkpoint.ram_storage == storage,
        "checkpoint request id conflicts with its original capture binding"
    );
    let published = binding
        .snapshot_store()?
        .open_for_restore(&checkpoint.snapshot_id, &binding.compatibility)?;
    drop(published);
    Ok(Some(checkpoint))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn maximum_lower_reuse_set_fits_control_frame_without_wire_source_paths() {
        let request = CaptureRequest {
            operation: OperationKind::RunCheckpoint {
                request_id: "checkpoint".into(),
                ram_storage: SnapshotRamStorage::Compressed,
            },
            directory: PathBuf::from("/private/job/execution-snapshots/captures/capture-run"),
            filesystem_reuse: (0..128)
                .map(|slot| FilesystemReuse {
                    slot,
                    id: format!("{slot:064x}"),
                })
                .collect(),
            #[cfg(any(
                all(target_os = "linux", target_arch = "x86_64"),
                all(target_os = "macos", target_arch = "aarch64")
            ))]
            ram_delta: None,
        };
        let bytes = serde_json::to_vec(&request).unwrap();
        assert!(bytes.len() <= super::super::control::MAX_FRAME);
        let decoded: CaptureRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded.filesystem_reuse.len(), 128);
        assert_eq!(decoded.filesystem_reuse[127].slot, 127);
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn filesystem_pool_rejects_every_writable_role_and_guest_projection() {
        use super::super::supported::OverlayDeviceSpec;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let make = |name: &str| {
            let path = root.join(name);
            fs::create_dir_all(&path).unwrap();
            path
        };
        let pool = make("pool");
        let pool_child = make("pool/tree");
        let binding = LaunchBinding {
            store: make("job/snapshots"),
            filesystem_pool: Some(pool.clone()),
            readonly_lowers: vec![],
            private_roots: vec![],
            compatibility: Compatibility {
                host_boot: "boot".into(),
                build: "build".into(),
                firmware: "firmware".into(),
                profile: "pool-boundary-test".into(),
            },
            run_id: "run".into(),
            attempt_id: "attempt".into(),
        };
        let device = OverlayDeviceSpec {
            lowers: vec![make("readonly")],
            upper: make("private/upper"),
            work: Some(make("private/work")),
            preimages: Some(make("private/preimages")),
            apply_target: Some(make("private/apply")),
            baseline_lower: Some(make("private/baseline")),
            excluded: vec![],
            access_policy: Default::default(),
        };
        native::validate_pool_binding(&binding, &device, Some(&device)).unwrap();
        for writable in [&pool, &pool_child, root] {
            for role in 0..5 {
                let mut invalid = device.clone();
                match role {
                    0 => invalid.upper = writable.to_owned(),
                    1 => invalid.work = Some(writable.to_owned()),
                    2 => invalid.preimages = Some(writable.to_owned()),
                    3 => invalid.apply_target = Some(writable.to_owned()),
                    4 => invalid.baseline_lower = Some(writable.to_owned()),
                    _ => unreachable!(),
                }
                assert!(native::validate_pool_binding(&binding, &invalid, None).is_err());
                assert!(native::validate_pool_binding(&binding, &device, Some(&invalid)).is_err());
            }
            let mut invalid = binding.clone();
            invalid.store = writable.to_owned();
            assert!(native::validate_pool_binding(&invalid, &device, None).is_err());
        }
        let alias = root.join("private/pool-alias");
        std::os::unix::fs::symlink(&pool, &alias).unwrap();
        let mut invalid = device.clone();
        invalid.upper = alias;
        assert!(native::validate_pool_binding(&binding, &invalid, None).is_err());
        for projected in [&pool, root] {
            invalid = device.clone();
            invalid.lowers = vec![projected.to_owned()];
            assert!(native::validate_pool_binding(&binding, &invalid, None).is_err());
        }
        // A specific authenticated tree may be projected read-only; projecting
        // its parent pool would expose other snapshots and reference markers.
        let mut retained = device;
        retained.lowers = vec![pool_child];
        native::validate_pool_binding(&binding, &retained, None).unwrap();
    }

    #[test]
    fn sealed_request_receipt_replays_exact_object_and_rejects_conflicts_or_deletion() {
        // SnapshotStore accepts opaque machine bytes. This checks its request
        // receipt protocol; full native machine validity has a separate KVM gate.
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("data"), b"owned").unwrap();
        let binding = LaunchBinding {
            store: directory.path().join("snapshots"),
            filesystem_pool: None,
            readonly_lowers: vec![],
            private_roots: vec![],
            compatibility: Compatibility {
                host_boot: "boot".into(),
                build: "build".into(),
                firmware: "firmware".into(),
                profile: "receipt-test".into(),
            },
            run_id: "run".into(),
            attempt_id: "attempt".into(),
        };
        let store = SnapshotStore::new(&binding.store).unwrap();
        let pending = store.begin().unwrap();
        pending.create_ram().unwrap().write_all(&[0; 4096]).unwrap();
        let snapshot_id = pending
            .publish(&source, b"opaque-machine", binding.compatibility.clone())
            .unwrap();
        let checkpoint = ExecutionCheckpoint {
            snapshot_id,
            store: binding.store.clone(),
            source_run_id: binding.run_id.clone(),
            source_attempt_id: binding.attempt_id.clone(),
            created_at_unix_ms: 1,
            ram_storage: SnapshotRamStorage::Raw,
        };
        remember_request(&binding, "save", &checkpoint).unwrap();
        assert_eq!(
            lookup_request(&binding, "save", SnapshotRamStorage::Raw).unwrap(),
            Some(checkpoint.clone())
        );
        assert!(lookup_request(&binding, "save", SnapshotRamStorage::Compressed).is_err());
        assert!(
            lookup_request(&binding, "new", SnapshotRamStorage::Raw)
                .unwrap()
                .is_none()
        );
        let mut foreign = checkpoint.clone();
        foreign.source_attempt_id = "foreign".into();
        remember_request(&binding, "foreign", &foreign).unwrap();
        assert!(lookup_request(&binding, "foreign", SnapshotRamStorage::Raw).is_err());
        store.delete(&checkpoint.snapshot_id).unwrap();
        assert!(lookup_request(&binding, "save", SnapshotRamStorage::Raw).is_err());
    }
}

#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]
pub(super) mod native {
    use super::super::supported::{OverlayDeviceSpec, RunnerSpec};
    use super::*;
    use crate::environment_snapshot::copy_owned_tree;
    use devices::snapshot::BusDeviceSnapshot;
    use krun_vmm::snapshot::MachineSnapshot;
    use std::{collections::BTreeMap, fs::OpenOptions, os::unix::fs::OpenOptionsExt};

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct SavedExecution {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub private_sources: Vec<crate::environment_snapshot::CapturedFilesystemSource>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub filesystem_layers: Vec<crate::environment_snapshot::CapturedFilesystemLayer>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub ram_delta: Option<krun_vmm::snapshot::RamDeltaCapture>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cpu_qos: Option<pvisor_core::CpuQosClass>,
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

    fn private_roots(
        root: &OverlayDeviceSpec,
        workspace: Option<&OverlayDeviceSpec>,
    ) -> std::collections::BTreeSet<PathBuf> {
        let mut private = std::collections::BTreeSet::new();
        for device in std::iter::once(root).chain(workspace) {
            private.insert(device.upper.clone());
            private.extend(device.work.iter().cloned());
            private.extend(device.preimages.iter().cloned());
            private.extend(device.apply_target.iter().cloned());
            private.extend(device.baseline_lower.iter().cloned());
        }
        private
    }

    fn readonly_roots(
        root: &OverlayDeviceSpec,
        workspace: Option<&OverlayDeviceSpec>,
    ) -> std::collections::BTreeSet<PathBuf> {
        let private = private_roots(root, workspace);
        std::iter::once(root)
            .chain(workspace)
            .flat_map(|device| device.lowers.iter().cloned())
            .filter(|path| !private.contains(path))
            .collect()
    }

    pub(in crate::executor::vm) fn capture_lower_bindings(
        root: &OverlayDeviceSpec,
        workspace: Option<&OverlayDeviceSpec>,
    ) -> anyhow::Result<Vec<LowerBinding>> {
        capture_role_bindings(root, workspace, true)
    }

    pub(in crate::executor::vm) fn capture_private_bindings(
        root: &OverlayDeviceSpec,
        workspace: Option<&OverlayDeviceSpec>,
    ) -> anyhow::Result<Vec<LowerBinding>> {
        capture_role_bindings(root, workspace, false)
    }

    fn capture_role_bindings(
        root: &OverlayDeviceSpec,
        workspace: Option<&OverlayDeviceSpec>,
        immutable: bool,
    ) -> anyhow::Result<Vec<LowerBinding>> {
        let readonly = readonly_roots(root, workspace);
        let mut slots = BTreeMap::new();
        let mut bindings = Vec::new();
        for device in std::iter::once(root).chain(workspace) {
            for source in device
                .lowers
                .iter()
                .chain(std::iter::once(&device.upper))
                .chain(device.work.iter())
                .chain(device.preimages.iter())
                .chain(device.apply_target.iter())
                .chain(device.baseline_lower.iter())
            {
                if slots.contains_key(source) {
                    continue;
                }
                ensure!(slots.len() < 128, "too many native backing roots");
                let slot = slots.len() as u8;
                slots.insert(source.clone(), slot);
                if readonly.contains(source) == immutable {
                    bindings.push(LowerBinding {
                        slot,
                        source: source.clone(),
                    });
                }
            }
        }
        Ok(bindings)
    }

    pub(super) fn lower_slot(path: &Path) -> anyhow::Result<u8> {
        let name = path.to_str().context("non-UTF-8 native lower slot")?;
        let slot = name
            .strip_prefix("layer-")
            .context("invalid native lower slot")?
            .parse::<u8>()?;
        ensure!(
            slot < 128 && name == format!("layer-{slot:03}"),
            "invalid native lower slot"
        );
        Ok(slot)
    }

    pub(super) fn validate_private_sources(
        saved: &SavedExecution,
        binding: &LaunchBinding,
    ) -> anyhow::Result<()> {
        if saved.version == 1 {
            ensure!(
                saved.private_sources.is_empty(),
                "legacy capture cannot contain direct private sources"
            );
            return Ok(());
        }
        ensure!(
            saved.version == 2
                && binding.filesystem_pool.is_some()
                && !saved.private_sources.is_empty()
                && saved.private_sources.len() == binding.private_roots.len(),
            "direct private capture has no trusted launch bindings"
        );
        let private = private_roots(&saved.root, saved.workspace.as_ref());
        validate_private_source_bindings(&private, &saved.private_sources, binding)
    }

    fn validate_private_source_bindings(
        private: &std::collections::BTreeSet<PathBuf>,
        captured_sources: &[crate::environment_snapshot::CapturedFilesystemSource],
        binding: &LaunchBinding,
    ) -> anyhow::Result<()> {
        let mut sources = std::collections::BTreeSet::new();
        let mut paths = std::collections::BTreeSet::new();
        for captured in captured_sources {
            let slot = lower_slot(&captured.path)?;
            ensure!(
                binding
                    .private_roots
                    .iter()
                    .any(|root| root.slot == slot && root.source == captured.source)
                    && !binding.readonly_lowers.iter().any(|root| root.slot == slot)
                    && private.contains(&captured.source)
                    && sources.insert(captured.source.clone())
                    && paths.insert(captured.path.clone()),
                "direct private source escapes its launch slot or frozen role"
            );
        }
        ensure!(
            &sources == private,
            "direct capture omitted a private frozen role"
        );
        Ok(())
    }

    pub(super) fn finalize_captured_lowers(
        saved: &mut SavedExecution,
        binding: &LaunchBinding,
        capture_root: &Path,
        allowed: &[RetainedLower],
        references: Option<&Path>,
    ) -> anyhow::Result<Vec<RetainedLower>> {
        let immutable = readonly_roots(&saved.root, saved.workspace.as_ref());
        let private = private_roots(&saved.root, saved.workspace.as_ref());
        let mut copies = BTreeMap::new();
        let mut created = Vec::new();
        for layer in &mut saved.filesystem_layers {
            let slot = lower_slot(&layer.path)?;
            let source = &binding
                .readonly_lowers
                .iter()
                .find(|lower| lower.slot == slot)
                .context("captured lower has no read-only launch slot")?
                .source;
            ensure!(
                layer.source == *source
                    && immutable.contains(source)
                    && !private
                        .iter()
                        .any(|path| path.starts_with(source) || source.starts_with(path)),
                "captured lower escapes its read-only launch binding"
            );
            let owner = if let Some(id) = &layer.id {
                allowed
                    .iter()
                    .find(|owner| owner.slot == slot && &owner.owner.id == id)
                    .context("captured lower is not pinned at its launch slot")?
            } else {
                ensure!(
                    !allowed.iter().any(|owner| owner.slot == slot),
                    "capture omitted a supervisor-pinned lower id"
                );
                let owner = binding.snapshot_store()?.retain_live_lower(
                    source,
                    &layer.path,
                    references.context("fresh lower has no private reference staging")?,
                )?;
                layer.id = Some(owner.id.clone());
                created.push(RetainedLower {
                    slot,
                    source: source.clone(),
                    owner: std::sync::Arc::new(owner),
                });
                created.last().unwrap()
            };
            ensure!(
                binding.filesystem_pool.as_deref() == Some(owner.owner.pool())
                    && binding
                        .readonly_lowers
                        .iter()
                        .any(|lower| lower.slot == slot && lower.source == owner.source)
                    && layer.source == owner.source
                    && immutable.contains(&layer.source)
                    && !private
                        .iter()
                        .any(|path| path.starts_with(&layer.source)
                            || layer.source.starts_with(path)),
                "captured lower escapes its read-only launch binding"
            );
            owner.owner.verify_source(&layer.source)?;
            if layer.source != owner.owner.root() {
                ensure!(
                    copies
                        .insert(layer.source.clone(), owner.owner.root().to_owned())
                        .is_none(),
                    "duplicate live lower relocation"
                );
            }
        }
        // These pool trees are deliberately outside the fresh runner's Landlock
        // rules. Only the supervisor reads/rebinds them, while CPUs/devices remain
        // frozen. Private backing remains unchanged by this lower relocation.
        for (name, device) in std::iter::once(("/dev/root", &mut saved.root)).chain(
            saved
                .workspace
                .iter_mut()
                .map(|device| ("pvisor-workspace", device)),
        ) {
            let selected = copies
                .iter()
                .filter(|(source, _)| device.lowers.contains(source))
                .map(|(source, destination)| (source.clone(), destination.clone()))
                .collect::<Vec<_>>();
            if selected.is_empty() {
                continue;
            }
            let mut tag = [0; 36];
            tag[..name.len()].copy_from_slice(name.as_bytes());
            let mut count = 0;
            for mapping in &mut saved.state.devices {
                if let BusDeviceSnapshot::Virtio(device) = &mut mapping.device {
                    count += usize::from(device.rebind_filesystem_lower_copies(&tag, &selected)?);
                }
            }
            ensure!(
                count == 1,
                "supervisor must relocate exactly one {name} device"
            );
            for lower in &mut device.lowers {
                if let Some(destination) = copies.get(lower) {
                    *lower = destination.clone();
                }
            }
        }
        for layer in &mut saved.filesystem_layers {
            if let Some(destination) = copies.get(&layer.source) {
                layer.source = destination.clone();
            }
        }
        let pinned = allowed.iter().chain(created.iter()).collect::<Vec<_>>();
        validate_captured_layers(saved, binding, capture_root, &pinned)?;
        Ok(created)
    }

    pub(in crate::executor::vm) fn validate_pool_binding(
        binding: &LaunchBinding,
        root: &OverlayDeviceSpec,
        workspace: Option<&OverlayDeviceSpec>,
    ) -> anyhow::Result<()> {
        let Some(pool) = &binding.filesystem_pool else {
            return Ok(());
        };
        for private in private_roots(root, workspace)
            .into_iter()
            .chain(std::iter::once(binding.store.clone()))
        {
            let private = private.canonicalize()?;
            ensure!(
                !pool.starts_with(&private) && !private.starts_with(pool),
                "immutable filesystem pool overlaps a VM-writable root"
            );
        }
        for lower in std::iter::once(root)
            .chain(workspace)
            .flat_map(|device| &device.lowers)
        {
            ensure!(
                !pool.starts_with(lower),
                "immutable pool cannot be projected inside a guest lower"
            );
        }
        Ok(())
    }

    pub(super) fn validate_captured_layers(
        saved: &SavedExecution,
        binding: &LaunchBinding,
        capture_root: &Path,
        allowed: &[&RetainedLower],
    ) -> anyhow::Result<()> {
        let Some(pool) = &binding.filesystem_pool else {
            ensure!(
                saved.filesystem_layers.is_empty() && allowed.is_empty(),
                "native filesystem reuse has no host-owned pool"
            );
            return Ok(());
        };
        let expected = readonly_roots(&saved.root, saved.workspace.as_ref());
        let private = private_roots(&saved.root, saved.workspace.as_ref());
        let mut sources = std::collections::BTreeSet::new();
        let mut paths = std::collections::BTreeSet::new();
        let mut reused = std::collections::BTreeSet::new();
        ensure!(
            saved.filesystem_layers.len() <= 128,
            "too many captured immutable layers"
        );
        for layer in &saved.filesystem_layers {
            let slot = lower_slot(&layer.path)?;
            ensure!(
                binding
                    .readonly_lowers
                    .iter()
                    .any(|lower| lower.slot == slot)
                    && layer.path.components().count() == 1
                    && layer
                        .path
                        .components()
                        .all(|part| matches!(part, std::path::Component::Normal(_)))
                    && sources.insert(layer.source.clone())
                    && paths.insert(layer.path.clone())
                    && expected.contains(&layer.source)
                    && !private
                        .iter()
                        .any(|path| path.starts_with(&layer.source)
                            || layer.source.starts_with(path)),
                "captured immutable layer has an ambiguous or writable role"
            );
            if let Some(id) = &layer.id {
                ensure!(
                    allowed.iter().any(|owner| &owner.owner.id == id
                        && lower_slot(&layer.path).is_ok_and(|slot| slot == owner.slot))
                        && reused.insert(id.clone())
                        && layer.source == pool.join("filesystems").join(id).join("tree"),
                    "captured lower was not pinned by its supervisor"
                );
            } else {
                ensure!(
                    layer.source == capture_root.join(&layer.path),
                    "fresh captured lower escapes the owned forest"
                );
            }
        }
        ensure!(
            sources == expected
                && reused.len() == allowed.len()
                && saved.filesystem_layers.len() == binding.readonly_lowers.len(),
            "captured lower set does not match its frozen roles and pinned owners"
        );
        Ok(())
    }

    pub(in crate::executor::vm) fn prepare_restore(
        checkpoint: ExecutionCheckpoint,
        settings: &crate::VmSettings,
        storage: &Path,
    ) -> anyhow::Result<(PreparedRestore, crate::OverlayHint)> {
        use std::os::unix::ffi::OsStrExt;
        checkpoint.validate()?;
        ensure!(
            settings.memory_pool.is_none()
                && !settings.ram_compression
                && settings.ram_backing.is_none(),
            "snapshot restore requires private COW RAM, without a writable backing or cold pager"
        );
        crate::util::create_dir_all_durable(storage)?;
        let storage = storage.canonicalize()?;
        ensure!(
            !storage.starts_with(&checkpoint.store) && !checkpoint.store.starts_with(&storage),
            "restored Attempt and source snapshot store must be independent"
        );
        let restore_binding = binding(
            storage.join("execution-snapshots"),
            "restore-preparation".into(),
            "restore-preparation".into(),
            settings.library_dir.as_deref(),
            settings.snapshot_filesystem_pool.as_deref(),
        )?;
        let compatibility = restore_binding.compatibility.clone();
        let store = match settings.snapshot_filesystem_pool.as_deref() {
            Some(pool) => SnapshotStore::with_filesystem_pool(&checkpoint.store, pool)?,
            None => SnapshotStore::new(&checkpoint.store)?,
        };
        let published = store.open_for_restore(&checkpoint.snapshot_id, &compatibility)?;
        #[cfg(target_os = "linux")]
        let private_files = published.pin_private_files(&restore_binding.snapshot_store()?)?;
        #[cfg(not(target_os = "linux"))]
        let private_files = None;
        let mut saved: SavedExecution = serde_json::from_slice(&published.machine_bytes()?)?;
        ensure!(
            (saved.version == 1 && saved.private_sources.is_empty()
                || saved.version == 2
                    && published.manifest().version == 5
                    && !saved.private_sources.is_empty())
                && saved.run_id == checkpoint.source_run_id
                && saved.attempt_id == checkpoint.source_attempt_id
                && saved.created_at_unix_ms == checkpoint.created_at_unix_ms
                && saved.ram_storage == checkpoint.ram_storage
                && saved.cpus == settings.cpus as u8
                && saved.memory_mib == settings.memory_mib
                && saved.guest.network.is_none(),
            "snapshot identity, resource budget or no-network contract mismatch"
        );
        saved.guest.command()?;
        if let Some(delta) = &saved.ram_delta {
            delta.validate().map_err(anyhow::Error::msg)?;
            ensure!(
                saved.ram_storage == SnapshotRamStorage::Compressed
                    && delta.block_bytes as usize == crate::ram_backing::BLOCK_BYTES
                    && published
                        .manifest()
                        .ram_blocks
                        .as_ref()
                        .is_some_and(|blocks| blocks.length == delta.length),
                "incremental RAM audit does not match its self-contained payload"
            );
        }
        ensure!(
            !saved.root.lowers.is_empty()
                && saved
                    .workspace
                    .as_ref()
                    .is_none_or(|device| !device.lowers.is_empty()),
            "snapshot overlay must have a lower layer"
        );
        let source = PathBuf::from(std::ffi::OsStr::from_bytes(
            &published.manifest().source_root,
        ));
        ensure!(
            saved.filesystem_layers.len() == published.manifest().filesystem_layers.len(),
            "native lower records do not match the sealed snapshot"
        );
        let immutable = readonly_roots(&saved.root, saved.workspace.as_ref());
        let private_roles = private_roots(&saved.root, saved.workspace.as_ref());
        let mut layered = BTreeMap::new();
        for captured in &saved.filesystem_layers {
            let layer = published
                .manifest()
                .filesystem_layers
                .iter()
                .find(|layer| layer.path == captured.path.as_os_str().as_bytes())
                .context("native lower has no authenticated inventory")?;
            ensure!(
                layer.source == captured.source.as_os_str().as_bytes()
                    && captured.id.as_ref().is_none_or(|id| id == &layer.id)
                    && immutable.contains(&captured.source)
                    && !private_roles
                        .iter()
                        .any(|path| path.starts_with(&captured.source)
                            || captured.source.starts_with(path))
                    && layered
                        .insert(captured.source.clone(), captured.path.clone())
                        .is_none(),
                "sealed immutable lower has an ambiguous or writable native binding"
            );
        }
        ensure!(
            layered.is_empty()
                || layered
                    .keys()
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>()
                    == immutable,
            "native layered snapshot omitted a lower role"
        );
        if saved.version == 2 {
            let mut sources = std::collections::BTreeSet::new();
            let mut slots = published
                .manifest()
                .filesystem_layers
                .iter()
                .map(|layer| PathBuf::from(std::ffi::OsStr::from_bytes(&layer.path)))
                .collect::<std::collections::BTreeSet<_>>();
            for captured in &saved.private_sources {
                lower_slot(&captured.path)?;
                let source_bytes = captured.source.as_os_str().as_bytes();
                ensure!(
                    captured.source.is_absolute()
                        && captured.source != Path::new("/")
                        && source_bytes.len() <= 4096
                        && !source_bytes.contains(&0)
                        && !source_bytes
                            .split(|byte| *byte == b'/')
                            .skip(1)
                            .any(|part| part.is_empty() || part == b"." || part == b"..")
                        && captured.source.components().all(|part| matches!(
                            part,
                            std::path::Component::RootDir | std::path::Component::Normal(_)
                        ))
                        && private_roles.contains(&captured.source)
                        && sources.insert(captured.source.clone())
                        && slots.insert(captured.path.clone())
                        && published
                            .manifest()
                            .filesystem
                            .entries
                            .iter()
                            .any(|entry| entry.path == captured.path.as_os_str().as_bytes()
                                && entry.object
                                    == crate::environment_snapshot::TreeObject::Directory)
                        && layered
                            .insert(captured.source.clone(), captured.path.clone())
                            .is_none(),
                    "sealed private source has no unique role/directory binding"
                );
            }
            ensure!(
                sources == private_roles && slots.len() <= 128,
                "sealed private role inventory is incomplete"
            );
        }
        ensure!(
            source.is_absolute() && source != Path::new("/"),
            "invalid snapshot filesystem root"
        );
        let preparation = storage.join("execution-restore");
        fs::create_dir(&preparation).context("Attempt restore preparation already exists")?;
        let rootfs = preparation.join("rootfs");
        fs::create_dir(&rootfs)?;
        let references = preparation.join("filesystem-references");
        crate::util::create_dir_all_durable(&references)?;
        let mut readonly = std::collections::BTreeSet::new();
        let mut private = std::collections::BTreeSet::new();
        for device in std::iter::once(&saved.root).chain(saved.workspace.iter()) {
            readonly.extend(device.lowers.iter().cloned());
            private.insert(device.upper.clone());
            private.extend(device.work.iter().cloned());
            private.extend(device.preimages.iter().cloned());
            private.extend(device.apply_target.iter().cloned());
            private.extend(device.baseline_lower.iter().cloned());
        }
        readonly.retain(|path| !private.contains(path));
        // Linux's runner installs Landlock read-only rules for every lower.
        // Marker links require the same volume; other profiles keep full copies.
        let share = cfg!(target_os = "linux") && published.can_share_layers(&references)?;
        let mut filesystem_owners = Vec::new();
        let mut shared_readonly_lowers = Vec::new();
        let mut copies: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
        let mut relocate = |path: &Path| -> anyhow::Result<PathBuf> {
            if let Some(destination) = copies.get(path) {
                return Ok(destination.clone());
            }
            let relative = if let Some(relative) = layered.get(path) {
                relative.as_path()
            } else {
                path.strip_prefix(&source)
                    .context("snapshot layer escapes captured tree")?
            };
            ensure!(
                relative.components().count() == 1
                    && relative
                        .components()
                        .all(|c| matches!(c, std::path::Component::Normal(_))),
                "invalid owned snapshot layer path"
            );
            let destination = if share && readonly.contains(path) {
                let owner = published.share_readonly_layer(relative, &references)?;
                let destination = owner.root().to_owned();
                filesystem_owners.push(std::sync::Arc::new(owner));
                shared_readonly_lowers.push(path.to_owned());
                destination
            } else {
                let destination = rootfs.join(relative);
                published.materialize_layer(relative, &destination)?;
                destination
            };
            ensure!(
                fs::symlink_metadata(&destination)?.is_dir(),
                "missing restored owned layer"
            );
            copies.insert(path.to_owned(), destination.clone());
            Ok(destination)
        };
        fn relocate_device(
            device: &OverlayDeviceSpec,
            relocate: &mut impl FnMut(&Path) -> anyhow::Result<PathBuf>,
        ) -> anyhow::Result<OverlayDeviceSpec> {
            let mut copy = device.clone();
            copy.lowers = device
                .lowers
                .iter()
                .map(|p| relocate(p))
                .collect::<anyhow::Result<_>>()?;
            copy.upper = relocate(&device.upper)?;
            copy.work = device.work.as_deref().map(&mut *relocate).transpose()?;
            copy.preimages = device
                .preimages
                .as_deref()
                .map(&mut *relocate)
                .transpose()?;
            copy.apply_target = device
                .apply_target
                .as_deref()
                .map(&mut *relocate)
                .transpose()?;
            copy.baseline_lower = device
                .baseline_lower
                .as_deref()
                .map(&mut *relocate)
                .transpose()?;
            Ok(copy)
        }
        let mut root = relocate_device(&saved.root, &mut relocate)?;
        let mut workspace = saved
            .workspace
            .as_ref()
            .map(|device| relocate_device(device, &mut relocate))
            .transpose()?;
        let recorded = workspace.as_mut().unwrap_or(&mut root);
        let preimages = recorded
            .preimages
            .clone()
            .context("snapshot has no durable preimage journal")?;
        let recorded_preimages = storage.join("preimages");
        fs::rename(&preimages, &recorded_preimages)?;
        for destination in copies.values_mut() {
            if *destination == preimages {
                *destination = recorded_preimages.clone();
            }
        }
        fn replace_preimages(device: &mut OverlayDeviceSpec, old: &Path, new: &Path) {
            if device.preimages.as_deref() == Some(old) {
                device.preimages = Some(new.to_owned());
            }
        }
        replace_preimages(&mut root, &preimages, &recorded_preimages);
        if let Some(workspace) = &mut workspace {
            replace_preimages(workspace, &preimages, &recorded_preimages);
        }
        let recorded = workspace.as_ref().unwrap_or(&root);
        let target = recorded
            .apply_target
            .clone()
            .unwrap_or_else(|| recorded.lowers.last().unwrap().clone());
        let overlay = crate::OverlayHint {
            access_policy: recorded.access_policy.clone(),
            lower_dirs: recorded.lowers.clone(),
            stage_dir: Some(storage.clone()),
            upper_dir: Some(recorded.upper.clone()),
            work_dir: recorded.work.clone(),
            protect_target: true,
            execution_snapshot: Some(crate::ExecutionOverlayHint {
                target,
                baseline_lower: recorded.baseline_lower.clone(),
                excluded_paths: recorded.excluded.clone(),
            }),
            ..Default::default()
        };
        let ram_owner = super::super::restore_ram::acquire(
            &checkpoint.store,
            &checkpoint.snapshot_id,
            &published,
        )?;
        let ram = ram_owner.file.clone();
        krun_vmm::snapshot::MachineRestore {
            state: saved.state.clone(),
            ram_file: ram.clone(),
        }
        .validate(usize::from(saved.cpus))
        .map_err(anyhow::Error::msg)?;
        let machine = preparation.join("machine.json");
        // Keep original bindings in this immutable private header. The runner
        // reopens copies after entering the same UID mapping as capture.
        crate::util::write_private_json(&machine, &saved)?;
        let ram_path = ram_owner.path.clone();
        let prepared = PreparedRestore {
            cpu_qos: saved.cpu_qos,
            checkpoint,
            launch: RestoreLaunch {
                machine,
                copies: copies.into_iter().collect(),
                shared_readonly_lowers,
            },
            root,
            workspace,
            workspace_target: saved.workspace_target.take(),
            guest: saved.guest,
            ram,
            ram_path,
            _ram_owner: ram_owner,
            _filesystem_owners: filesystem_owners,
            _private_files: private_files,
        };
        drop(published);
        Ok((prepared, overlay))
    }

    pub(in crate::executor::vm) fn machine_restore(
        spec: &RunnerSpec,
        ram: fs::File,
    ) -> anyhow::Result<krun_vmm::snapshot::MachineRestore> {
        let launch = spec.restore.as_ref().context("missing prepared restore")?;
        let mut saved: SavedExecution = serde_json::from_slice(&fs::read(&launch.machine)?)?;
        ensure!(
            saved.cpu_qos == spec.cpu_qos
                && matches!(saved.version, 1 | 2)
                && saved.cpus == spec.cpus
                && saved.memory_mib == spec.memory_mib
                && serde_json::to_value(&saved.guest)? == serde_json::to_value(&spec.guest)?,
            "restore runner contract mismatch"
        );
        for (name, overlay) in std::iter::once(("/dev/root", &spec.root))
            .chain(spec.workspace.iter().map(|w| ("pvisor-workspace", w)))
        {
            let mut tag = [0; 36];
            tag[..name.len()].copy_from_slice(name.as_bytes());
            let mut count = 0;
            let original = if name == "/dev/root" {
                &saved.root
            } else {
                saved
                    .workspace
                    .as_ref()
                    .context("missing saved workspace")?
            };
            let shared = launch
                .shared_readonly_lowers
                .iter()
                .filter(|source| original.lowers.contains(source))
                .cloned()
                .collect::<Vec<_>>();
            for mapping in &mut saved.state.devices {
                if let BusDeviceSnapshot::Virtio(device) = &mut mapping.device
                    && if shared.is_empty() {
                        device.rebind_filesystem_layers(&tag, &launch.copies)?
                    } else {
                        device.rebind_filesystem_shared_lowers(&tag, &launch.copies, &shared)?
                    }
                {
                    ensure!(
                        device.rebind_filesystem_policy(&tag, &overlay.access_policy)?,
                        "missing restored policy device"
                    );
                    count += 1;
                }
            }
            ensure!(count == 1, "restore requires exactly one {name} filesystem");
        }
        let restore = krun_vmm::snapshot::MachineRestore {
            state: saved.state,
            ram_file: std::sync::Arc::new(ram),
        };
        restore
            .validate(usize::from(spec.cpus))
            .map_err(anyhow::Error::msg)?;
        Ok(restore)
    }

    struct CaptureLayers<'a> {
        pool: Option<&'a Path>,
        readonly: std::collections::BTreeSet<PathBuf>,
        reuse: &'a [FilesystemReuse],
        copies: BTreeMap<PathBuf, PathBuf>,
        layers: Vec<crate::environment_snapshot::CapturedFilesystemLayer>,
        private_sources: Vec<crate::environment_snapshot::CapturedFilesystemSource>,
    }

    #[cfg(test)]
    #[test]
    fn direct_private_capture_creates_no_data_tree_and_rejects_forged_launch_slots() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let make = |name: &str| {
            let path = root.join(name);
            fs::create_dir_all(&path).unwrap();
            path
        };
        let device = OverlayDeviceSpec {
            lowers: vec![make("lower")],
            upper: make("upper"),
            work: Some(make("work")),
            preimages: Some(make("preimages")),
            apply_target: Some(make("target")),
            baseline_lower: Some(make("baseline")),
            excluded: vec![],
            access_policy: Default::default(),
        };
        fs::write(device.upper.join("data"), b"private writable backing").unwrap();
        fs::hard_link(device.upper.join("data"), device.upper.join("alias")).unwrap();
        fs::set_permissions(device.upper.join("data"), fs::Permissions::from_mode(0o640)).unwrap();
        let before = fs::metadata(device.upper.join("data")).unwrap();
        let binding = LaunchBinding {
            store: make("store"),
            filesystem_pool: Some(make("pool")),
            readonly_lowers: capture_lower_bindings(&device, Some(&device)).unwrap(),
            private_roots: capture_private_bindings(&device, Some(&device)).unwrap(),
            compatibility: Compatibility {
                host_boot: "boot".into(),
                build: "build".into(),
                firmware: "firmware".into(),
                profile: "direct-source".into(),
            },
            run_id: "run".into(),
            attempt_id: "attempt".into(),
        };
        let capture = make("capture/rootfs");
        let mut collected = CaptureLayers {
            pool: binding.filesystem_pool.as_deref(),
            readonly: readonly_roots(&device, Some(&device)),
            reuse: &[],
            copies: BTreeMap::new(),
            layers: Vec::new(),
            private_sources: Vec::new(),
        };
        assert_eq!(
            collect(&device, &capture, &mut collected).unwrap().upper,
            device.upper
        );
        collect(&device, &capture, &mut collected).unwrap();
        assert_eq!(fs::read_dir(&capture).unwrap().count(), 0);
        assert_eq!(collected.layers.len(), 1);
        assert_eq!(collected.private_sources.len(), 5);
        let private = private_roots(&device, Some(&device));
        validate_private_source_bindings(&private, &collected.private_sources, &binding).unwrap();
        for fault in 0..6 {
            let mut forged = collected.private_sources.clone();
            match fault {
                0 => forged[0].source = root.join("foreign"),
                1 => forged[0].path = "layer-000".into(),
                2 => {
                    forged.pop();
                }
                3 => forged.push(forged[0].clone()),
                4 => forged[0].path = "layer-001/../layer-001".into(),
                5 => forged[0].source = device.lowers[0].clone(),
                _ => unreachable!(),
            }
            assert!(
                validate_private_source_bindings(&private, &forged, &binding).is_err(),
                "fault {fault}"
            );
        }
        let after = fs::metadata(device.upper.join("data")).unwrap();
        assert_eq!(
            (
                before.ino(),
                before.nlink(),
                before.mode(),
                before.ctime(),
                before.ctime_nsec()
            ),
            (
                after.ino(),
                after.nlink(),
                after.mode(),
                after.ctime(),
                after.ctime_nsec()
            )
        );
        assert_eq!(
            fs::read(device.upper.join("data")).unwrap(),
            b"private writable backing"
        );
    }

    fn collect(
        device: &OverlayDeviceSpec,
        directory: &Path,
        captured: &mut CaptureLayers<'_>,
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
            if let Some(copied) = captured.copies.get(source) {
                return Ok(copied.clone());
            }
            ensure!(
                captured.copies.len() < 128,
                "too many checkpoint backing directories"
            );
            let path = PathBuf::from(format!("layer-{:03}", captured.copies.len()));
            let immutable = captured.pool.is_some() && captured.readonly.contains(source);
            let reused = immutable
                .then(|| {
                    captured
                        .reuse
                        .iter()
                        .find(|reuse| reuse.slot as usize == captured.copies.len())
                        .map(|reuse| reuse.id.clone())
                })
                .flatten();
            let direct = captured.pool.is_some();
            let copied = if direct {
                source.to_owned()
            } else {
                let copied = directory.join(&path);
                copy_owned_tree(source, &copied)
                    .with_context(|| format!("copy frozen backing {}", source.display()))?;
                copied
            };
            if immutable {
                captured
                    .layers
                    .push(crate::environment_snapshot::CapturedFilesystemLayer {
                        path,
                        source: copied.clone(),
                        id: reused,
                    });
            } else if direct {
                captured.private_sources.push(
                    crate::environment_snapshot::CapturedFilesystemSource {
                        path,
                        source: source.to_owned(),
                    },
                );
            }
            captured.copies.insert(source.to_owned(), copied.clone());
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

    pub(in crate::executor::vm) fn capture(
        spec: &RunnerSpec,
        vm: &mut krun_vmm::Vmm,
        directory: &Path,
        storage: SnapshotRamStorage,
        delta: Option<&krun_vmm::snapshot::RamDeltaSpec>,
        filesystem_reuse: &[FilesystemReuse],
    ) -> anyhow::Result<CaptureReady> {
        let binding = spec
            .checkpoint
            .as_ref()
            .context("Job has no durable checkpoint binding")?;
        ensure!(
            filesystem_reuse.len() <= 128
                && filesystem_reuse.iter().all(|reuse| reuse.slot < 128
                    && reuse.id.len() == 64
                    && reuse
                        .id
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    && binding
                        .readonly_lowers
                        .iter()
                        .any(|lower| lower.slot == reuse.slot))
                && filesystem_reuse
                    .iter()
                    .map(|reuse| reuse.slot)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == filesystem_reuse.len()
                && filesystem_reuse
                    .iter()
                    .map(|reuse| &reuse.id)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == filesystem_reuse.len()
                && (filesystem_reuse.is_empty() || binding.filesystem_pool.is_some()),
            "invalid supervisor-pinned filesystem reuse set"
        );
        if binding.filesystem_pool.is_some() {
            ensure!(
                binding.readonly_lowers
                    == capture_lower_bindings(&spec.root, spec.workspace.as_ref())?,
                "native read-only launch slots changed"
            );
            ensure!(
                binding.private_roots
                    == capture_private_bindings(&spec.root, spec.workspace.as_ref())?,
                "native private launch slots changed"
            );
        }
        validate_pool_binding(binding, &spec.root, spec.workspace.as_ref())?;
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
        ensure!(
            delta.is_none() || storage == SnapshotRamStorage::Compressed && spec.restore.is_some(),
            "incremental capture requires a compressed restored baseline"
        );
        let (mut state, ram_delta) = vm
            .capture_machine_state_with_ram_delta(&ram, delta)
            .map_err(anyhow::Error::msg)?;
        ram.sync_all()?;
        let created_at_unix_ms = crate::unix_now_ms();
        let rootfs = directory.join("rootfs");
        fs::create_dir(&rootfs)?;
        let mut captured = CaptureLayers {
            pool: binding.filesystem_pool.as_deref(),
            readonly: readonly_roots(&spec.root, spec.workspace.as_ref()),
            reuse: filesystem_reuse,
            copies: BTreeMap::new(),
            layers: Vec::new(),
            private_sources: Vec::new(),
        };
        let root = collect(&spec.root, &rootfs, &mut captured)?;
        let workspace = spec
            .workspace
            .as_ref()
            .map(|device| collect(device, &rootfs, &mut captured))
            .transpose()?;
        ensure!(
            captured
                .layers
                .iter()
                .filter(|layer| layer.id.is_some())
                .count()
                == filesystem_reuse.len(),
            "capture did not retain all pinned lowers"
        );
        let copies = captured.copies.into_iter().collect::<Vec<_>>();
        let mut tags = vec!["/dev/root"];
        if workspace.is_some() {
            tags.push("pvisor-workspace");
        }
        for name in tags {
            let mut tag = [0; 36];
            tag[..name.len()].copy_from_slice(name.as_bytes());
            let mut count = 0;
            if binding.filesystem_pool.is_some() {
                for mapping in &state.devices {
                    if let BusDeviceSnapshot::Virtio(device) = &mapping.device {
                        count += usize::from(device.verify_frozen_filesystem_backing(&tag)?);
                    }
                }
                ensure!(
                    count == 1,
                    "checkpoint must verify exactly one {name} filesystem"
                );
                continue;
            }
            let device = if name == "/dev/root" {
                &spec.root
            } else {
                spec.workspace.as_ref().context("missing workspace")?
            };
            let shared = if binding.filesystem_pool.is_some() {
                device
                    .lowers
                    .iter()
                    .filter(|path| captured.readonly.contains(*path))
                    .cloned()
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            for mapping in &mut state.devices {
                if let BusDeviceSnapshot::Virtio(device) = &mut mapping.device {
                    count += usize::from(if shared.is_empty() {
                        device.rebind_filesystem_layers(&tag, &copies)?
                    } else {
                        device.rebind_filesystem_shared_lowers(&tag, &copies, &shared)?
                    });
                }
            }
            ensure!(
                count == 1,
                "checkpoint must bind exactly one {name} filesystem"
            );
        }
        let saved = SavedExecution {
            private_sources: captured.private_sources,
            filesystem_layers: captured.layers,
            ram_delta,
            cpu_qos: spec.cpu_qos,
            version: if binding.filesystem_pool.is_some() {
                2
            } else {
                1
            },
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
