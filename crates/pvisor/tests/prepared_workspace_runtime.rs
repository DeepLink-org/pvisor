//! Owned workspace admission and unchanged public apply conflict contracts.
#![cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64")
))]

use pvisor::environment_snapshot::{BaseReference, SnapshotBase, SnapshotStore};
use pvisor::{OverlayHint, PVisor, VmExecutor, VmSettings};
use pvisor_core::{RunSpec, RunState};
use pvisor_vm::api::RuntimeSupport;
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    target: PathBuf,
    stage: PathBuf,
    base: SnapshotBase,
    rootfs: BaseReference,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let target = root.join("workspace");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("file"), b"baseline").unwrap();
        let root_source = root.join("root-source");
        fs::create_dir(&root_source).unwrap();
        let store = SnapshotStore::new(&root.join("workspace-store")).unwrap();
        let base = store.prepare_workspace(&target).unwrap();
        let rootfs = SnapshotStore::new(&root.join("rootfs-store"))
            .unwrap()
            .prepare_rootfs(&root_source)
            .unwrap()
            .reference()
            .clone();
        Self {
            _temp: temp,
            target,
            stage: root.join("stage"),
            root,
            base,
            rootfs,
        }
    }

    fn settings(&self) -> VmSettings {
        let mut settings = VmSettings {
            prepared_rootfs: Some(pvisor::PreparedRootfs {
                store: self.root.join("rootfs-store"),
                base: self.rootfs.clone(),
            }),
            ..Default::default()
        };
        if pvisor_vm::api::VmPlatform::embedded_kernel().is_none() {
            fs::write(
                self.root.join(pvisor_vm::api::VmPlatform::firmware_name()),
                b"fixture firmware, never executed",
            )
            .unwrap();
            settings.library_dir = Some(self.root.clone());
        }
        settings
    }

    fn executor(&self) -> Arc<VmExecutor> {
        Arc::new(VmExecutor::new(self.settings()).unwrap())
    }

    fn builder(&self) -> pvisor::PVisorBuilder {
        PVisor::builder()
            .storage(&self.stage)
            .with_workspace_base(
                self.base.try_clone().unwrap(),
                self.target.clone(),
                self.executor(),
            )
            .unwrap()
    }

    fn stopped_stage(&self) -> (pvisor_overlay_core::OverlayCore, pvisor::RunRecord) {
        use pvisor_overlay_core::{OverlayCore, OverlayLayout};
        pvisor_overlay_core::stage::begin(&self.stage.join("preimages"), Default::default())
            .unwrap();
        let core = OverlayCore::new_for_layout_with_compact_preimages(
            OverlayLayout::with_baseline(
                vec![self.base.root()],
                self.target.clone(),
                Some(&self.base.root()),
            )
            .unwrap(),
            self.stage.join("upper"),
            Some(self.stage.join("work")),
            vec![],
            Some(self.stage.join("preimages")),
        )
        .unwrap();
        let record: pvisor::RunRecord = serde_json::from_value(serde_json::json!({
            "schema_version":1,"run_id":"owned-apply","session_id":"owned-apply",
            "attempt_id":"attempt-owned","agent":"sh","pid":0,"command":["/bin/true"],
            "state":"completed","started_at_unix_ms":1,"finished_at_unix_ms":2,
            "storage":self.stage,"network":{},"overlay_lowers":[self.base.root()],
            "overlay":{"id":"owned-apply","generation":0,"target":self.target,"baseline_lower":self.base.root(),
                "upper":{"upper_dir":self.stage.join("upper"),"work_dir":self.stage.join("work")},
                "merged_dir":self.stage.join("merged"),"stage_dir":self.stage,"auto_apply":false,"state":"staged"}
        })).unwrap();
        pvisor::write_private_json(&self.stage.join("workspace-baseline.json"), &serde_json::json!({
            "version":1,"target":self.target,
            "workspace":{"store":self.root.join("workspace-store"),"base":self.base.reference()},
            "rootfs":{"store":self.root.join("rootfs-store"),"base":self.rootfs},
        })).unwrap();
        (core, record)
    }

    fn apply(
        &self,
        target: Option<PathBuf>,
    ) -> anyhow::Result<pvisor::job_service::MutationResponse> {
        pvisor::job_service::RuntimeJobService::apply(
            &pvisor::job_service::ServiceContext::default(),
            pvisor::job_service::ApplyRequest {
                job: pvisor::job_service::JobSelection {
                    selector: Some(self.stage.clone()),
                    storage: self.root.clone(),
                },
                target,
                selection: Default::default(),
                all: true,
            },
        )
    }
}

#[test]
fn owned_workspace_builder_authenticates_and_rejects_executor_replacement() {
    let f = Fixture::new();
    let builder = f.builder();
    assert!(
        builder
            .clone()
            .build()
            .resolve_operation(RunSpec::process("owned-valid", "sh", "/bin/true"))
            .is_ok()
    );
    assert!(
        !f.stage.exists(),
        "dry admission must not initialize a stage"
    );
    let error = builder
        .executors(vec![Arc::new(pvisor::ProcessExecutor::default())])
        .build()
        .resolve_operation(RunSpec::process("owned-replaced", "sh", "/bin/true"))
        .unwrap_err();
    assert!(error.to_string().contains("prepare failed"));
    assert!(!f.stage.exists());
    let clone = f.builder().build();
    fs::remove_file(
        f.root
            .join("workspace-store/prepared-workspace")
            .join(&f.base.reference().id)
            .join("reference"),
    )
    .unwrap();
    assert!(
        clone
            .resolve_operation(RunSpec::process("lost-pin", "sh", "/bin/true"))
            .is_err()
    );
    assert!(!f.stage.exists());
}

#[test]
fn owned_workspace_rejects_unretained_and_ordinary_rootfs_sources() {
    let f = Fixture::new();
    let unretained = SnapshotStore::new(&f.root.join("unretained-store"))
        .unwrap()
        .import_base(&f.target)
        .unwrap();
    assert!(
        PVisor::builder()
            .with_workspace_base(unretained, f.target.clone(), f.executor())
            .is_err()
    );
    let mut settings = f.settings();
    settings.prepared_rootfs = None;
    settings.rootfs = Some(f.root.join("root-source"));
    settings.rootfs_immutable = true;
    let ordinary = Arc::new(VmExecutor::new(settings).unwrap());
    assert!(
        PVisor::builder()
            .with_workspace_base(f.base.try_clone().unwrap(), f.target.clone(), ordinary)
            .is_err()
    );
    assert!(!f.stage.exists());
}

#[test]
fn owned_workspace_rejects_compose_restored_stage_and_store_aliases_before_writes() {
    let f = Fixture::new();
    let other = f.root.join("compose");
    fs::create_dir(&other).unwrap();
    let hints = [
        OverlayHint {
            lower_dirs: vec![other, f.target.clone()],
            ..Default::default()
        },
        OverlayHint {
            execution_snapshot: Some(pvisor::ExecutionOverlayHint {
                target: f.target.clone(),
                baseline_lower: Some(f.base.root()),
                excluded_paths: vec![],
            }),
            ..Default::default()
        },
        OverlayHint {
            stage_dir: Some(f.base.root().join("forbidden-stage")),
            ..Default::default()
        },
        OverlayHint {
            upper_dir: Some(f.base.root().join("forbidden-upper")),
            ..Default::default()
        },
        OverlayHint {
            merged_dir: Some(f.root.join("merged")),
            ..Default::default()
        },
    ];
    for (index, hint) in hints.into_iter().enumerate() {
        assert!(
            f.builder()
                .overlay(hint)
                .build()
                .resolve_operation(RunSpec::process(
                    format!("invalid-{index}"),
                    "sh",
                    "/bin/true"
                ))
                .is_err()
        );
    }
    let alias = f.root.join("store-alias");
    std::os::unix::fs::symlink(f.root.join("workspace-store"), &alias).unwrap();
    assert!(
        f.builder()
            .storage(alias.join("forbidden"))
            .build()
            .resolve_operation(RunSpec::process("alias", "sh", "/bin/true"))
            .is_err()
    );
    assert!(!f.base.root().join("forbidden-stage").exists());
    assert!(!f.base.root().join("forbidden-upper").exists());
    assert!(!alias.join("forbidden").exists());
    assert!(!f.stage.exists());
}

#[test]
fn owned_workspace_checks_actual_vm_writable_backing_in_both_stores() {
    let f = Fixture::new();
    for path in [
        f.root.join("workspace-store/new.ram"),
        f.base.root().join("new.ram"),
    ] {
        let mut settings = f.settings();
        settings.ram_backing = Some(path.clone());
        let executor = Arc::new(VmExecutor::new(settings).unwrap());
        assert!(
            PVisor::builder()
                .with_workspace_base(f.base.try_clone().unwrap(), f.target.clone(), executor)
                .is_err()
        );
        assert!(!path.exists());
    }
    for socket in [
        f.root.join("workspace-store/new.sock"),
        f.root.join("rootfs-store/new.sock"),
    ] {
        let vm = VmExecutor::new(f.settings())
            .unwrap()
            .with_vsock_ports([(9000, socket.clone())].into())
            .unwrap();
        assert!(
            PVisor::builder()
                .with_workspace_base(f.base.try_clone().unwrap(), f.target.clone(), Arc::new(vm))
                .is_err()
        );
        assert!(!socket.exists());
    }
    assert!(!f.stage.exists());
}

#[tokio::test]
async fn executor_output_observer_runs_before_driver_terminal_publication() {
    let temp = tempfile::tempdir().unwrap();
    let stage = temp.path().join("observer-stage");
    let order = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&order);
    let stored = stage.clone();
    let runtime = PVisor::builder()
        .storage(&stage)
        .on_executor_output(move |output| {
            assert_eq!(output.state, RunState::Completed);
            assert_eq!(
                pvisor::RunRecord::read(&stored).unwrap().state,
                pvisor::RunRecordState::Running
            );
            seen.lock()
                .unwrap()
                .push("executor-returned-before-teardown");
        })
        .build();
    let mut spec = RunSpec::process("observer-order", "sh", "/bin/sh");
    let pvisor_core::RunInvocation::Process(process) = &mut spec.invocation;
    process.args = vec!["-c".into(), "exit 0".into()];
    let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
    assert_eq!(result.state, RunState::Completed);
    assert_eq!(
        *order.lock().unwrap(),
        ["executor-returned-before-teardown"]
    );
    assert_eq!(
        pvisor::RunRecord::read(&stage).unwrap().state,
        pvisor::RunRecordState::Completed
    );
}

#[test]
fn owned_workspace_public_apply_keeps_snapshot_baseline_and_host_conflicts() {
    for host_edit in [false, true] {
        let f = Fixture::new();
        let (core, record) = f.stopped_stage();
        if host_edit {
            fs::write(f.target.join("file"), b"host edit before copyup").unwrap();
        }
        let upper = core.copy_up(std::path::Path::new("file")).unwrap();
        fs::write(upper, b"agent change").unwrap();
        drop(core);
        pvisor_overlay_core::stage::seal(&f.stage.join("upper"), &f.stage.join("preimages"))
            .unwrap();
        record.write().unwrap();
        let result = f.apply(None);
        if host_edit {
            let error = result.expect_err("apply must preserve an intervening Host edit");
            assert!(
                error.to_string().contains("target changed after staging"),
                "{error:#}"
            );
            assert_eq!(
                fs::read(f.target.join("file")).unwrap(),
                b"host edit before copyup"
            );
        } else {
            assert!(matches!(
                result.unwrap().outcome,
                pvisor::job_service::MutationOutcome::Applied { .. }
            ));
            assert_eq!(fs::read(f.target.join("file")).unwrap(), b"agent change");
        }
        assert_eq!(fs::read(f.base.root().join("file")).unwrap(), b"baseline");
        f.base.verify().unwrap();
    }
}

#[test]
fn owned_workspace_apply_reauthenticates_receipt_and_rejects_target_override_before_mkdir() {
    let f = Fixture::new();
    let (core, record) = f.stopped_stage();
    fs::write(
        core.copy_up(std::path::Path::new("file")).unwrap(),
        b"agent change",
    )
    .unwrap();
    drop(core);
    pvisor_overlay_core::stage::seal(&f.stage.join("upper"), &f.stage.join("preimages")).unwrap();
    record.write().unwrap();
    for path in [
        f.base.root().join("forbidden"),
        f.root.join("rootfs-store/forbidden"),
    ] {
        assert!(f.apply(Some(path.clone())).is_err());
        assert!(!path.exists());
    }
    let receipt = f.stage.join("workspace-baseline.json");
    let bytes = fs::read(&receipt).unwrap();
    let mut changed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    changed["workspace"]["base"]["id"] = serde_json::json!("f".repeat(64));
    fs::write(&receipt, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(f.apply(None).is_err());
    assert_eq!(fs::read(f.target.join("file")).unwrap(), b"baseline");
    fs::write(&receipt, &bytes).unwrap();
    fs::remove_file(
        f.root
            .join("rootfs-store/prepared-rootfs")
            .join(&f.rootfs.id)
            .join("reference"),
    )
    .unwrap();
    assert!(f.apply(None).is_err());
    assert_eq!(fs::read(f.target.join("file")).unwrap(), b"baseline");
}

#[test]
fn owned_workspace_checkpoint_review_survives_but_reconstruction_is_rejected() {
    let f = Fixture::new();
    let (core, record) = f.stopped_stage();
    fs::write(
        core.copy_up(std::path::Path::new("file")).unwrap(),
        b"checkpoint content",
    )
    .unwrap();
    drop(core);
    pvisor_overlay_core::stage::seal(&f.stage.join("upper"), &f.stage.join("preimages")).unwrap();
    record.write().unwrap();
    let response = pvisor::job_service::RuntimeJobService::create_workspace_checkpoint(
        &pvisor::job_service::ServiceContext::default(),
        pvisor::job_service::WorkspaceCreateRequest {
            job: pvisor::job_service::JobSelection {
                selector: Some(f.stage.clone()),
                storage: f.root.clone(),
            },
            request_id: None,
        },
    )
    .unwrap();
    let checkpoint = response.checkpoint;
    let view = pvisor::checkpoint::workspace_view(&record, &checkpoint).unwrap();
    assert_eq!(
        view.overlay.as_ref().unwrap().baseline_lower,
        Some(f.base.root())
    );
    assert_eq!(
        fs::read(checkpoint.upper_snapshot.join("file")).unwrap(),
        b"checkpoint content"
    );
    let child = f.root.join("child");
    let restore = |cp: &pvisor::LogicalCheckpoint| {
        pvisor::restore_logical_checkpoint(cp, &child.join("upper"), &child.join("preimages"))
    };
    assert!(format!("{:#}", restore(&checkpoint).unwrap_err()).contains("fresh-only"));
    assert!(!child.exists());
    let mut moved_source = checkpoint.clone();
    moved_source.source_stage = f.root.join("source-no-longer-present");
    assert!(format!("{:#}", restore(&moved_source).unwrap_err()).contains("fresh-only"));
    assert!(
        !child.exists(),
        "authenticated catalog membership still refuses reconstruction"
    );

    // A separate ordinary mutable checkpoint keeps its historical upper/journal
    // restoration contract even when the live target is no longer online.
    let normal_stage = f.root.join("ordinary-stage");
    let normal_target = f.root.join("ordinary-target");
    fs::create_dir(&normal_target).unwrap();
    fs::write(normal_target.join("file"), b"ordinary base").unwrap();
    pvisor_overlay_core::stage::begin(&normal_stage.join("preimages"), Default::default()).unwrap();
    let ordinary = pvisor_overlay_core::OverlayCore::new_for_layout_with_compact_preimages(
        pvisor_overlay_core::OverlayLayout::new(vec![normal_target.clone()], normal_target.clone())
            .unwrap(),
        normal_stage.join("upper"),
        Some(normal_stage.join("work")),
        vec![],
        Some(normal_stage.join("preimages")),
    )
    .unwrap();
    fs::write(
        ordinary.copy_up(std::path::Path::new("file")).unwrap(),
        b"ordinary checkpoint",
    )
    .unwrap();
    drop(ordinary);
    pvisor_overlay_core::stage::seal(&normal_stage.join("upper"), &normal_stage.join("preimages"))
        .unwrap();
    let mut normal = record.clone();
    normal.run_id = "ordinary-checkpoint".into();
    normal.session_id = normal.run_id.clone();
    normal.storage = normal_stage.clone();
    normal.overlay_lowers = vec![normal_target.clone()];
    let overlay = normal.overlay.as_mut().unwrap();
    overlay.id = normal.run_id.clone();
    overlay.target = normal_target.clone();
    overlay.baseline_lower = None;
    overlay.stage_dir = normal_stage.clone();
    overlay.upper.upper_dir = normal_stage.join("upper");
    overlay.upper.work_dir = normal_stage.join("work");
    overlay.merged_dir = normal_stage.join("merged");
    normal.write().unwrap();
    let cp = pvisor::create_logical_checkpoint(&normal, None).unwrap();
    fs::remove_dir_all(normal_target).unwrap();
    let restored = f.root.join("ordinary-restored");
    fs::create_dir(&restored).unwrap();
    pvisor::restore_logical_checkpoint(&cp, &restored.join("upper"), &restored.join("preimages"))
        .unwrap();
    assert_eq!(
        fs::read(restored.join("upper/file")).unwrap(),
        b"ordinary checkpoint"
    );
}

#[tokio::test]
async fn owned_workspace_native_restore_services_refuse_before_launch_or_stage_mutation() {
    use pvisor::job_service::*;
    let f = Fixture::new();
    let (core, record) = f.stopped_stage();
    drop(core);
    pvisor_overlay_core::stage::seal(&f.stage.join("upper"), &f.stage.join("preimages")).unwrap();
    record.write().unwrap();
    let context = ServiceContext::default();
    let job = || JobSelection {
        selector: Some(f.stage.clone()),
        storage: f.root.clone(),
    };
    assert!(execution_blocker(&record).unwrap().contains("fresh-only"));
    assert!(format!("{:#}", policy::workspace_config(&record).unwrap_err()).contains("fresh-only"));
    let captured = RuntimeJobService::capture_execution(
        &context,
        CaptureRequest {
            job: job(),
            suspend: false,
            ram_storage: pvisor_core::operation::SnapshotRamStorage::Compressed,
            request_id: None,
            timeout: std::time::Duration::from_secs(1),
        },
    )
    .await;
    assert!(format!("{:#}", captured.unwrap_err()).contains("fresh-only"));
    let resumed = RuntimeJobService::resume(
        &context,
        ResumeRequest {
            job: job(),
            request_id: None,
            eager_ram: false,
        },
        |_| async { panic!("fresh-only restore must not invoke a launcher") },
    )
    .await;
    assert!(format!("{:#}", resumed.unwrap_err()).contains("fresh-only"));
    let child = f.root.join("fork-child");
    let forked = RuntimeJobService::fork_execution(
        &context,
        ExecutionForkRequest {
            job: job(),
            checkpoint: Some("unused".into()),
            stage: Some(child.clone()),
            name: None,
            ram_storage: None,
            eager_ram: false,
            request_id: None,
        },
        |_| async { panic!("fresh-only fork must not invoke a launcher") },
    )
    .await;
    assert!(format!("{:#}", forked.unwrap_err()).contains("fresh-only"));
    assert!(!child.exists());
    assert!(!f.stage.join("attempts").exists());
    assert!(!f.stage.join("execution-job.json").exists());
}

#[tokio::test]
async fn owned_workspace_managed_selector_cannot_replace_runtime_ownership() {
    let f = Fixture::new();
    let mut config = pvisor::RunConfig::default();
    config.vm.prepared_rootfs = Some(pvisor::PreparedRootfs {
        store: f.root.join("rootfs-store"),
        base: f.rootfs.clone(),
    });
    config.overlayfs = Some(pvisor::OverlayFsSettings {
        prepared_workspace: Some(pvisor::PreparedBase {
            store: f.root.join("workspace-store"),
            base: f.base.reference().clone(),
        }),
        ..Default::default()
    });
    let plain = PVisor::builder().storage(&f.stage).build();
    let denied = pvisor::job_service::RuntimeJobService::start_managed(
        &plain,
        RunSpec::process("unowned-managed", "sh", "/bin/true"),
        config.clone(),
    )
    .await;
    assert!(
        format!("{:#}", denied.err().expect("selector is not a lease"))
            .contains("explicit owned runtime baseline")
    );
    assert!(!f.stage.exists());
    config
        .overlayfs
        .as_mut()
        .unwrap()
        .prepared_workspace
        .as_mut()
        .unwrap()
        .base
        .id = "f".repeat(64);
    let owned = f.builder().build();
    let denied = pvisor::job_service::RuntimeJobService::start_managed(
        &owned,
        RunSpec::process("wrong-selector", "sh", "/bin/true"),
        config,
    )
    .await;
    assert!(
        format!(
            "{:#}",
            denied
                .err()
                .expect("a different selector cannot be persisted")
        )
        .contains("actual owned baseline")
    );
    let mut restored = RunSpec::process("restore-metadata", "sh", "/bin/true");
    restored.metadata.insert(
        "pvisor.orchestration.execution_restore".into(),
        serde_json::json!({"snapshot_id":"not-fresh"}),
    );
    assert!(owned.resolve_operation(restored).is_err());
    assert!(!f.stage.exists());
}
