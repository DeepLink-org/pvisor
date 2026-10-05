//! Host-owned repositories. Native outcome is sealed before publication and a
//! target imports only a controller-bound receipt, never a task-selected URL.
use super::*;
use std::fs;

#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Profile {
    pub repository: String,
    pub backend: Backend,
    pub location: String,
    #[serde(default)]
    pub publish: bool,
}
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Backend {
    Filesystem,
    S3,
}

pub(super) struct Repository {
    pub support: CheckpointStorageSupport,
    #[cfg(target_os = "linux")]
    engine: pvisor::environment_snapshot::SnapshotRepository,
    #[cfg(target_os = "linux")]
    filesystem_pool: Option<PathBuf>,
    producers: Arc<tokio::sync::Semaphore>,
}
impl Repository {
    pub fn validate_import(
        state: &Path,
        checkpoint: &pvisor_core::operation::ExecutionCheckpoint,
        publication: &CheckpointPublication,
    ) -> anyhow::Result<()> {
        #[cfg(target_os = "linux")]
        {
            publication.validate()?;
            let expected = state
                .join("checkpoint-imports")
                .join(&checkpoint.snapshot_id)
                .canonicalize()?;
            ensure!(
                checkpoint.store.canonicalize()? == expected
                    && expected.starts_with(state.join("checkpoint-imports")),
                "checkpoint is outside its owned import storage"
            );
            ensure!(
                record::<CheckpointPublication>(
                    &expected.join("source.json"),
                    ARTIFACT_CHUNK_BYTES as u64
                )? == *publication,
                "checkpoint import provenance mismatch"
            );
            let mut original = checkpoint.clone();
            original.store = publication.checkpoint.store.clone();
            ensure!(
                original == publication.checkpoint,
                "checkpoint import native binding mismatch"
            );
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (state, checkpoint, publication);
            anyhow::bail!("checkpoint repositories require Linux")
        }
    }
    pub fn new(profile: &Profile, vm: &pvisor::VmSettings) -> anyhow::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let support = CheckpointStorageSupport {
                version: 1,
                repository: profile.repository.clone(),
                publish: profile.publish,
                compatibility: VmExecutor::checkpoint_compatibility(vm)?,
            };
            support.validate()?;
            let engine = match profile.backend {
                Backend::Filesystem => {
                    pvisor::environment_snapshot::SnapshotRepository::filesystem(
                        Path::new(&profile.location),
                        !profile.publish,
                    )?
                }
                Backend::S3 => pvisor::environment_snapshot::SnapshotRepository::s3(
                    &profile.location,
                    !profile.publish,
                )?,
            };
            Ok(Self {
                support,
                engine,
                filesystem_pool: vm.snapshot_filesystem_pool.clone(),
                producers: Arc::new(tokio::sync::Semaphore::new(1)),
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (profile, vm);
            anyhow::bail!("checkpoint repositories require Linux")
        }
    }
    pub async fn permit(&self) -> anyhow::Result<tokio::sync::OwnedSemaphorePermit> {
        Ok(self.producers.clone().acquire_owned().await?)
    }
    pub fn publish(
        &self,
        storage: &Path,
        result: &pvisor_core::RunResult,
        requirement: &CheckpointRetention,
    ) -> anyhow::Result<CheckpointPublication> {
        #[cfg(target_os = "linux")]
        {
            ensure!(
                self.support.publish && self.support.repository == requirement.repository,
                "worker cannot publish requested checkpoint repository"
            );
            let receipt = pvisor_core::operation::ExecutionSuspension::from_result(result)?;
            let checkpoint = receipt.checkpoint;
            ensure!(
                checkpoint.store == storage.join("execution-snapshots")
                    && checkpoint.source_run_id == result.run_id.as_str()
                    && checkpoint.source_attempt_id == result.attempt_id.as_str(),
                "suspended checkpoint is outside its native Attempt"
            );
            let header = record::<pvisor::environment_snapshot::EnvironmentManifest>(
                &checkpoint
                    .store
                    .join("objects")
                    .join(&checkpoint.snapshot_id)
                    .join("manifest.json"),
                16 * 1024 * 1024,
            )?;
            let store = self.store(&checkpoint.store)?;
            let published = store.open(&checkpoint.snapshot_id, &header.compatibility)?;
            let transfer = self.engine.publish(&published)?;
            let publication = CheckpointPublication {
                version: 1,
                repository: requirement.repository.clone(),
                checkpoint,
                transfer,
                compatibility: header.compatibility,
            };
            publication.validate()?;
            Ok(publication)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (storage, result, requirement);
            anyhow::bail!("checkpoint repositories require Linux")
        }
    }
    pub async fn import(
        self: &Arc<Self>,
        state: &Path,
        publication: &CheckpointPublication,
    ) -> anyhow::Result<pvisor_core::operation::ExecutionCheckpoint> {
        publication.validate()?;
        ensure!(
            publication.repository == self.support.repository
                && publication.compatibility == self.support.compatibility,
            "checkpoint repository or actual host compatibility mismatch"
        );
        let permit = self.permit().await?;
        let repository = self.clone();
        let state = state.to_owned();
        let publication = publication.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            repository.import_blocking(&state, &publication)
        })
        .await?
    }
    fn import_blocking(
        &self,
        state: &Path,
        publication: &CheckpointPublication,
    ) -> anyhow::Result<pvisor_core::operation::ExecutionCheckpoint> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            let parent = state.join("checkpoint-imports");
            match fs::DirBuilder::new().mode(0o700).create(&parent) {
                Ok(()) => fs::File::open(state)?.sync_all()?,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            let metadata = fs::symlink_metadata(&parent)?;
            ensure!(
                metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
                "checkpoint import directory must be private and owned"
            );
            let root = parent.join(&publication.checkpoint.snapshot_id);
            let store = self.store(&root)?;
            let marker = root.join("source.json");
            if marker.try_exists()? {
                ensure!(
                    record::<CheckpointPublication>(&marker, ARTIFACT_CHUNK_BYTES as u64)?
                        == *publication,
                    "imported checkpoint provenance mismatch"
                );
                store.open_for_restore(
                    &publication.checkpoint.snapshot_id,
                    &self.support.compatibility,
                )?;
            } else {
                self.engine
                    .import(&store, &publication.transfer, &self.support.compatibility)?;
                super::persist(&marker, publication)?;
            }
            let mut checkpoint = publication.checkpoint.clone();
            checkpoint.store = root.canonicalize()?;
            Ok(checkpoint)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (state, publication);
            anyhow::bail!("checkpoint repositories require Linux")
        }
    }
    #[cfg(target_os = "linux")]
    fn store(&self, root: &Path) -> anyhow::Result<pvisor::environment_snapshot::SnapshotStore> {
        match self.filesystem_pool.as_deref() {
            Some(pool) => {
                pvisor::environment_snapshot::SnapshotStore::with_filesystem_pool(root, pool)
            }
            None => pvisor::environment_snapshot::SnapshotStore::new(root),
        }
    }
}

#[cfg(target_os = "linux")]
fn record<T: serde::de::DeserializeOwned>(path: &Path, maximum: u64) -> anyhow::Result<T> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let input = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    ensure!(
        input.metadata()?.is_file() && input.metadata()?.len() <= maximum,
        "invalid checkpoint provenance record"
    );
    let mut bytes = Vec::new();
    input.take(maximum + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= maximum,
        "checkpoint provenance record exceeds limit"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_and_expired_preparation_joins_the_checkpoint_producer_without_starting_a_run()
     {
        for expired in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let state = temp.path().join("worker");
            fs::create_dir(&state).unwrap();
            let support = CheckpointStorageSupport {
                version: 1,
                repository: "test".into(),
                publish: false,
                compatibility: pvisor_core::operation::SnapshotCompatibility {
                    host_boot: "boot".into(),
                    build: "a".repeat(64),
                    firmware: "b".repeat(64),
                    profile: "profile".into(),
                },
            };
            let repository = Arc::new(Repository {
                support: support.clone(),
                filesystem_pool: None,
                engine: pvisor::environment_snapshot::SnapshotRepository::filesystem(
                    &temp.path().join("remote"),
                    false,
                )
                .unwrap(),
                producers: Arc::new(tokio::sync::Semaphore::new(1)),
            });
            // Hold the actual repository's sole bulk producer. This Attempt
            // must keep its local reservation even after cancellation/expiry.
            let owner = repository.permit().await.unwrap();
            let checkpoint = pvisor_core::operation::ExecutionCheckpoint {
                snapshot_id: "c".repeat(64),
                store: temp.path().join("absent-source"),
                source_run_id: "source".into(),
                source_attempt_id: "native-source".into(),
                created_at_unix_ms: 1,
                ram_storage: pvisor_core::operation::SnapshotRamStorage::Compressed,
            };
            let publication = CheckpointPublication {
                version: 1,
                repository: support.repository,
                checkpoint: checkpoint.clone(),
                transfer: pvisor_core::operation::SnapshotTransfer {
                    version: 1,
                    snapshot_id: checkpoint.snapshot_id.clone(),
                    transfer_id: "d".repeat(64),
                },
                compatibility: support.compatibility,
            };
            let args = Arc::new(
                Args::try_parse_from([
                    "worker",
                    "--id",
                    "node",
                    "--token",
                    "test-worker-credential",
                    "--backend",
                    "vm",
                    "--state",
                    state.to_str().unwrap(),
                ])
                .unwrap(),
            );
            let profile = WorkerProfile {
                checkpoints: Some(repository),
                ..Default::default()
            };
            let runtime = AttemptRuntime {
                args: args.clone(),
                profile: Arc::new(profile),
                environments: None,
            };
            let mut run = pvisor_core::RunSpec::process("branch", "test", "/never-start");
            let RunInvocation::Process(process) = &mut run.invocation;
            process.inherit_env = false;
            run.parent_run_id = Some("source".into());
            let assignment = Assignment {
                spec: TaskSpec {
                    version: CLUSTER_VERSION,
                    id: "branch".into(),
                    tenant: "test".into(),
                    run,
                    execution: ExecutionClass {
                        executor: ExecutorKind::VirtualMachine,
                        isolation: IsolationKind::VirtualMachine,
                    },
                    resources: Resources {
                        slots: 1,
                        memory_bytes: 64 * 1024 * 1024,
                        cpu_millis: 1000,
                    },
                    labels: BTreeMap::new(),
                    cache_keys: vec![],
                    retain_bundle: false,
                    retain_artifacts: None,
                    gateway: None,
                    cpu_qos: None,
                    environment: None,
                    restore: Some(ExecutionRestore {
                        task_id: "source".into(),
                        request_id: "suspend".into(),
                    }),
                },
                lease: Lease {
                    key: LeaseKey {
                        task_id: "branch".into(),
                        generation: 1,
                        worker_id: "node".into(),
                        incarnation: "epoch".into(),
                    },
                    expires_at_ms: 1000,
                },
                environment: None,
                checkpoint: Some(checkpoint),
                checkpoint_publication: Some(publication),
            };
            let storage = state.join("tasks/branch-1");
            fs::create_dir_all(&storage).unwrap();
            let (stop, stop_rx) = watch::channel(false);
            let (lease, lease_rx) = watch::channel(Instant::now() + Duration::from_secs(60));
            let (_commands, commands_rx) = mpsc::channel(1);
            let (acknowledgements, _ack_rx) = mpsc::channel(1);
            let (native_done, mut native_rx) = mpsc::channel(1);
            let outbox = Arc::new(outbox::Outbox::open(&state, "node", &args.url).unwrap());
            let execution = execute(
                runtime,
                assignment,
                AttemptChannels {
                    stop: stop_rx,
                    lease_clock: lease_rx,
                    commands: commands_rx,
                    acknowledgements,
                    memory_ready: None,
                    native_done,
                },
                storage.clone(),
                Client::new(&args.url, args.token.clone()).unwrap(),
                outbox,
            );
            tokio::pin!(execution);
            assert!(
                tokio::time::timeout(Duration::from_millis(10), &mut execution)
                    .await
                    .is_err()
            );
            if expired {
                lease.send(Instant::now() - Duration::from_secs(1)).unwrap();
            } else {
                stop.send(true).unwrap();
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(10), &mut execution)
                    .await
                    .is_err(),
                "preparation released admission while its producer was still queued"
            );
            drop(owner);
            let completion = tokio::time::timeout(Duration::from_secs(2), execution)
                .await
                .unwrap();
            assert!(completion.result.is_none());
            assert!(completion.error.unwrap().contains(if expired {
                "preparation lease expired"
            } else {
                "preparation cancelled"
            }));
            assert!(
                pvisor::RunRecord::read(&storage).is_err(),
                "cancelled import must never start a native Run"
            );
            assert!(native_rx.try_recv().is_err());
            assert!(!storage.join("run-bundle.json").exists());
            assert!(storage.join("completion.json").exists());
        }
    }
}
