use super::*;
use crate::{
    scheduler::{Scheduler, SchedulerConfig},
    *,
};
use pvisor_core::{ExecutorKind, IsolationKind, RunResult, RunSpec};
fn key(id: &str) -> LeaseKey {
    LeaseKey {
        task_id: id.into(),
        worker_id: "node".into(),
        incarnation: "epoch".into(),
        generation: 1,
    }
}
fn request(retire: Option<u64>) -> ArtifactGcRequest {
    ArtifactGcRequest {
        version: CLUSTER_VERSION,
        retire_before_ms: retire,
        max_objects: 4096,
    }
}
fn empty() -> Snapshot {
    Snapshot {
        live: BTreeSet::new(),
        retained: vec![],
        retire: vec![],
        legacy: false,
    }
}
fn plan(store: &ArtifactStore, snapshot: Snapshot, now: u64) -> ArtifactGcPlan {
    store
        .gc_plan(request(None), snapshot, store.gc_sequence().unwrap(), now)
        .unwrap()
}
fn scheduler(path: &Path) -> Scheduler {
    Scheduler::open(path, SchedulerConfig::default()).unwrap()
}
fn start(s: &mut Scheduler, id: &str, at: u64) -> LeaseKey {
    let class = ExecutionClass {
        executor: ExecutorKind::Process,
        isolation: IsolationKind::HostProcess,
    };
    let resources = Resources {
        slots: 1,
        memory_bytes: 64 * 1024 * 1024,
        cpu_millis: 250,
    };
    if s.workers().is_empty() {
        s.register(
            WorkerRegistration {
                version: CLUSTER_VERSION,
                id: "node".into(),
                incarnation: "epoch".into(),
                capacity: resources,
                execution: vec![class.clone()],
                labels: BTreeMap::new(),
                cache_keys: vec![],
                vm_control_protocol: None,
                vm_control_actions: vec![],
                artifact_protocol: Some(CLUSTER_VERSION),
                artifact_export: None,
                gateway: None,
                cpu_observation_protocol: None,
                cpu_qos_classes: vec![],
                execution_restore_protocol: None,
                parked_execution_suspend_protocol: None,
                environment_support: None,
            },
            at,
        )
        .unwrap();
    }
    let mut run = RunSpec::process(id, "GC fixture", "/bin/true");
    let pvisor_core::RunInvocation::Process(process) = &mut run.invocation;
    process.inherit_env = false;
    s.submit(
        TaskSpec {
            version: CLUSTER_VERSION,
            id: id.into(),
            tenant: "tenant".into(),
            run,
            execution: class,
            resources,
            labels: BTreeMap::new(),
            cache_keys: vec![],
            retain_bundle: true,
            retain_artifacts: None,
            environment: None,
            gateway: None,
            restore: None,
            cpu_qos: None,
        },
        at,
    )
    .unwrap();
    s.poll(
        PollRequest {
            worker_id: "node".into(),
            incarnation: "epoch".into(),
            active: vec![],
            available: resources,
            max_assignments: 1,
            admission: None,
        },
        at + 1,
    )
    .unwrap()
    .assignments
    .remove(0)
    .lease
    .key
}
fn archive(store: &ArtifactStore, key: LeaseKey, shared: Option<BlobRef>) -> Completion {
    let result: RunResult = serde_json::from_value(serde_json::json!({ "run_id": key.task_id, "attempt_id": "native", "state": "completed", "started_at_unix_ms": 1, "finished_at_unix_ms": 2, "exit_code": 0 })).unwrap();
    let body = store
        .put(&serde_json::to_vec(&serde_json::json!({"schema_version":4,"run":result})).unwrap())
        .unwrap();
    let mut files = vec![ArtifactFile {
        name: "run-bundle.json".into(),
        bytes: body.bytes,
        digest: body.digest.clone(),
        chunks: vec![body],
    }];
    if let Some(shared) = shared {
        files.push(ArtifactFile {
            name: "shared.bin".into(),
            bytes: shared.bytes,
            digest: shared.digest.clone(),
            chunks: vec![shared],
        });
    }
    let manifest = ArtifactManifest {
        version: CLUSTER_VERSION,
        key: key.clone(),
        files,
    };
    let reference = store.put(&serde_json::to_vec(&manifest).unwrap()).unwrap();
    Completion {
        key,
        result: Some(result),
        error: None,
        artifacts: Some(reference),
        artifact_error: None,
    }
}
#[test]
fn verification_remains_pinned_through_commit_and_retirement_preserves_exact_receipt_after_restart()
{
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let mut s = scheduler(&path);
    let key = start(&mut s, "first", 0);
    let store = s.artifact_store();
    let completion = archive(&store, key.clone(), None);
    let reference = completion.artifacts.as_ref().unwrap();
    let verified = store.verify(reference, &key).unwrap();
    let before_commit = plan(&store, empty(), 3);
    assert_eq!(before_commit.objects.len(), 2);
    let report = store
        .apply_gc(&before_commit.id, 4, |_| Ok(BTreeSet::new()))
        .unwrap();
    assert_eq!((report.deleted_objects, report.skipped_objects), (0, 2));
    s.complete_verified(completion.clone(), Some(verified), 5)
        .unwrap();
    let snapshot = s.artifact_gc_snapshot(&request(None), 6).unwrap();
    assert!(plan(&store, snapshot, 6).objects.is_empty());
    drop(store);
    drop(s);
    let mut s = scheduler(&path);
    let store = s.artifact_store();
    let sequence = store.gc_sequence().unwrap();
    let snapshot = s.artifact_gc_snapshot(&request(Some(6)), 7).unwrap();
    let retire = store
        .gc_plan(request(Some(6)), snapshot, sequence, 7)
        .unwrap();
    assert_eq!(retire.retire.len(), 1);
    assert_eq!(retire.objects.len(), 2);
    // Preview preserves both state and evidence.
    assert!(s.task("first").unwrap().artifact_retired_at_ms.is_none());
    assert!(store.get(reference).is_ok());
    let report = store
        .apply_gc(&retire.id, 8, |entries| s.retire_artifacts(entries, 8))
        .unwrap();
    assert_eq!((report.retired_tasks, report.deleted_objects), (1, 2));
    assert_eq!(store.storage_usage().unwrap().stored_objects, 0);
    let receipt = s.complete(completion.clone(), 9).unwrap();
    assert_eq!(receipt.phase, TaskPhase::Succeeded);
    assert_eq!(receipt.updated_at_ms, 5);
    assert_eq!(receipt.artifact_retired_at_ms, Some(8));
    assert_eq!(receipt.artifacts.as_ref(), Some(reference));
    let again = store
        .apply_gc(&retire.id, 9, |_| panic!("cached plan must not repeat WAL"))
        .unwrap();
    assert_eq!(again.deleted_objects, 2);
    drop(store);
    drop(s);
    let mut s = scheduler(&path);
    assert_eq!(
        s.complete(completion.clone(), 10)
            .unwrap()
            .artifact_retired_at_ms,
        Some(8)
    );
    let mut conflicting = completion;
    conflicting.artifact_error = Some("changed".into());
    assert!(s.complete(conflicting, 10).is_err());
    assert_eq!(
        s.artifact_store().storage_usage().unwrap().stored_objects,
        0
    );
    assert!(s.artifact_store().stored_plan(&retire.id, 10).is_err());
}
#[test]
fn upload_pins_survive_restart_and_partial_journal_tail_and_protect_reused_chunks() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("objects");
    let store = ArtifactStore::open(&root).unwrap();
    let key = key("upload");
    let id = lease_id(&key);
    let reference = store.put_for_lease(&key, b"sealed chunk").unwrap();
    let orphan = store.put(b"orphan").unwrap();
    let mut snapshot = empty();
    snapshot.live.insert(id.clone());
    let first = plan(&store, snapshot.clone(), 1);
    assert_eq!(first.objects, vec![orphan]);
    store
        .apply_gc(&first.id, 2, |_| Ok(snapshot.live.clone()))
        .unwrap();
    assert!(store.get(&reference).is_ok());
    let pin_path = root.join(".lease-pins").join(&id);
    let original_len = fs::metadata(&pin_path).unwrap().len();
    OpenOptions::new()
        .append(true)
        .open(&pin_path)
        .unwrap()
        .write_all(b"partial-unacknowledged-frame")
        .unwrap();
    drop(store);
    let store = ArtifactStore::open(&root).unwrap();
    assert_eq!(fs::metadata(&pin_path).unwrap().len(), original_len);
    assert_eq!(
        store.put_for_lease(&key, b"sealed chunk").unwrap(),
        reference
    );
    assert!(plan(&store, snapshot.clone(), 3).objects.is_empty());
    let expired = plan(&store, empty(), 4);
    let report = store
        .apply_gc(&expired.id, 5, |_| Ok(BTreeSet::new()))
        .unwrap();
    assert_eq!(report.deleted_objects, 1);
    assert!(!pin_path.exists());
    assert_eq!(store.storage_usage().unwrap().stored_bytes, 0);
    // A stale preview cannot delete a freshly recreated content address.
    let recreated = store.put(b"sealed chunk").unwrap();
    assert_eq!(recreated, reference);
    let stale = store.stored_plan(&expired.id, 5).unwrap();
    let mut uncached = stale;
    uncached.report = None;
    assert_eq!(
        store
            .sweep(uncached, BTreeSet::new(), 6)
            .unwrap()
            .deleted_objects,
        0
    );
    assert!(store.get(&recreated).is_ok());
}
#[test]
fn protected_download_survives_retirement_and_restart_and_shared_chunks_outlive_other_roots() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("journal");
    let now = pvisor_core::unix_now_ms();
    let mut s = scheduler(&path);
    let store = s.artifact_store();
    let shared = store.put(b"shared evidence").unwrap();
    let key = start(&mut s, "old", 0);
    let first = archive(&store, key, Some(shared.clone()));
    s.complete(first.clone(), 3).unwrap();
    let key = start(&mut s, "new", 4);
    let second = archive(&store, key, Some(shared.clone()));
    s.complete(second.clone(), 7).unwrap();
    let download = store
        .begin_download(first.artifacts.as_ref().unwrap(), now)
        .unwrap();
    let snapshot = s.artifact_gc_snapshot(&request(Some(4)), 8).unwrap();
    let preview = plan(&store, snapshot, now);
    assert_eq!(preview.retire.len(), 1);
    assert!(preview.objects.is_empty());
    store
        .apply_gc(&preview.id, now, |entries| s.retire_artifacts(entries, 8))
        .unwrap();
    drop(store);
    drop(s);
    let mut s = scheduler(&path);
    let store = s.artifact_store();
    assert_eq!(
        store
            .renew_download(&download.id, now + 1000)
            .unwrap()
            .manifest,
        download.manifest
    );
    let snapshot = s.artifact_gc_snapshot(&request(None), 9).unwrap();
    assert!(plan(&store, snapshot, now + 1001).objects.is_empty());
    store.release_download(&download.id).unwrap();
    let snapshot = s.artifact_gc_snapshot(&request(None), 10).unwrap();
    let preview = plan(&store, snapshot, now + 1002);
    assert_eq!(preview.objects.len(), 2);
    assert!(!preview.objects.contains(&shared));
    store
        .apply_gc(&preview.id, now + 1003, |_| Ok(BTreeSet::new()))
        .unwrap();
    assert!(store.get(&shared).is_ok());
    assert!(store.get(second.artifacts.as_ref().unwrap()).is_ok());
    assert!(store.get(first.artifacts.as_ref().unwrap()).is_err());
    let last = store
        .begin_download(second.artifacts.as_ref().unwrap(), now + 1004)
        .unwrap();
    let snapshot = s.artifact_gc_snapshot(&request(Some(8)), 11).unwrap();
    let preview = plan(&store, snapshot, now + 1005);
    store
        .apply_gc(&preview.id, now + 1006, |entries| {
            s.retire_artifacts(entries, 11)
        })
        .unwrap();
    let snapshot = s.artifact_gc_snapshot(&request(None), 12).unwrap();
    let after_expiry = plan(&store, snapshot, now + 1004 + DOWNLOAD_TTL + 1);
    store
        .apply_gc(&after_expiry.id, now + 1004 + DOWNLOAD_TTL + 2, |_| {
            Ok(BTreeSet::new())
        })
        .unwrap();
    assert_eq!(store.storage_usage().unwrap().stored_objects, 0);
    assert!(
        store
            .renew_download(&last.id, now + 1004 + DOWNLOAD_TTL + 2)
            .is_err()
    );
}
#[test]
fn authority_rejects_replacement_wal_and_accepts_relocated_matching_backup() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("shared.one");
    let root = path.with_extension("artifacts");
    let mut s = scheduler(&path);
    let key = start(&mut s, "authority", 0);
    let completion = archive(&s.artifact_store(), key, None);
    s.complete(completion.clone(), 3).unwrap();
    assert!(Scheduler::open(&temp.path().join("shared.two"), SchedulerConfig::default()).is_err());
    let reference = completion.artifacts.as_ref().unwrap();
    let manifest = s.artifact_store().read_manifest(reference).unwrap();
    fs::copy(&path, temp.path().join("shared.two")).unwrap();
    assert!(Scheduler::open(&temp.path().join("shared.two"), SchedulerConfig::default()).is_err());
    drop(s);
    fs::remove_file(temp.path().join("shared.two")).unwrap();
    assert!(Scheduler::open(&temp.path().join("shared.two"), SchedulerConfig::default()).is_err());
    let backup = temp.path().join("relocated/backup.wal");
    let backup_root = backup.with_extension("artifacts");
    fs::create_dir_all(&backup_root).unwrap();
    fs::copy(&path, &backup).unwrap();
    fs::copy(
        root.join(".authority.json"),
        backup_root.join(".authority.json"),
    )
    .unwrap();
    for reference in refs(reference, &manifest).unwrap() {
        fs::create_dir_all(backup_root.join(&reference.digest[..2])).unwrap();
        fs::copy(
            root.join(&reference.digest[..2]).join(&reference.digest),
            backup_root
                .join(&reference.digest[..2])
                .join(&reference.digest),
        )
        .unwrap();
    }
    let mut restored = scheduler(&backup);
    assert_eq!(
        restored.complete(completion, 4).unwrap().phase,
        TaskPhase::Succeeded
    );
    let store = restored.artifact_store();
    let preview = plan(
        &store,
        restored.artifact_gc_snapshot(&request(None), 4).unwrap(),
        4,
    );
    assert!(preview.objects.is_empty());
    drop(store);
    drop(restored);
    // Even the same pathname cannot reset authority by replacing the WAL.
    fs::remove_file(&backup).unwrap();
    assert!(Scheduler::open(&backup, SchedulerConfig::default()).is_err());
}

#[test]
fn concurrent_upload_cannot_be_unlinked_between_publication_and_durable_lease_pin() {
    let temp = tempfile::tempdir().unwrap();
    let store = ArtifactStore::open(&temp.path().join("objects")).unwrap();
    let key = key("concurrent");
    let id = lease_id(&key);
    let existing = store
        .put_for_lease(&key, b"earlier acknowledged chunk")
        .unwrap();
    let target = store.put(b"planned orphan reused by live upload").unwrap();
    let preview = plan(&store, empty(), 1);
    assert_eq!(preview.objects.len(), 2);
    let entry = store.quota.gc.inner.lock().unwrap().leases[&id].clone();
    let held = entry.lock().unwrap();
    let producer = {
        let store = store.clone();
        let key = key.clone();
        std::thread::spawn(move || {
            store.put_for_lease(&key, b"planned orphan reused by live upload")
        })
    };
    // The producer now holds the publication barrier while waiting for the
    // per-lease append lock. GC must wait until that durable pin is installed.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while store.quota.gc.barrier.try_write().is_ok() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    let sweep = {
        let store = store.clone();
        let plan_id = preview.id.clone();
        std::thread::spawn(move || store.apply_gc(&plan_id, 2, |_| Ok(BTreeSet::from([id]))))
    };
    drop(held);
    assert_eq!(producer.join().unwrap().unwrap(), target);
    let report = sweep.join().unwrap().unwrap();
    assert_eq!((report.deleted_objects, report.skipped_objects), (0, 2));
    assert!(store.get(&existing).is_ok());
    assert!(store.get(&target).is_ok());
}

#[test]
fn small_plans_bound_stale_lease_metadata_io_and_eventually_reclaim_all_orphans() {
    let temp = tempfile::tempdir().unwrap();
    let store = ArtifactStore::open(&temp.path().join("objects")).unwrap();
    for id in ["one", "two", "three"] {
        store.put_for_lease(&key(id), id.as_bytes()).unwrap();
    }
    for at in 1..=6 {
        let preview = store
            .gc_plan(
                ArtifactGcRequest {
                    version: CLUSTER_VERSION,
                    retire_before_ms: None,
                    max_objects: 1,
                },
                empty(),
                store.gc_sequence().unwrap(),
                at,
            )
            .unwrap();
        assert!(preview.objects.len() <= 1);
        let before = fs::read_dir(store.root.join(".lease-pins"))
            .unwrap()
            .count();
        let report = store
            .apply_gc(&preview.id, at, |_| Ok(BTreeSet::new()))
            .unwrap();
        let after = fs::read_dir(store.root.join(".lease-pins"))
            .unwrap()
            .count();
        assert!(before - after <= 1);
        assert!(report.deleted_objects <= 1);
    }
    assert_eq!(store.storage_usage().unwrap().stored_objects, 0);
}

#[test]
fn invalid_plans_and_pin_corruption_fail_closed_and_do_not_delete_external_data() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("objects");
    let store = ArtifactStore::open(&root).unwrap();
    let reference = store.put(b"proof").unwrap();
    let preview = plan(&store, empty(), 1);
    assert!(
        store
            .apply_gc(&preview.id, preview.expires_at_ms, |_| unreachable!())
            .is_err()
    );
    assert!(
        store
            .apply_gc(&"0".repeat(64), 2, |_| unreachable!())
            .is_err()
    );
    let other = ArtifactStore::open(&temp.path().join("other")).unwrap();
    assert!(other.apply_gc(&preview.id, 2, |_| unreachable!()).is_err());
    let snapshot = Snapshot {
        legacy: true,
        ..empty()
    };
    assert!(plan(&store, snapshot, 2).objects.is_empty());
    assert!(store.get(&reference).is_ok());
    let key = key("corruption");
    store.put_for_lease(&key, b"leased proof").unwrap();
    let pin_path = root.join(".lease-pins").join(lease_id(&key));
    let bytes = fs::read(&pin_path).unwrap();
    let mut corrupt = bytes.clone();
    corrupt[0] = b'x';
    fs::write(&pin_path, corrupt).unwrap();
    drop(store);
    assert!(ArtifactStore::open(&root).is_err());
    fs::write(&pin_path, bytes).unwrap();
    let store = ArtifactStore::open(&root).unwrap();
    let preview = plan(&store, empty(), 3);
    let external = temp.path().join("external");
    fs::write(&external, b"proof").unwrap();
    let path = store.path(&reference).unwrap();
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(&external, &path).unwrap();
    assert!(
        store
            .apply_gc(&preview.id, 4, |_| Ok(BTreeSet::new()))
            .is_err()
    );
    assert_eq!(fs::read(external).unwrap(), b"proof");
}
