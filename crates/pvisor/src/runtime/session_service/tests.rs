use super::paths::fork_stage_candidate;
use super::*;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

fn fixture(root: &Path) -> RunRecord {
    for dir in ["target", "upper", "work", "merged"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    let record: RunRecord = serde_json::from_value(serde_json::json!({
        "schema_version":1,"run_id":"session-service","session_id":"session-service","execution_id":"execution-service",
        "agent":"sh","pid":0,"command":["/bin/sh"],"state":"completed",
        "started_at_unix_ms":1,"finished_at_unix_ms":2,"storage":root,"network":{},"gateway_listen":null,
        "overlay":{"id":"session-service","generation":7,"target":root.join("target"),
            "upper":{"upper_dir":root.join("upper"),"work_dir":root.join("work")},
            "merged_dir":root.join("merged"),"stage_dir":root,"auto_apply":false,"state":"staged"}
    })).unwrap();
    record.write().unwrap();
    record
}
fn selection(record: &RunRecord) -> SessionSelection {
    SessionSelection {
        selector: Some(record.stage_dir()),
        storage: record.storage.clone(),
    }
}
fn apply_request(record: &RunRecord) -> ApplyCommandRequest {
    ApplyCommandRequest {
        session: selection(record),
        rootfs: false,
        selection: Default::default(),
        all: true,
        dry_run: false,
        expected_plan: None,
    }
}
fn conflict(error: anyhow::Error) {
    assert_eq!(
        error.downcast_ref::<AgentCtlHostError>().unwrap().code,
        AgentCtlHostErrorCode::Conflict
    );
}

#[test]
fn durable_request_ids_share_core_bounds_and_reject_controls() {
    validate_request_id(&"x".repeat(256)).unwrap();
    for id in [
        "x".repeat(257),
        "bad\nkey".into(),
        "bad\u{0085}key".into(),
        " ".into(),
    ] {
        assert_eq!(
            validate_request_id(&id)
                .unwrap_err()
                .downcast_ref::<AgentCtlHostError>()
                .unwrap()
                .code,
            AgentCtlHostErrorCode::InvalidRequest
        );
    }
}

#[test]
fn durable_target_rejects_stale_session_execution_and_generation() {
    let root = tempfile::tempdir().unwrap();
    let record = fixture(root.path());
    let target = AgentCtlTarget {
        session_id: record.run_id.clone(),
        execution_id: record.execution_id.clone(),
        generation: Some("7".into()),
    };
    check_target(&target, &record).unwrap();
    for field in 0..3 {
        let mut stale = target.clone();
        match field {
            0 => stale.session_id = "other-session".into(),
            1 => stale.execution_id = Some("stale-execution".into()),
            _ => stale.generation = Some("6".into()),
        }
        conflict(check_target(&stale, &record).unwrap_err());
    }
}

#[test]
fn fork_stage_candidate_does_not_create_nested_missing_directories() {
    let root = tempfile::tempdir().unwrap();
    let candidate = root.path().join("missing/nested/stage");
    assert_eq!(
        fork_stage_candidate(&candidate).unwrap(),
        root.path()
            .canonicalize()
            .unwrap()
            .join("missing/nested/stage")
    );
    assert!(!root.path().join("missing").exists());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn fork_stage_candidate_canonicalizes_existing_paths_and_symlink_ancestors() {
    let root = tempfile::tempdir().unwrap();
    let existing = root.path().join("existing");
    std::fs::create_dir(&existing).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&existing, &alias).unwrap();
    assert_eq!(
        fork_stage_candidate(&alias).unwrap(),
        existing.canonicalize().unwrap()
    );
    assert_eq!(
        fork_stage_candidate(&alias.join("nested/stage")).unwrap(),
        existing.canonicalize().unwrap().join("nested/stage")
    );
    assert!(!existing.join("nested").exists());
}

#[test]
fn fork_stage_candidate_resolves_relative_paths_from_current_directory() {
    assert_eq!(
        fork_stage_candidate(Path::new(".")).unwrap(),
        std::env::current_dir().unwrap().canonicalize().unwrap()
    );
}

#[test]
fn embedded_status_and_drop_publish_generation_without_cli() {
    let root = tempfile::tempdir().unwrap();
    let record = fixture(root.path());
    std::fs::write(root.path().join("upper/change"), "staged").unwrap();
    let context = ServiceContext::default();
    let status = SessionService::status(
        &context,
        StatusRequest {
            session: selection(&record),
        },
    )
    .unwrap();
    assert!(!status.live);
    assert_eq!(status.filesystem.unwrap().changed_files, 1);
    let response = SessionService::drop(
        &context,
        DropRequest {
            session: selection(&record),
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
    let repeated = SessionService::drop(
        &context,
        DropRequest {
            session: selection(&current),
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
fn status_is_bounded_independently_of_history_paths_and_launch_configuration() {
    use pvisor_core::operation::{FilesystemObservation, PathOperationCounters};
    let root = tempfile::tempdir().unwrap();
    let mut record = fixture(root.path());
    record.command = vec!["界".repeat(1024); 128];
    record.orchestration.insert(
        "large-private-launch-data".into(),
        serde_json::json!("x".repeat(2 * 1024 * 1024)),
    );
    let observation = FilesystemObservation {
        paths: (0..8192)
            .map(|index| {
                (
                    format!("{index:05}/{}", "界".repeat(200)),
                    [(
                        "read".into(),
                        PathOperationCounters {
                            hits: 2,
                            denied: 1,
                            ..Default::default()
                        },
                    )]
                    .into(),
                )
            })
            .collect(),
        ..Default::default()
    };
    record.filesystem_observation = Some(observation);
    record.write().unwrap();
    let requests: std::collections::BTreeMap<_, _> = (0..4096).map(|index| (
        format!("{index:08}{}", "a".repeat(248)),
        serde_json::json!({"suspend":true,"ram_storage":"raw","checkpoint":null,"error":"failed"})
    )).collect();
    let session: crate::Session = serde_json::from_value(serde_json::json!({
        "version":crate::session_execution::SESSION_SCHEMA_VERSION,"run_id":record.run_id,"root":root.path(),
        "active_stage":root.path(),"previous_stage":root.path(),"active_execution":record.execution_id,
        "config":crate::RunConfig::default(),"spec":pvisor_core::RunSpec::process("session-service", "test", "true"),
        "state":"terminal","head":null,"checkpoints":{},"requests":requests,"resumes":{},"forks":{},"stores":[]
    })).unwrap();
    session.write().unwrap();
    let status = SessionService::status(
        &ServiceContext::default(),
        StatusRequest {
            session: selection(&record),
        },
    )
    .unwrap();
    assert_eq!(status.schema_version, 3);
    assert!(status.record.command_truncated);
    assert_eq!(
        status.execution.as_ref().unwrap().capture_request_count,
        4096
    );
    let observation = status.filesystem_observation.as_ref().unwrap();
    assert_eq!(observation.hits, 16384);
    assert_eq!(observation.denied, 8192);
    assert_eq!(observation.observed_paths, 8192);
    assert_eq!(observation.denied_samples.len(), 5);
    assert!(
        observation
            .denied_samples
            .iter()
            .all(|sample| sample.truncated && sample.target.len() <= 512)
    );
    let encoded = crate::host_transport::encode_host_frame(&status).unwrap();
    assert!(encoded.len() < 256 * 1024);
    let json = serde_json::to_value(status).unwrap();
    assert!(json["execution"].get("requests").is_none());
    assert!(json["record"].get("orchestration").is_none());
    assert!(json["filesystem_observation"].get("paths").is_none());
}

#[test]
fn empty_apply_selection_is_rejected_before_target_or_stage_mutation() {
    let root = tempfile::tempdir().unwrap();
    let record = fixture(root.path());
    std::fs::write(root.path().join("upper/change"), "staged").unwrap();
    let before = std::fs::read(root.path().join("run.json")).unwrap();
    let response = SessionService::apply_command(
        &ServiceContext::default(),
        ApplyCommandRequest {
            all: false,
            ..apply_request(&record)
        },
    )
    .unwrap();
    assert_eq!(response.status, "blocked");
    assert!(
        response
            .blockers
            .join(" ")
            .contains("requires all or explicit positive selection")
    );
    assert!(response.execution.is_none());
    assert_eq!(std::fs::read(root.path().join("run.json")).unwrap(), before);
    assert_eq!(
        std::fs::read_to_string(root.path().join("upper/change")).unwrap(),
        "staged"
    );
    assert!(!root.path().join("target/change").exists());
}

#[test]
fn embedded_selective_apply_retains_remaining_effects_and_advances_generation() {
    let root = tempfile::tempdir().unwrap();
    let record = fixture(root.path());
    std::fs::write(root.path().join("upper/one"), "first").unwrap();
    std::fs::write(root.path().join("upper/two"), "second").unwrap();
    let context = ServiceContext::default();
    let first = SessionService::apply_command(
        &context,
        ApplyCommandRequest {
            all: false,
            selection: super::super::ApplySelection {
                paths: vec!["one".into()],
                ..Default::default()
            },
            ..apply_request(&record)
        },
    )
    .unwrap();
    assert_eq!(first.status, "applied", "{:?}", first.blockers);
    assert_eq!(first.execution.unwrap().applied, Some(1));
    assert_eq!(first.remaining.in_scope, Some(1));
    assert_eq!(
        std::fs::read_to_string(root.path().join("target/one")).unwrap(),
        "first"
    );
    assert!(!root.path().join("target/two").exists());
    let current = RunRecord::read(root.path()).unwrap();
    assert_eq!(current.overlay.as_ref().unwrap().generation, 8);
    let repeated = SessionService::apply_command(
        &context,
        ApplyCommandRequest {
            all: false,
            selection: super::super::ApplySelection {
                paths: vec!["one".into()],
                ..Default::default()
            },
            ..apply_request(&current)
        },
    )
    .unwrap();
    assert_eq!(repeated.status, "no_changes");
    assert!(repeated.execution.is_none());
    assert_eq!(
        RunRecord::read(root.path())
            .unwrap()
            .overlay
            .unwrap()
            .generation,
        8
    );
    let second = SessionService::apply_command(&context, apply_request(&current)).unwrap();
    assert_eq!(second.status, "applied", "{:?}", second.blockers);
    assert_eq!(second.execution.unwrap().applied, Some(1));
    assert_eq!(second.remaining.in_scope, Some(0));
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
fn uncommitted_review_returns_typed_unknown_and_status_cannot_invent_completion() {
    let root = tempfile::tempdir().unwrap();
    let mut record = fixture(root.path());
    record.schema_version = 3;
    record.completion_protocol = Some(super::super::completion::VERSION);
    record.write().unwrap();
    std::fs::write(root.path().join("upper/change"), b"retained").unwrap();
    // Exercise mutation directly while raw disk state still says Completed,
    // before status has had any opportunity to reconcile it to Unknown.
    for apply in [true, false] {
        let error = if apply {
            let response =
                SessionService::apply_command(&ServiceContext::default(), apply_request(&record))
                    .unwrap();
            assert_eq!(response.status, "blocked");
            assert!(response.execution.is_none());
            response.blockers.join(" ")
        } else {
            SessionService::drop(
                &ServiceContext::default(),
                DropRequest {
                    session: selection(&record),
                },
            )
            .err()
            .unwrap()
            .to_string()
        };
        assert!(error.contains("EXECUTION_UNKNOWN"));
        assert_eq!(
            std::fs::read(root.path().join("upper/change")).unwrap(),
            b"retained"
        );
        assert!(!root.path().join("target/change").exists());
    }
    let error = SessionService::review(
        &ServiceContext::default(),
        ReviewRequest {
            session: selection(&record),
            checkpoint: None,
        },
    )
    .err()
    .unwrap();
    let unknown = error.downcast_ref::<ReviewUnavailable>().unwrap();
    assert_eq!(unknown.session_id, record.run_id);
    assert_eq!(unknown.stage, record.stage_dir());
    let status = SessionService::status(
        &ServiceContext::default(),
        StatusRequest {
            session: selection(&record),
        },
    )
    .unwrap();
    assert_eq!(status.record.state, super::super::RunRecordState::Unknown);
    assert!(
        RunRecord::read(root.path())
            .unwrap()
            .require_stopped()
            .is_err()
    );
    assert!(!root.path().join("completion.json").exists());
    assert!(root.path().join("upper").is_dir());
}

#[test]
fn sealed_storage_without_completion_receipt_refuses_recovery_and_reuse() {
    use pvisor_overlay_core::{OverlayCore, OverlayLayout, load_preimages, stage};
    use std::{fs, os::unix::fs::MetadataExt};

    let root = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let mut record = fixture(root.path());
    record.schema_version = 3;
    record.completion_protocol = Some(super::super::completion::VERSION);
    record.write().unwrap();
    let overlay = record.overlay.as_ref().unwrap();
    let journal = root.path().join("preimages");
    stage::begin(&journal, stage::StageDurability::Checkpoint).unwrap();
    fs::write(overlay.target.join("change"), b"host original").unwrap();
    let core = OverlayCore::new_for_layout_with_compact_preimages(
        OverlayLayout::new(vec![overlay.target.clone()], overlay.target.clone()).unwrap(),
        overlay.upper.upper_dir.clone(),
        Some(overlay.upper.work_dir.clone()),
        vec![],
        Some(journal.clone()),
    )
    .unwrap();
    fs::write(core.copy_up(Path::new("change")).unwrap(), b"retained").unwrap();
    core.sync_preimages().unwrap();
    drop(core);
    stage::complete(&overlay.upper.upper_dir, &journal).unwrap();
    stage::require_sealed(&journal).unwrap();
    let observations = load_preimages(&journal).unwrap();
    assert_eq!(observations.len(), 1);
    let log = fs::read(journal.join("log-v2")).unwrap();
    let seal = fs::read(journal.join("sealed-v1")).unwrap();
    let identity = |path: &Path| {
        let metadata = fs::symlink_metadata(path).unwrap();
        (
            metadata.dev(),
            metadata.ino(),
            metadata.uid(),
            metadata.gid(),
        )
    };
    let candidate = overlay.upper.upper_dir.join("change");
    let original_identity = identity(&candidate);
    let result: pvisor_core::RunResult = serde_json::from_value(serde_json::json!({
        "run_id":record.run_id,"execution_id":record.execution_id,"state":"completed",
        "started_at_unix_ms":1,"finished_at_unix_ms":2,"exit_code":0
    }))
    .unwrap();
    let agentctl = serde_json::from_value(serde_json::json!({
        "run_id":record.run_id,"execution_id":record.execution_id,
        "directive":crate::AgentDirective::Continue,"clients":[]
    }))
    .unwrap();
    crate::RunBundle::capture(&record, &result, agentctl, false)
        .unwrap()
        .write(root.path())
        .unwrap();
    let bundle = fs::read(crate::RunBundle::path(root.path())).unwrap();
    let receipt = root.path().join("completion.json");
    fs::create_dir(&receipt).unwrap();
    fs::write(receipt.join("owned-sentinel"), b"obstruction").unwrap();
    // A real destination-type fault, rejected by receipt preflight before
    // atomic replacement. Storage sealing and Bundle publication succeeded.
    let error = super::super::completion::Completion::commit(&record, &result).unwrap_err();
    assert!(error.to_string().contains("regular file"), "{error:#}");
    assert_eq!(
        fs::read(receipt.join("owned-sentinel")).unwrap(),
        b"obstruction"
    );
    assert!(RunRecord::read(root.path()).is_err());

    // Removing only our obstruction is not authority to synthesize a receipt.
    fs::remove_file(receipt.join("owned-sentinel")).unwrap();
    fs::remove_dir(&receipt).unwrap();
    for _ in 0..2 {
        let reopened = OverlayCore::open_existing(
            vec![overlay.target.clone()],
            overlay.upper.upper_dir.clone(),
            Some(overlay.upper.work_dir.clone()),
            vec![],
            Some(journal.clone()),
        )
        .unwrap();
        assert_eq!(
            reopened
                .copy_up(Path::new("change"))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EROFS)
        );
        drop(reopened);
        let context = ServiceContext::default();
        let status = SessionService::status(
            &context,
            StatusRequest {
                session: selection(&record),
            },
        )
        .unwrap();
        assert_eq!(status.record.state, super::super::RunRecordState::Unknown);
        let current = RunRecord::read(root.path()).unwrap();
        assert!(current.require_stopped().is_err());
        let response = SessionService::apply_command(&context, apply_request(&current)).unwrap();
        assert_eq!(response.status, "blocked");
        assert!(response.blockers.join(" ").contains("EXECUTION_UNKNOWN"));
        assert!(response.execution.is_none());
        assert!(
            SessionService::drop(
                &context,
                DropRequest {
                    session: selection(&current)
                },
            )
            .unwrap_err()
            .to_string()
            .contains("EXECUTION_UNKNOWN")
        );
        assert!(
            SessionService::create_workspace_checkpoint(
                &context,
                WorkspaceCreateRequest {
                    session: selection(&current),
                    request_id: None
                },
            )
            .unwrap_err()
            .to_string()
            .contains("EXECUTION_UNKNOWN")
        );
        assert!(super::super::registry::RunLease::acquire_new(root.path()).is_err());
        assert!(!root.path().join("checkpoints").exists());
        assert!(!receipt.exists());
        assert_eq!(fs::read(&candidate).unwrap(), b"retained");
        assert_eq!(identity(&candidate), original_identity);
        assert_eq!(
            fs::read(overlay.target.join("change")).unwrap(),
            b"host original"
        );
        assert_eq!(fs::read(journal.join("log-v2")).unwrap(), log);
        assert_eq!(fs::read(journal.join("sealed-v1")).unwrap(), seal);
        assert_eq!(load_preimages(&journal).unwrap(), observations);
        assert_eq!(
            fs::read(crate::RunBundle::path(root.path())).unwrap(),
            bundle
        );
    }
    // Only an explicit retry by the completion owner binds the original result.
    // This is not automatic receipt recovery or an Execution rerun.
    let (_current, _lease) = record.lock_current().unwrap();
    super::super::completion::Completion::commit(&record, &result).unwrap();
    let current = RunRecord::read(root.path()).unwrap();
    assert_eq!(current.state, super::super::RunRecordState::Completed);
    current.require_stopped().unwrap();
    assert_eq!(
        crate::RunBundle::read(root.path()).unwrap().run.state,
        result.state
    );
    assert_eq!(fs::read(&candidate).unwrap(), b"retained");
    assert_eq!(identity(&candidate), original_identity);
    assert_eq!(fs::read(journal.join("sealed-v1")).unwrap(), seal);
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
    // This fixture is a legacy Run (schema 1), so its historical Bundle is v4.
    // Protocol-aware v5 Bundles require a matching completion receipt.
    historical["schema_version"] = serde_json::json!(4);
    historical["run"]["run_id"] = serde_json::json!(record.run_id);
    historical["agentctl"]["run_id"] = serde_json::json!(record.run_id);
    historical["safety"]["filesystem_changes_staged"] = serde_json::json!(true);
    historical["executor_observations"] =
        serde_json::to_value(pvisor_core::ExecutorObservations::default()).unwrap();
    historical["filesystem"] = serde_json::json!({
        "state":"staged", "target":root.path().join("target"), "upper":root.path().join("upper"),
        "changed_files":0, "whiteouts":0, "changes":[], "sample_paths":[]
    });
    let bundle: crate::RunBundle = serde_json::from_value(historical).unwrap();
    bundle.write(root.path()).unwrap();
    let before = std::fs::read(crate::RunBundle::path(root.path())).unwrap();
    let response = SessionService::review(
        &ServiceContext::default(),
        ReviewRequest {
            session: selection(&record),
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
    std::fs::write(root.path().join("upper/change"), "first").unwrap();
    let context = ServiceContext::default();
    let create = || {
        SessionService::create_workspace_checkpoint(
            &context,
            WorkspaceCreateRequest {
                session: selection(&record),
                request_id: Some("scoped-request".into()),
            },
        )
    };
    let first = create().unwrap();
    assert!(!first.reused);
    std::fs::write(root.path().join("upper/change"), "second").unwrap();
    let second = create().unwrap();
    assert!(second.reused);
    assert_eq!(
        first.checkpoint.checkpoint_id,
        second.checkpoint.checkpoint_id
    );
    assert_eq!(
        std::fs::read(second.checkpoint.upper_snapshot.join("change")).unwrap(),
        b"first"
    );
    SessionService::delete_workspace_checkpoint(
        &context,
        WorkspaceDeleteRequest {
            session: selection(&record),
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
        SessionService::collect_workspace_transactions(
            &context,
            WorkspaceGcRequest {
                session: selection(&record)
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
        SessionService::create_selected_workspace_checkpoint(&context, &selected, None)
            .unwrap_err(),
    );
    conflict(
        SessionService::delete_selected_workspace_checkpoint(&context, &selected, "missing")
            .unwrap_err(),
    );
    conflict(
        SessionService::collect_selected_workspace_transactions(&context, &selected).unwrap_err(),
    );
    assert!(pending.is_dir(), "stale GC must not collect anything");
    assert_eq!(
        std::fs::read_dir(root.path().join(crate::CHECKPOINTS_DIR))
            .unwrap()
            .count(),
        1
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
    let response = SessionService::apply_command(&context, apply_request(&record)).unwrap();
    assert_eq!(response.status, "blocked");
    assert!(
        response
            .blockers
            .join(" ")
            .contains("stale Session, Execution, or workspace generation")
    );
    assert!(response.execution.is_none());
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
        session_id: record.run_id.clone(),
        execution_id: record.execution_id.clone(),
        generation: Some("6".into()),
    };
    let context = ServiceContext {
        expected_target: Some(&target),
        ..Default::default()
    };
    conflict(
        SessionService::drop(
            &context,
            DropRequest {
                session: selection(&record),
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
        SessionService::drop(
            &context,
            DropRequest {
                session: selection(&record)
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
async fn capture_rechecks_request_context_after_waiting_for_session_lease() {
    use crate::runtime::session_execution::SESSION_SCHEMA_VERSION;
    use crate::{Session, SessionState};
    use pvisor_core::operation::SnapshotRamStorage;
    use std::time::Duration;

    for state in [SessionState::Running, SessionState::Suspended] {
        for change in ["cancel", "generation", "authority"] {
            let root = tempfile::tempdir().unwrap();
            let record = fixture(root.path());
            let mut config = crate::RunConfig::default();
            config.run.executor = crate::RunExecutorKind::Vm;
            config.overlaynet.mode = crate::OverlayNetMode::Off;
            let session: Session = serde_json::from_value(serde_json::json!({
                "version":SESSION_SCHEMA_VERSION,"run_id":record.run_id,"root":root.path(),
                "active_stage":root.path(),"previous_stage":root.path(),"active_execution":record.execution_id,
                "config":config,"spec":pvisor_core::RunSpec::process("session-service", "sh", "/bin/sh"),
                "state":state,"head":null,"checkpoints":{},"requests":{},"resumes":{},"forks":{},"stores":[]
            })).unwrap();
            session.write().unwrap();
            let before = serde_json::to_value(&session).unwrap();
            let lease = session.lock().unwrap();
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
            let mut pending = Box::pin(SessionService::capture_execution(
                &context,
                CaptureRequest {
                    session: selection(&record),
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
                serde_json::to_value(session.current().unwrap()).unwrap(),
                before
            );
        }
    }
}

#[tokio::test]
async fn resume_replay_is_fenced_before_embedded_launcher() {
    use crate::runtime::session_execution::{ResumeRequest as Receipt, SESSION_SCHEMA_VERSION};
    use crate::{Session, SessionState};
    let root = tempfile::tempdir().unwrap();
    let record = fixture(root.path());
    let mut config = crate::RunConfig::default();
    config.run.executor = crate::RunExecutorKind::Vm;
    config.overlaynet.mode = crate::OverlayNetMode::Off;
    let session: Session = serde_json::from_value(serde_json::json!({
        "version":SESSION_SCHEMA_VERSION,"run_id":record.run_id,"root":root.path(),
        "active_stage":root.path(),"previous_stage":root.path(),"active_execution":"new-execution",
        "config":config,"spec":pvisor_core::RunSpec::process("session-service", "sh", "/bin/sh"),
        "state":"suspended","head":null,"checkpoints":{},"requests":{},
        "resumes":{"replay":{"stage":root.path(),"eager_ram":false}},"forks":{},"stores":[]
    }))
    .unwrap();
    session.write().unwrap();
    // Callback success alone cannot make a Restoring or unbound Session Finished.
    assert!(super::lifecycle::confirm_restored_completion(root.path(), &record.run_id).is_err());
    let mut finished = session.clone();
    let mut stored = serde_json::to_value(&finished).unwrap();
    stored["state"] = serde_json::to_value(SessionState::Terminal).unwrap();
    stored["active_execution"] = serde_json::json!(record.execution_id.clone().unwrap());
    finished = serde_json::from_value(stored).unwrap();
    finished.write().unwrap();
    super::lifecycle::confirm_restored_completion(root.path(), &record.run_id).unwrap();
    assert!(super::lifecycle::confirm_restored_completion(root.path(), "another-session").is_err());
    session.write().unwrap();
    let launched = AtomicBool::new(false);
    let error = SessionService::resume(
        &ServiceContext::default(),
        ResumeRequest {
            session: selection(&record),
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
    let error = SessionService::fork_execution(
        &ServiceContext::default(),
        ExecutionForkRequest {
            session: selection(&record),
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
    assert!(!root.path().join("executions").exists());

    let mut admitted = session.clone();
    let mut stored = serde_json::to_value(&admitted).unwrap();
    stored["state"] = serde_json::to_value(SessionState::Restoring).unwrap();
    stored["active_stage"] = serde_json::json!(root.path().join("executions/restore"));
    stored["resumes"]["restore"] = serde_json::to_value(Receipt {
        stage: root.path().join("executions/restore"),
        eager_ram: false,
    })
    .unwrap();
    admitted = serde_json::from_value(stored).unwrap();
    assert!(admitted.owns_restore_transition(&admitted, "restore"));
    for variant in 0..5 {
        let mut value = serde_json::to_value(&admitted).unwrap();
        match variant {
            0 => value["active_stage"] = serde_json::json!(root.path().join("executions/other")),
            1 => value["active_execution"] = serde_json::json!("other-execution"),
            2 => value["head"] = serde_json::json!("other-head"),
            3 => value["resumes"]["restore"]["eager_ram"] = serde_json::json!(true),
            _ => {
                value["resumes"]["another"] = serde_json::to_value(Receipt {
                    stage: admitted.active_stage().to_owned(),
                    eager_ram: false,
                })
                .unwrap()
            }
        }
        let current: Session = serde_json::from_value(value).unwrap();
        assert!(!current.owns_restore_transition(&admitted, "restore"));
    }
}
