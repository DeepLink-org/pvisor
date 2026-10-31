//! Generation-scoped bind stages. Catalog admission never waits for a Job lock.
use super::*;
use crate::runtime::{self, JobDirectoryLock, RunRecord, StagedRunLease};
use pvisor_core::overlay::{OverlayRecord, OverlayState};

const BINDING: &str = "pvisor.docker_staged_bind";

/// One explicit bind mount in the current container generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedMount {
    /// Exact absolute guest mount selector; not a host path or prefix.
    pub target: PathBuf,
    /// Canonical host directory whose changes may be accepted.
    pub source: PathBuf,
    /// Independent Job stage directory; absent before the first start.
    pub stage: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Binding {
    version: u32,
    store: PathBuf,
    instance_id: String,
    generation: String,
    mount_index: usize,
    guest_target: PathBuf,
    source: PathBuf,
}

pub(super) fn validate_mounts(config: &mut Create) -> anyhow::Result<()> {
    let staged = config.mounts.iter().any(|m| m.staged);
    if !staged {
        return Ok(());
    }
    ensure!(
        cfg!(target_os = "linux"),
        "staged bind mounts require Linux"
    );
    ensure!(
        !config.auto_remove,
        "--rm conflicts with staged bind mounts; remove explicitly after review"
    );
    for mount in &mut config.mounts {
        validate_target(&mount.target)?;
        ensure!(
            mount.source.is_absolute() && mount.source.is_dir(),
            "mount source must be an existing absolute directory"
        );
        mount.source = mount.source.canonicalize()?;
        if mount.staged {
            ensure!(
                !mount.read_only && mount.volume.is_none(),
                "stage requires a writable explicit bind, not ro or a named/anonymous volume"
            );
            ensure!(
                !is_root_identity(&fs::metadata(&mount.source)?)?,
                "staged bind source cannot be '/'"
            );
        }
    }
    for (index, a) in config.mounts.iter().enumerate() {
        for b in &config.mounts[index + 1..] {
            if a.staged || b.staged {
                ensure!(
                    !overlap(&a.source, &b.source) && !overlap(&a.target, &b.target),
                    "mounts involving stage must not overlap host sources or guest targets"
                );
            }
        }
    }
    // Distinct paths (even distinct directory inodes) do not prove independence:
    // bind mounts and cross-tree hardlinks can expose the same writable lower.
    ensure!(
        config.mounts.iter().all(|m| m.staged || m.read_only),
        "staged mounts cannot be mixed with direct-writable mounts; source independence cannot be assured"
    );
    Ok(())
}
fn is_root_identity(metadata: &fs::Metadata) -> anyhow::Result<bool> {
    let root = fs::metadata("/")?;
    Ok(metadata.dev() == root.dev() && metadata.ino() == root.ino())
}
fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
fn path(store: &Store, id: &str, generation: &str, index: usize) -> PathBuf {
    store
        .directory(id)
        .join("stages")
        .join(generation)
        .join(index.to_string())
}
fn binding(store: &Store, instance: &Instance, index: usize, mount: &Mount) -> Binding {
    Binding {
        version: 1,
        store: store.root.clone(),
        instance_id: instance.id.clone(),
        generation: instance.generation.clone(),
        mount_index: index,
        guest_target: mount.target.clone(),
        source: mount.source.clone(),
    }
}
impl Store {
    /// Describe explicit staged binds. This is metadata, not operation admission;
    /// use `staged_mount_stage` and shared Job service checks for review/apply/drop.
    pub fn staged_mounts(&self, selector: &str) -> anyhow::Result<Vec<StagedMount>> {
        let _catalog = self.lock()?;
        let instance = self.resolve(selector)?;
        Ok(instance
            .config
            .mounts
            .iter()
            .enumerate()
            .filter(|(_, m)| m.staged)
            .map(|(index, m)| StagedMount {
                target: m.target.clone(),
                source: m.source.clone(),
                stage: (!instance.generation.is_empty())
                    .then(|| path(self, &instance.id, &instance.generation, index)),
            })
            .collect())
    }
    /// Resolve a stopped, receipt-confirmed staged bind under the catalog lock.
    /// Omission is valid only for exactly one staged bind; otherwise `mount` must
    /// equal its complete guest target. Ordinary binds/volumes never select a Job.
    /// Resolution observes existing locks without creating them or changing their
    /// diagnostic records. Missing locks fail closed for every caller.
    /// Returned paths confer no durable admission: the Job service rechecks the
    /// container binding under its operation lease, so restart/removal fail closed.
    pub fn staged_mount_stage(
        &self,
        selector: &str,
        mount: Option<&Path>,
    ) -> anyhow::Result<PathBuf> {
        let _catalog = self.lock()?;
        let instance = self.resolve(selector)?;
        let staged: Vec<_> = instance
            .config
            .mounts
            .iter()
            .enumerate()
            .filter(|(_, m)| m.staged)
            .collect();
        let (index, selected) = if let Some(target) = mount {
            staged.iter().copied().find(|(_, m)| m.target == target).context("--mount must exactly name a staged bind guest target; ordinary mounts cannot be reviewed/applied/dropped")?
        } else {
            ensure!(
                staged.len() == 1,
                "container requires exactly one staged bind to omit --mount; select an exact guest target"
            );
            staged[0]
        };
        ensure!(
            !instance.generation.is_empty(),
            "container has no staged generation; start it first"
        );
        stopped(self, &instance)?;
        let stage = path(self, &instance.id, &instance.generation, index);
        let selected_record = RunRecord::read(&stage)?;
        let _directory = JobDirectoryLock::observe_existing(&selected_record)?;
        let (record, _lease) = selected_record.observe_current()?;
        validate_record(&record, &binding(self, &instance, index, selected))?;
        record.require_stopped()?;
        Ok(stage)
    }
    /// Recheck a container Job's owner/generation binding. Shared Job operations
    /// invoke this automatically, including when given a stage path directly.
    /// Native Jobs without a Docker binding are unaffected. Never hold this lock
    /// over frontend interaction; start/remove take nonblocking Job locks.
    pub fn check_bound_staged_record(record: &RunRecord) -> anyhow::Result<()> {
        let Some(value) = record.orchestration.get(BINDING) else {
            return Ok(());
        };
        let bound: Binding = serde_json::from_value(value.clone())?;
        ensure!(
            bound.store.is_absolute() && bound.store.is_dir(),
            "container store no longer exists"
        );
        let store = Store::open(bound.store.clone())?;
        let _catalog = store.lock()?;
        let instance = store.resolve(&bound.instance_id)?;
        stopped(&store, &instance)?;
        ensure!(
            instance.generation == bound.generation,
            "stale container generation stage"
        );
        let mount = instance
            .config
            .mounts
            .get(bound.mount_index)
            .context("staged mount no longer exists")?;
        ensure!(
            mount.staged && binding(&store, &instance, bound.mount_index, mount) == bound,
            "container staged mount binding changed"
        );
        validate_record(record, &bound)
    }
}
fn stopped(store: &Store, instance: &Instance) -> anyhow::Result<()> {
    ensure!(
        !store.active(instance)? && instance.state == "exited",
        "container owner is active or generation has no confirmed exit"
    );
    let result: Completion = serde_json::from_reader(File::open(
        store
            .root
            .join("results")
            .join(format!("{}.json", instance.id)),
    )?)?;
    ensure!(
        result.generation == instance.generation,
        "container generation completion missing or stale"
    );
    Ok(())
}
fn validate_record(record: &RunRecord, bound: &Binding) -> anyhow::Result<()> {
    ensure!(bound.version == 1, "unsupported staged bind contract");
    ensure!(
        record.stage_dir()
            == bound
                .store
                .join("instances")
                .join(&bound.instance_id)
                .join("stages")
                .join(&bound.generation)
                .join(bound.mount_index.to_string()),
        "container stage directory binding changed"
    );
    let actual: Binding = serde_json::from_value(
        record
            .orchestration
            .get(BINDING)
            .context("missing container binding")?
            .clone(),
    )?;
    ensure!(&actual == bound, "container stage orchestration mismatch");
    let scope = record
        .project_apply_scope
        .as_ref()
        .context("missing staged project scope")?;
    ensure!(scope.version == 1, "unsupported staged project scope");
    ensure!(
        scope.root == bound.source
            && record.workspace.as_ref() == Some(&bound.source)
            && record
                .overlay
                .as_ref()
                .is_some_and(|o| o.target == bound.source
                    && o.upper.upper_dir == record.stage_dir().join("upper")
                    && o.upper.work_dir == record.stage_dir().join("work")
                    && o.baseline_lower.is_none()
                    && !o.auto_apply)
            && record.overlay_lowers == [bound.source.clone()],
        "staged bind apply authority changed"
    );
    ensure!(
        record.completion_protocol == Some(1),
        "container stage requires completion receipt"
    );
    if runtime::completion::Completion::load(record)?.is_some() {
        let bundle = runtime::bundle::RunBundle::read(&record.stage_dir())?;
        ensure!(
            bundle.orchestration == record.orchestration
                && bundle.project_apply_scope == record.project_apply_scope,
            "container binding contradicts committed Bundle"
        );
    }
    Ok(())
}

pub(super) struct Stage {
    record: RunRecord,
    _directory: JobDirectoryLock,
    _lease: StagedRunLease,
}

pub(super) fn prepare(store: &Store, instance: &Instance) -> anyhow::Result<Vec<Stage>> {
    let mut stages = Vec::new();
    for (index, mount) in instance
        .config
        .mounts
        .iter()
        .enumerate()
        .filter(|(_, m)| m.staged)
    {
        let stage = path(store, &instance.id, &instance.generation, index);
        private_directory(stage.parent().unwrap())?;
        private_directory(&stage)?;
        let lease = StagedRunLease::acquire_new(&stage)?;
        let id = uuid();
        let scope = crate::job_service::ProjectApplyScope::capture(&mount.source)?;
        let overlay: OverlayRecord = serde_json::from_value(serde_json::json!({
            "id":id,"generation":0,"target":mount.source,
            "upper":{"upper_dir":stage.join("upper"),"work_dir":stage.join("work")},
            "merged_dir":stage.join("merged"),"stage_dir":stage,"auto_apply":false,"state":"active"
        }))?;
        let overlay = runtime::prepare_overlay_record_mountless(
            &overlay,
            std::slice::from_ref(&mount.source),
            &id,
            pvisor_core::overlay::StageDurability::Checkpoint,
            pvisor_core::overlay::StageRecording::On,
        )?;
        runtime::save_lower_mutability(
            &stage,
            std::slice::from_ref(&mount.source),
            &[pvisor_overlay_core::LayerMutability::Mutable],
        )?;
        let record: RunRecord = serde_json::from_value(serde_json::json!({
            "schema_version":2,"completion_protocol":1,"run_id":id,"attempt_id":uuid(),
            "session_id":instance.id,"agent":"docker-staged-bind-owner","pid":std::process::id(),
            "command":instance.argv,"state":"running","started_at_unix_ms":instance.started_ms,
            "finished_at_unix_ms":null,"storage":stage,"workspace":mount.source,
            "project_apply_scope":scope,"gateway_listen":null,"network":{},"overlay":overlay,
            "overlay_lowers":[mount.source],"orchestration":{BINDING:binding(store, instance, index, mount)}
        }))?;
        record.write()?;
        record.initialize_apply_observation_locks()?;
        let directory = JobDirectoryLock::acquire(&record, "docker-generation-owner")?;
        stages.push(Stage {
            record,
            _directory: directory,
            _lease: lease,
        });
    }
    Ok(stages)
}

pub(super) fn complete(stages: &mut [Stage], code: i32) -> anyhow::Result<()> {
    for stage in stages {
        let record = &mut stage.record;
        runtime::stage_overlay_record(record.overlay.as_mut().context("missing stage overlay")?)?;
        let result: pvisor_core::RunResult = serde_json::from_value(serde_json::json!({
            "run_id":record.run_id,"attempt_id":record.attempt_id,"state":if code == 0 {"completed"} else {"failed"},
            "started_at_unix_ms":record.started_at_unix_ms,"finished_at_unix_ms":crate::unix_now_ms(),"exit_code":code,
            "warnings":["Container owner observed VMM exit and sealed this bind stage; no executor isolation attestation is claimed."]
        }))?;
        let snapshot = crate::AgentCtlSnapshot {
            run_id: record.run_id.clone(),
            attempt_id: record.attempt_id.clone().unwrap(),
            directive: pvisor_core::protocol::AgentDirective::Continue,
            clients: vec![],
        };
        runtime::bundle::RunBundle::capture(record, &result, snapshot, false)?
            .write(&record.stage_dir())?;
        runtime::completion::Completion::commit(record, &result)?;
        runtime::completion::Completion::load(record)?
            .context("completion receipt missing")?
            .project(record)?;
        record.write()?;
    }
    Ok(())
}

// Written only on a path which never successfully spawned a VMM. It permits
// catalog cleanup, never Job review/apply/drop or completion synthesis.
pub(super) fn failed_admission(store: &Store, instance: &Instance) -> anyhow::Result<()> {
    crate::util::write_private_json(
        &store.directory(&instance.id).join("failed-admission.json"),
        &instance.generation,
    )
}

pub(super) struct PreviousStages {
    _stages: Vec<Stage>,
    _partial: Vec<StagedRunLease>,
}

// Both catalog mutations hold these nonblocking leases through publication or
// deletion. Operations holding a stage may briefly acquire the catalog safely.
pub(super) fn lock_previous(
    store: &Store,
    instance: &Instance,
    removing: bool,
) -> anyhow::Result<PreviousStages> {
    let mut previous = PreviousStages {
        _stages: vec![],
        _partial: vec![],
    };
    if instance.generation.is_empty() {
        return Ok(previous);
    }
    let failed = match File::open(store.directory(&instance.id).join("failed-admission.json")) {
        Ok(file) => serde_json::from_reader::<_, String>(file)? == instance.generation,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if failed {
        stopped(store, instance)?;
    }
    for (index, mount) in instance
        .config
        .mounts
        .iter()
        .enumerate()
        .filter(|(_, m)| m.staged)
    {
        let stage = path(store, &instance.id, &instance.generation, index);
        if failed {
            // Even a proven non-execution must not erase later recovery evidence.
            for name in ["apply-ledger.json", "completion.json", "run-bundle.json"] {
                ensure!(
                    !stage.join(name).try_exists()?,
                    "failed admission has recovery/completion evidence; reconcile before start/remove"
                );
            }
            if !stage.try_exists()? {
                continue;
            }
            if !stage.join("run.json").try_exists()? {
                let lease = StagedRunLease::acquire(&stage)?;
                for entry in fs::read_dir(&stage)? {
                    let entry = entry?;
                    let name = entry.file_name();
                    let kind = entry.file_type()?;
                    ensure!(
                        (name == "lease.lock" && kind.is_file())
                            || (["upper", "work", "merged", "preimages"]
                                .iter()
                                .any(|allowed| name == *allowed)
                                && kind.is_dir()),
                        "partial admission has unknown evidence; reconcile before start/remove"
                    );
                }
                for name in ["upper", "work", "merged", "preimages"] {
                    let directory = stage.join(name);
                    if directory.try_exists()? {
                        ensure!(
                            fs::read_dir(directory)?.next().is_none(),
                            "partial admission has unknown workload evidence; reconcile before start/remove"
                        );
                    }
                }
                previous._partial.push(lease);
                continue;
            }
        }
        let selected = RunRecord::read(&stage)?;
        let directory = JobDirectoryLock::acquire(&selected, "docker-generation-transition")?;
        let (record, lease) = selected.lock_current()?;
        validate_record(&record, &binding(store, instance, index, mount))?;
        if !failed {
            record.require_stopped()?;
        }
        let overlay = record.overlay.as_ref().context("missing stage overlay")?;
        ensure!(
            !pvisor_overlay_core::apply::has_pending_applies(overlay)?,
            "container stage has pending apply ledger; reconcile before start/remove"
        );
        if failed {
            ensure!(
                overlay.state == OverlayState::Active,
                "failed admission has unexpected stage state; reconcile before start/remove"
            );
        }
        if (failed || !removing)
            && !matches!(
                overlay.state,
                OverlayState::Applied | OverlayState::Discarded
            )
        {
            ensure!(
                runtime::overlay_changes(overlay, &record.overlay_lowers)?.is_empty(),
                "container has unaccepted staged changes; apply/drop before starting a new generation"
            );
        }
        previous._stages.push(Stage {
            record,
            _directory: directory,
            _lease: lease,
        });
    }
    Ok(previous)
}

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
pub(super) fn attach(
    vm: &mut pvisor_vm::api::VmBuilder,
    store: &Store,
    id: &str,
    generation: &str,
    index: usize,
    mount: &Mount,
    tag: &str,
) -> anyhow::Result<()> {
    use pvisor_vm::api::{OverlayConfig, PermissionSemantics, VmConfiguration};
    let record = RunRecord::read(&path(store, id, generation, index))?;
    validate_record(
        &record,
        &Binding {
            version: 1,
            store: store.root.clone(),
            instance_id: id.into(),
            generation: generation.into(),
            mount_index: index,
            guest_target: mount.target.clone(),
            source: mount.source.clone(),
        },
    )?;
    record
        .project_apply_scope
        .as_ref()
        .context("missing staged project scope")?
        .validate()?;
    let overlay = record.overlay.context("missing bind overlay")?;
    vm.overlay(
        tag,
        OverlayConfig {
            lower_dirs: vec![mount.source.clone()],
            lower_mutability: vec![pvisor_overlay_core::LayerMutability::Mutable],
            upper_dir: overlay.upper.upper_dir,
            work_dir: Some(overlay.upper.work_dir),
            preimage_dir: Some(overlay.stage_dir.join("preimages")),
            apply_target: Some(mount.source.clone()),
            baseline_lower: None,
            baseline_content_index: None,
            excluded_paths: vec![],
            access_policy: Default::default(),
            semantics: PermissionSemantics::LinuxComplete,
        },
        0,
    )?;
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn root_identity_is_inode_based() {
        assert!(is_root_identity(&fs::metadata("/").unwrap()).unwrap());
        let directory = tempfile::tempdir().unwrap();
        assert!(!is_root_identity(&fs::metadata(directory.path()).unwrap()).unwrap());
    }

    #[test]
    fn root_bind_alias_child() {
        let Some(alias) = std::env::var_os("PVISOR_TEST_ROOT_BIND_ALIAS") else {
            return;
        };
        let alias = PathBuf::from(alias);
        assert_ne!(alias, Path::new("/"));
        assert!(is_root_identity(&fs::metadata(&alias).unwrap()).unwrap());
        assert!(
            crate::job_service::ProjectApplyScope::capture(&alias)
                .unwrap_err()
                .to_string()
                .contains("identity")
        );
        let mut config: Create = serde_json::from_value(serde_json::json!({
            "image":"unused", "command":[], "env":{}, "ports":[], "cpus":1,
            "memory_mib":32, "auto_remove":false, "interactive":false, "network":false,
            "mounts":[{"source":alias,"target":"/workspace","read_only":false,"staged":true,"volume":null}]
        })).unwrap();
        assert!(
            validate_mounts(&mut config)
                .unwrap_err()
                .to_string()
                .contains("cannot be '/'")
        );
    }

    #[test]
    fn root_bind_alias_is_refused_in_private_mount_namespace_when_available() {
        let probe = Command::new("unshare")
            .args(["--user", "--map-root-user", "--mount", "true"])
            .output();
        if !probe.as_ref().is_ok_and(|output| output.status.success()) {
            eprintln!(
                "root bind-alias integration unavailable: user/mount namespaces or unshare unavailable"
            );
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let output = Command::new("unshare")
            .args(["--user", "--map-root-user", "--mount", "sh", "-c", "mount --bind / \"$1\" || exit 77; exec \"$2\" --exact docker::staging::tests::root_bind_alias_child --nocapture", "root-bind-test"])
            .arg(directory.path())
            .arg(std::env::current_exe().unwrap())
            .env("PVISOR_TEST_ROOT_BIND_ALIAS", directory.path())
            .output().unwrap();
        if output.status.code() == Some(77) {
            eprintln!(
                "root bind-alias integration unavailable: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
