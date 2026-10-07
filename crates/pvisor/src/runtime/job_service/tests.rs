use super::*;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

fn fixture(root: &Path) -> RunRecord {
    for dir in ["target", "upper", "work", "merged"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    let record: RunRecord = serde_json::from_value(serde_json::json!({
        "schema_version":1,"run_id":"job-service","session_id":"session-service","attempt_id":"attempt-service",
        "agent":"sh","pid":0,"command":["/bin/sh"],"state":"completed",
        "started_at_unix_ms":1,"finished_at_unix_ms":2,"storage":root,"network":{},"gateway_listen":null,
        "overlay":{"id":"job-service","generation":7,"target":root.join("target"),
            "upper":{"upper_dir":root.join("upper"),"work_dir":root.join("work")},
            "merged_dir":root.join("merged"),"stage_dir":root,"auto_apply":false,"state":"staged"}
    })).unwrap();
    record.write().unwrap();
    record
}
fn selection(record: &RunRecord) -> JobSelection {
    JobSelection {
        selector: Some(record.stage_dir()),
        storage: record.storage.clone(),
    }
}
fn conflict(error: anyhow::Error) {
    assert_eq!(
        error.downcast_ref::<AgentCtlHostError>().unwrap().code,
        AgentCtlHostErrorCode::Conflict
    );
}

#[test]
fn embedded_status_and_drop_publish_generation_without_cli() {
    let root = tempfile::tempdir().unwrap();
    let record = fixture(root.path());
    std::fs::write(root.path().join("upper/change"), "staged").unwrap();
    let context = ServiceContext::default();
    let status = RuntimeJobService::status(
        &context,
        StatusRequest {
            job: selection(&record),
        },
    )
    .unwrap();
    assert!(!status.live);
    assert_eq!(status.filesystem.unwrap().changed_files, 1);
    let response = RuntimeJobService::drop(
        &context,
        DropRequest {
            job: selection(&record),
        },
    )
    .unwrap();
    assert!(matches!(response.outcome, MutationOutcome::Dropped));
    let current = RunRecord::read(root.path()).unwrap();
    assert_eq!(current.overlay.as_ref().unwrap().generation, 8);
    assert_eq!(
        current.overlay.as_ref().unwrap().state,
        super::super::OverlayState::Discarded
    );
    let repeated = RuntimeJobService::drop(
        &context,
        DropRequest {
            job: selection(&current),
        },
    )
    .unwrap();
    assert!(matches!(repeated.outcome, MutationOutcome::AlreadyDropped));
    assert_eq!(
        RunRecord::read(root.path())
            .unwrap()
            .overlay
            .unwrap()
            .generation,
        8
    );
}

#[test]
fn embedded_selective_apply_retains_remaining_effects_and_advances_generation() {
    let root = tempfile::tempdir().unwrap();
    let record = fixture(root.path());
    std::fs::write(root.path().join("upper/one"), "first").unwrap();
    std::fs::write(root.path().join("upper/two"), "second").unwrap();
    let context = ServiceContext::default();
    let first = RuntimeJobService::apply(
        &context,
        ApplyRequest {
            job: selection(&record),
            target: None,
            all: false,
            selection: super::super::ApplySelection {
                paths: vec!["one".into()],
                includes: vec![],
                excludes: vec![],
            },
        },
    )
    .unwrap();
    assert!(matches!(
        first.outcome,
        MutationOutcome::Applied {
            applied: 1,
            remaining: 1,
            ..
        }
    ));
    assert_eq!(
        std::fs::read_to_string(root.path().join("target/one")).unwrap(),
        "first"
    );
    assert!(!root.path().join("target/two").exists());
    let current = RunRecord::read(root.path()).unwrap();
    assert_eq!(current.overlay.as_ref().unwrap().generation, 8);
    let second = RuntimeJobService::apply(
        &context,
        ApplyRequest {
            job: selection(&current),
            target: None,
            all: true,
            selection: super::super::ApplySelection::default(),
        },
    )
    .unwrap();
    assert!(matches!(
        second.outcome,
        MutationOutcome::Applied {
            applied: 1,
            remaining: 0,
            ..
        }
    ));
    assert_eq!(
        std::fs::read_to_string(root.path().join("target/two")).unwrap(),
        "second"
    );
    let current = RunRecord::read(root.path()).unwrap();
    assert_eq!(current.overlay.as_ref().unwrap().generation, 9);
    assert_eq!(
        current.overlay.as_ref().unwrap().state,
        super::super::OverlayState::Applied
    );
}

#[test]
fn embedded_review_refreshes_files_without_rewriting_historical_evidence() {
    let root = tempfile::tempdir().unwrap();
    let record = fixture(root.path());
    std::fs::write(root.path().join("upper/new-file"), "new contents").unwrap();
    let mut historical: serde_json::Value = serde_json::from_slice(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/bundles/v1-minimal.json"
    )))
    .unwrap();
    historical["schema_version"] = serde_json::json!(crate::RUN_BUNDLE_SCHEMA_VERSION);
    historical["run"]["run_id"] = serde_json::json!(record.run_id);
    historical["executor_observations"] =
        serde_json::to_value(pvisor_core::ExecutorObservations::default()).unwrap();
    historical["filesystem"] = serde_json::json!({
        "state":"staged", "target":root.path().join("target"), "upper":root.path().join("upper"),
        "changed_files":0, "whiteouts":0, "changes":[], "sample_paths":[]
    });
    let bundle: crate::RunBundle = serde_json::from_value(historical).unwrap();
    bundle.write(root.path()).unwrap();
    let before = std::fs::read(crate::RunBundle::path(root.path())).unwrap();
    let response = RuntimeJobService::review(
        &ServiceContext::default(),
        ReviewRequest {
            job: selection(&record),
            checkpoint: None,
        },
    )
    .unwrap();
    assert_eq!(
        response.bundle.filesystem.as_ref().unwrap().changed_files,
        1
    );
    assert_eq!(response.context.workspace_generation, Some(7));
    assert_eq!(response.context.execution_evidence, "historical_bundle");
    assert_eq!(
        std::fs::read(crate::RunBundle::path(root.path())).unwrap(),
        before
    );
}

#[test]
fn workspace_receipts_remain_committed_after_deletion() {
    let root = tempfile::tempdir().unwrap();
    let record = fixture(root.path());
    std::fs::write(root.path().join("upper/change"), "staged").unwrap();
    let context = ServiceContext::default();
    let create = || {
        RuntimeJobService::create_workspace_checkpoint(
            &context,
            WorkspaceCreateRequest {
                job: selection(&record),
                request_id: Some("scoped-request".into()),
            },
        )
    };
    let first = create().unwrap();
    assert!(!first.reused);
    let second = create().unwrap();
    assert!(second.reused);
    assert_eq!(
        first.checkpoint.checkpoint_id,
        second.checkpoint.checkpoint_id
    );
    RuntimeJobService::delete_workspace_checkpoint(
        &context,
        WorkspaceDeleteRequest {
            job: selection(&record),
            checkpoint_id: first.checkpoint.checkpoint_id,
        },
    )
    .unwrap();
    assert!(format!("{:#}", create().unwrap_err()).contains("request already committed"));
    let pending = root
        .path()
        .join(crate::CHECKPOINTS_DIR)
        .join(".pending-owned");
    std::fs::create_dir_all(&pending).unwrap();
    assert_eq!(
        RuntimeJobService::collect_workspace_transactions(
            &context,
            WorkspaceGcRequest {
                job: selection(&record)
            }
        )
        .unwrap(),
        1
    );
    assert!(!pending.exists());
    assert!(
        root.path()
            .join(crate::CHECKPOINTS_DIR)
            .join(".requests")
            .is_dir()
    );
}

#[test]
fn workspace_transactions_recheck_selection_under_stage_lease() {
    let root = tempfile::tempdir().unwrap();
    let selected = fixture(root.path());
    let pending = root
        .path()
        .join(crate::CHECKPOINTS_DIR)
        .join(".pending-owned");
    std::fs::create_dir_all(&pending).unwrap();
    let mut current = selected.clone();
    current.overlay.as_mut().unwrap().generation = 8;
    current.write().unwrap();
    let context = ServiceContext::default();
    conflict(
        RuntimeJobService::create_selected_workspace_checkpoint(&context, &selected, None)
            .unwrap_err(),
    );
    conflict(
        RuntimeJobService::delete_selected_workspace_checkpoint(&context, &selected, "missing")
            .unwrap_err(),
    );
    conflict(
        RuntimeJobService::collect_selected_workspace_transactions(&context, &selected)
            .unwrap_err(),
    );
    assert!(pending.is_dir());
}

#[test]
fn apply_rechecks_a_generation_change_after_resolution() {
    let root = tempfile::tempdir().unwrap();
    let record = fixture(root.path());
    let first = AtomicBool::new(true);
    let on_check = |selected: &RunRecord| {
        if first.swap(false, Ordering::SeqCst) {
            let mut changed = selected.clone();
            changed.overlay.as_mut().unwrap().generation += 1;
            changed.write()?;
        }
        Ok(())
    };
    let context = ServiceContext {
        check_record: Some(&on_check),
        ..Default::default()
    };
    conflict(
        RuntimeJobService::apply(
            &context,
            ApplyRequest {
                job: selection(&record),
                target: None,
                selection: super::super::ApplySelection {
                    paths: vec![],
                    includes: vec![],
                    excludes: vec![],
                },
                all: true,
            },
        )
        .unwrap_err(),
    );
    assert_eq!(
        RunRecord::read(root.path())
            .unwrap()
            .overlay
            .unwrap()
            .generation,
        8
    );
}

#[test]
fn fencing_and_cancellation_precede_effects() {
    let root = tempfile::tempdir().unwrap();
    let record = fixture(root.path());
    let target = AgentCtlTarget {
        job_id: record.run_id.clone(),
        attempt_id: record.attempt_id.clone(),
        generation: Some("6".into()),
    };
    let context = ServiceContext {
        expected_target: Some(&target),
        ..Default::default()
    };
    conflict(
        RuntimeJobService::drop(
            &context,
            DropRequest {
                job: selection(&record),
            },
        )
        .unwrap_err(),
    );
    let cancel = || anyhow::bail!("cancelled by embedded caller");
    let context = ServiceContext {
        check_cancelled: Some(&cancel),
        ..Default::default()
    };
    assert!(
        RuntimeJobService::drop(
            &context,
            DropRequest {
                job: selection(&record)
            }
        )
        .unwrap_err()
        .to_string()
        .contains("cancelled")
    );
    assert_eq!(
        RunRecord::read(root.path())
            .unwrap()
            .overlay
            .unwrap()
            .generation,
        7
    );
}

#[tokio::test]
async fn capture_rechecks_request_context_after_waiting_for_job_lease() {
    use crate::runtime::job_execution::{JOB_SCHEMA_VERSION, Job, JobState};
    use pvisor_core::operation::SnapshotRamStorage;
    use std::time::Duration;

    for state in [JobState::Running, JobState::Suspended] {
        for change in ["cancel", "generation", "authority"] {
            let root = tempfile::tempdir().unwrap();
            let record = fixture(root.path());
            let mut config = crate::RunConfig::default();
            config.run.executor = crate::RunExecutorKind::Vm;
            config.overlaynet.mode = crate::OverlayNetMode::Off;
            let job = Job {
                version: JOB_SCHEMA_VERSION,
                run_id: record.run_id.clone(),
                root: root.path().into(),
                active_stage: root.path().into(),
                previous_stage: root.path().into(),
                active_attempt: record.attempt_id.clone().unwrap(),
                config,
                spec: pvisor_core::RunSpec::process("job-service", "sh", "/bin/sh"),
                state,
                head: None,
                checkpoints: Default::default(),
                requests: Default::default(),
                resumes: Default::default(),
                forks: Default::default(),
                stores: Default::default(),
            };
            job.write().unwrap();
            let before = serde_json::to_value(&job).unwrap();
            let lease = job.lock().unwrap();
            let cancelled = AtomicBool::new(false);
            let authorized = AtomicBool::new(true);
            let check_cancelled = || {
                anyhow::ensure!(!cancelled.load(Ordering::SeqCst), "request cancelled");
                Ok(())
            };
            let check_record = |_: &RunRecord| {
                anyhow::ensure!(authorized.load(Ordering::SeqCst), "authority revoked");
                Ok(())
            };
            let context = ServiceContext {
                check_cancelled: Some(&check_cancelled),
                check_record: Some(&check_record),
                ..Default::default()
            };
            let mut pending = Box::pin(RuntimeJobService::capture_execution(
                &context,
                CaptureRequest {
                    job: selection(&record),
                    suspend: false,
                    ram_storage: SnapshotRamStorage::Raw,
                    request_id: Some("fenced-capture".into()),
                    timeout: Duration::from_secs(2),
                },
            ));
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut pending)
                    .await
                    .is_err()
            );
            match change {
                "cancel" => cancelled.store(true, Ordering::SeqCst),
                "authority" => authorized.store(false, Ordering::SeqCst),
                _ => {
                    let mut changed = record.clone();
                    changed.overlay.as_mut().unwrap().generation += 1;
                    changed.write().unwrap();
                }
            }
            drop(lease);
            let error = pending.await.unwrap_err();
            match change {
                "cancel" => assert!(error.to_string().contains("request cancelled")),
                "authority" => assert!(error.to_string().contains("authority revoked")),
                _ => conflict(error),
            }
            assert_eq!(
                serde_json::to_value(job.current().unwrap()).unwrap(),
                before
            );
        }
    }
}

#[tokio::test]
async fn resume_replay_is_fenced_before_embedded_launcher() {
    use crate::runtime::job_execution::{
        JOB_SCHEMA_VERSION, Job, JobState, ResumeRequest as Receipt,
    };
    let root = tempfile::tempdir().unwrap();
    let record = fixture(root.path());
    let mut config = crate::RunConfig::default();
    config.run.executor = crate::RunExecutorKind::Vm;
    config.overlaynet.mode = crate::OverlayNetMode::Off;
    let job = Job {
        version: JOB_SCHEMA_VERSION,
        run_id: record.run_id.clone(),
        root: root.path().into(),
        active_stage: root.path().into(),
        previous_stage: root.path().into(),
        active_attempt: "new-attempt".into(),
        config,
        spec: pvisor_core::RunSpec::process("job-service", "sh", "/bin/sh"),
        state: JobState::Suspended,
        head: None,
        checkpoints: Default::default(),
        requests: Default::default(),
        resumes: [(
            "replay".into(),
            Receipt {
                stage: root.path().into(),
                eager_ram: false,
            },
        )]
        .into(),
        forks: Default::default(),
        stores: Default::default(),
    };
    job.write().unwrap();
    // Callback success alone cannot make a Restoring or unbound Job Finished.
    assert!(super::lifecycle::confirm_restored_completion(root.path(), &record.run_id).is_err());
    let mut finished = job.clone();
    finished.state = JobState::Terminal;
    finished.active_attempt = record.attempt_id.clone().unwrap();
    finished.write().unwrap();
    super::lifecycle::confirm_restored_completion(root.path(), &record.run_id).unwrap();
    assert!(super::lifecycle::confirm_restored_completion(root.path(), "another-job").is_err());
    job.write().unwrap();
    let launched = AtomicBool::new(false);
    let error = RuntimeJobService::resume(
        &ServiceContext::default(),
        ResumeRequest {
            job: selection(&record),
            request_id: Some("replay".into()),
            eager_ram: false,
        },
        |_| async {
            launched.store(true, Ordering::SeqCst);
            Ok(0)
        },
    )
    .await
    .unwrap_err();
    conflict(error);
    assert!(!launched.load(Ordering::SeqCst));
    let error = RuntimeJobService::fork_execution(
        &ServiceContext::default(),
        ExecutionForkRequest {
            job: selection(&record),
            checkpoint: None,
            stage: None,
            name: None,
            ram_storage: None,
            request_id: Some("replay".into()),
            eager_ram: false,
        },
        |_| async {
            launched.store(true, Ordering::SeqCst);
            Ok(0)
        },
    )
    .await
    .unwrap_err();
    conflict(error);
    assert!(!launched.load(Ordering::SeqCst));
    assert!(!root.path().join("attempts").exists());

    let mut admitted = job.clone();
    admitted.state = JobState::Restoring;
    admitted.active_stage = root.path().join("attempts/restore");
    admitted.resumes.insert(
        "restore".into(),
        Receipt {
            stage: admitted.active_stage.clone(),
            eager_ram: false,
        },
    );
    assert!(owns_restore_transition(&admitted, &admitted, "restore"));
    for variant in 0..5 {
        let mut current = admitted.clone();
        match variant {
            0 => current.active_stage = root.path().join("attempts/other"),
            1 => current.active_attempt = "other-attempt".into(),
            2 => current.head = Some("other-head".into()),
            3 => current.resumes.get_mut("restore").unwrap().eager_ram = true,
            _ => {
                current.resumes.insert(
                    "another".into(),
                    Receipt {
                        stage: current.active_stage.clone(),
                        eager_ram: false,
                    },
                );
            }
        }
        assert!(!owns_restore_transition(&current, &admitted, "restore"));
    }
}
