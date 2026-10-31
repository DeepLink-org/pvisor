//! Public runtime project handoff contracts; no VM, FUSE or root privileges needed.
#![cfg(target_os = "linux")]

use pvisor::job_service::{ExportRequest, RuntimeJobService, ServiceContext};
use pvisor::{RunBundle, RunRecord};
use serde_json::json;
use std::fs;
use std::os::unix::{
    ffi::OsStrExt,
    fs::{PermissionsExt, symlink},
};
use std::path::{Path, PathBuf};

struct Fixture {
    root: tempfile::TempDir,
    stage: PathBuf,
    project: PathBuf,
    upper: PathBuf,
    record: RunRecord,
}
impl Fixture {
    fn new(rootfs: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let stage = root.path().join("stage");
        fs::create_dir(&project).unwrap();
        for dir in ["upper", "work", "merged"] {
            fs::create_dir_all(stage.join(dir)).unwrap();
        }
        let upper = if rootfs {
            stage.join("upper").join(project.strip_prefix("/").unwrap())
        } else {
            stage.join("upper")
        };
        fs::create_dir_all(&upper).unwrap();
        let record: RunRecord = serde_json::from_value(json!({
            "schema_version":1,"run_id":"handoff-job","session_id":"handoff-session","attempt_id":"handoff-attempt",
            "agent":"test","pid":0,"command":["/bin/true"],"state":"completed",
            "started_at_unix_ms":10,"finished_at_unix_ms":20,"storage":stage,"workspace":project,"network":{},
            "overlay":{"id":"handoff-job","generation":7,"target":if rootfs { Path::new("/") } else { &project },
                "upper":{"upper_dir":stage.join("upper"),"work_dir":stage.join("work")},
                "merged_dir":stage.join("merged"),"stage_dir":stage,"auto_apply":false,"state":"staged"}
        })).unwrap();
        record.write().unwrap();
        let this = Self {
            root,
            stage,
            project,
            upper,
            record,
        };
        this.bundle();
        this
    }
    fn bundle(&self) {
        self.bundle_state("completed");
    }
    fn bundle_state(&self, state: &str) -> serde_json::Value {
        self.bundle_outcome(state, Some(0))
    }
    fn bundle_outcome(&self, state: &str, exit_code: Option<i32>) -> serde_json::Value {
        let result = serde_json::from_value(json!({
            "run_id":"handoff-job","attempt_id":"handoff-attempt","state":state,
            "started_at_unix_ms":10,"finished_at_unix_ms":20,"exit_code":exit_code
        }))
        .unwrap();
        let agentctl = serde_json::from_value(json!({"run_id":"handoff-job","attempt_id":"handoff-attempt","directive":{"kind":"continue"},"clients":[]})).unwrap();
        RunBundle::capture(&self.record, &result, agentctl, false)
            .unwrap()
            .write(&self.stage)
            .unwrap();
        serde_json::to_value(result).unwrap()
    }
    fn completion(&mut self, state: &str, exit_code: Option<i32>) {
        self.record.schema_version = 2;
        self.record.completion_protocol = Some(1);
        self.record.write().unwrap();
        let result = self.bundle_outcome(state, exit_code);
        let bytes = fs::read(RunBundle::path(&self.stage)).unwrap();
        fs::write(self.stage.join("completion.json"), serde_json::to_vec(&json!({
            "version":1, "result":result, "bundle_blake3":blake3::hash(&bytes).to_hex().to_string()
        })).unwrap()).unwrap();
    }
    fn request(&self, paths: &[&str], all: bool) -> ExportRequest {
        ExportRequest {
            job: pvisor::job_service::JobSelection {
                selector: Some(self.stage.clone()),
                storage: self.stage.clone(),
            },
            output: self.root.path().join("export"),
            paths: paths.iter().map(PathBuf::from).collect(),
            all,
        }
    }
    fn export(
        &self,
        paths: &[&str],
        all: bool,
    ) -> anyhow::Result<pvisor::job_service::ExportResponse> {
        RuntimeJobService::export(&ServiceContext::default(), self.request(paths, all))
    }
    fn no_output(&self) {
        assert!(!self.root.path().join("export").exists());
        assert!(!fs::read_dir(self.root.path()).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .as_bytes()
                .starts_with(b".pvisor-export-")
        }));
    }
}
fn xattr(path: &Path, key: &str, bytes: &[u8]) {
    pvisor_overlay_core::sys::set_xattr(path, std::ffi::OsStr::new(key), bytes, 0).unwrap();
}

#[test]
fn root_stage_exports_only_project_files_and_logical_manifest_without_consuming() {
    let f = Fixture::new(true);
    fs::write(f.project.join("old"), b"keep in host").unwrap();
    fs::write(f.upper.join(".wh.old"), b"").unwrap();
    fs::write(f.upper.join("report"), b"artifact").unwrap();
    fs::create_dir(f.upper.join("empty")).unwrap();
    fs::create_dir_all(f.stage.join("upper/etc")).unwrap();
    fs::write(f.stage.join("upper/etc/experiment"), b"not project").unwrap();
    xattr(
        &f.upper.join("report"),
        pvisor_overlay_core::OVERRIDE_STAT_XATTR,
        b"123:456:100751",
    );
    xattr(
        &f.stage.join("upper"),
        pvisor_overlay_core::PRIVATE_BACKING_XATTR,
        b"1",
    );
    xattr(
        &f.upper.join("empty"),
        pvisor_overlay_core::OVERRIDE_STAT_XATTR,
        b"123:456:40750",
    );
    let before = fs::read(f.stage.join("run.json")).unwrap();
    let response = f.export(&[], true).unwrap();
    assert_eq!(response.job_id, "handoff-job");
    assert_eq!(response.schema_version, 1);
    assert_eq!(response.attempt_id.as_deref(), Some("handoff-attempt"));
    assert_eq!(response.state, pvisor_core::RunState::Completed);
    assert_eq!(response.exit_code, Some(0));
    assert!(!response.partial_results);
    assert_eq!(response.project_root, f.project);
    assert_eq!(
        fs::read(response.destination.join("files/report")).unwrap(),
        b"artifact"
    );
    assert!(!response.destination.join("files/etc").exists());
    assert!(!response.destination.join("files/.wh.old").exists());
    assert!(!response.destination.join("files/empty").exists());
    assert!(response.entries.iter().any(|c| c.path == "empty"));
    assert_eq!(
        response
            .entries
            .iter()
            .find(|c| c.path == "report")
            .unwrap()
            .mode,
        Some(0o751)
    );
    assert_eq!(response.deletions.len(), 1);
    assert_eq!(response.deletions[0].path, "old");
    let packaged = response.destination.join("files/report");
    assert_eq!(
        fs::metadata(&packaged).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    assert!(
        pvisor_overlay_core::sys::list_xattrs(&packaged)
            .unwrap()
            .is_empty()
    );
    let manifest = fs::read(response.destination.join("manifest.json")).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&manifest).unwrap(),
        serde_json::to_value(response).unwrap()
    );
    assert_eq!(fs::read(f.project.join("old")).unwrap(), b"keep in host");
    assert!(!f.project.join("report").exists());
    assert!(f.upper.join("report").exists());
    assert_eq!(before, fs::read(f.stage.join("run.json")).unwrap());
}

#[test]
fn ordinary_workspace_selection_does_not_export_unselected_dependencies() {
    let f = Fixture::new(false);
    fs::create_dir(f.upper.join("src")).unwrap();
    fs::write(f.upper.join("src/main"), b"code").unwrap();
    fs::write(f.upper.join("dependency"), b"not selected").unwrap();
    fs::create_dir(f.project.join("removed-dir")).unwrap();
    fs::write(f.upper.join(".wh.removed-dir"), b"").unwrap();
    let response = f.export(&["src", "removed-dir"], false).unwrap();
    assert!(response.destination.join("files/src/main").exists());
    assert!(!response.destination.join("files/dependency").exists());
    assert_eq!(response.deletions[0].path, "removed-dir");
}

#[test]
fn selection_must_be_explicit_and_project_relative_even_with_all() {
    let f = Fixture::new(true);
    for (paths, all) in [
        (vec![], false),
        (vec![""], false),
        (vec!["/etc/passwd"], false),
        (vec!["../project"], false),
        (vec!["src/../secret"], true),
        (vec!["."], true),
        (vec!["valid"], true),
    ] {
        assert!(f.export(&paths, all).is_err(), "{paths:?}");
        f.no_output();
    }
}

#[test]
fn symlink_special_and_opaque_are_unsupported_without_publishing() {
    for case in [
        "symlink",
        "ancestor",
        "fifo",
        "opaque-marker",
        "opaque-xattr",
        "opaque-above-project",
    ] {
        let f = Fixture::new(true);
        let outside = f.root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret"), b"not an artifact").unwrap();
        let selection = match case {
            "symlink" => {
                symlink(outside.join("secret"), f.upper.join("link")).unwrap();
                "link"
            }
            "ancestor" => {
                symlink(&outside, f.upper.join("link")).unwrap();
                "link/secret"
            }
            "fifo" => {
                let path =
                    std::ffi::CString::new(f.upper.join("pipe").as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
                "pipe"
            }
            _ => {
                fs::create_dir(f.upper.join("dir")).unwrap();
                fs::write(f.upper.join("dir/file"), b"artifact").unwrap();
                if case == "opaque-marker" {
                    fs::write(f.upper.join("dir/.wh..wh..opq"), b"").unwrap();
                } else if case == "opaque-xattr" {
                    xattr(&f.upper.join("dir"), "user.overlay.opaque", b"y");
                } else {
                    fs::write(f.stage.join("upper/.wh..wh..opq"), b"").unwrap();
                }
                "dir/file"
            }
        };
        // A symlink ancestor has no inventory child; it must not silently export
        // an empty success just because the selected leaf wasn't enumerated.
        assert!(f.export(&[selection], false).is_err(), "{case}");
        f.no_output();
        assert_eq!(
            fs::read(outside.join("secret")).unwrap(),
            b"not an artifact"
        );
    }
}

#[test]
fn existing_outputs_aliases_and_protected_overlap_are_refused() {
    let f = Fixture::new(true);
    fs::write(f.upper.join("file"), b"artifact").unwrap();
    let output = f.root.path().join("export");
    fs::create_dir(&output).unwrap();
    fs::write(output.join("sentinel"), b"keep").unwrap();
    assert!(f.export(&[], true).is_err());
    assert_eq!(fs::read(output.join("sentinel")).unwrap(), b"keep");
    fs::remove_dir_all(&output).unwrap();
    symlink(&f.project, &output).unwrap();
    assert!(f.export(&[], true).is_err());
    fs::remove_file(&output).unwrap();
    for output in [
        f.project.join("new"),
        f.stage.join("new"),
        f.stage.join("upper/new"),
        pvisor::host_transport::host_authority_root()
            .unwrap()
            .join("export-test"),
    ] {
        let mut request = f.request(&[], true);
        request.output = output.clone();
        assert!(
            RuntimeJobService::export(&ServiceContext::default(), request).is_err(),
            "{}",
            output.display()
        );
        assert!(!output.exists());
    }
    let alias = f.root.path().join("project-alias");
    symlink(&f.project, &alias).unwrap();
    let mut request = f.request(&[], true);
    request.output = alias.join("new");
    assert!(RuntimeJobService::export(&ServiceContext::default(), request).is_err());
    f.no_output();
}

#[test]
fn running_unknown_drop_and_scope_disagreement_are_refused() {
    for case in [
        "running",
        "unknown",
        "discarded",
        "active",
        "applied",
        "no-workspace",
        "root-workspace",
        "target",
        "bundle-spec",
    ] {
        let mut f = Fixture::new(false);
        fs::write(f.upper.join("file"), b"artifact").unwrap();
        match case {
            "running" => {
                f.record.state = pvisor::RunRecordState::Running;
                f.record.finished_at_unix_ms = None;
            }
            "unknown" => f.record.state = pvisor::RunRecordState::Unknown,
            "discarded" => {
                f.record.overlay.as_mut().unwrap().state =
                    pvisor_core::overlay::OverlayState::Discarded
            }
            "active" => {
                f.record.overlay.as_mut().unwrap().state =
                    pvisor_core::overlay::OverlayState::Active
            }
            "applied" => {
                f.record.overlay.as_mut().unwrap().state =
                    pvisor_core::overlay::OverlayState::Applied
            }
            "no-workspace" => f.record.workspace = None,
            "root-workspace" => f.record.workspace = Some("/".into()),
            "target" => f.record.workspace = Some(f.root.path().to_path_buf()),
            _ => {}
        }
        f.record.write().unwrap();
        if case == "bundle-spec" {
            let path = RunBundle::path(&f.stage);
            let mut bundle: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            bundle["spec"] = json!({"metadata":{"pvisor.workspace":f.stage}});
            fs::write(path, serde_json::to_vec(&bundle).unwrap()).unwrap();
        }
        assert!(f.export(&[], true).is_err(), "{case}");
        f.no_output();
    }
}

#[test]
fn cancellation_cleans_pending_output_and_stage_is_retryable() {
    let f = Fixture::new(false);
    fs::write(f.upper.join("file"), b"artifact").unwrap();
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let cancel = || {
        if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 3 {
            anyhow::bail!("cancel export");
        }
        Ok(())
    };
    let context = ServiceContext {
        check_cancelled: Some(&cancel),
        ..Default::default()
    };
    assert!(RuntimeJobService::export(&context, f.request(&[], true)).is_err());
    f.no_output();
    assert!(
        f.export(&[], true)
            .unwrap()
            .destination
            .join("files/file")
            .exists()
    );
}

#[test]
fn committed_failed_cancelled_results_export_but_missing_or_changed_receipts_do_not() {
    for (state, exit_code, partial) in [
        ("completed", Some(0), false),
        ("completed", Some(17), true),
        ("completed", None, false),
        ("failed", Some(1), true),
        ("cancelled", None, true),
    ] {
        let mut f = Fixture::new(true);
        fs::write(f.upper.join("file"), b"retained result").unwrap();
        f.completion(state, exit_code);
        let receipt_path = f.stage.join("completion.json");
        let receipt = fs::read(&receipt_path).unwrap();
        fs::remove_file(&receipt_path).unwrap();
        assert!(f.export(&[], true).is_err(), "missing {state}");
        f.no_output();
        fs::write(&receipt_path, &receipt).unwrap();
        let bundle_path = RunBundle::path(&f.stage);
        let bundle = fs::read(&bundle_path).unwrap();
        let mut corrupt = bundle.clone();
        corrupt.push(b' ');
        fs::write(&bundle_path, corrupt).unwrap();
        assert!(f.export(&[], true).is_err(), "changed {state}");
        f.no_output();
        fs::write(&bundle_path, bundle).unwrap();
        let response = f.export(&[], true).unwrap();
        assert!(response.destination.join("files/file").is_file());
        assert_eq!(response.schema_version, 1);
        assert_eq!(response.attempt_id.as_deref(), Some("handoff-attempt"));
        assert_eq!(serde_json::to_value(response.state).unwrap(), state);
        assert_eq!(response.exit_code, exit_code);
        assert_eq!(response.partial_results, partial);
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(response.destination.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest, serde_json::to_value(&response).unwrap());
    }
}

#[test]
fn output_races_never_replace_a_winner_or_write_into_rebound_parent() {
    for race in ["winner", "parent"] {
        let f = Fixture::new(false);
        fs::write(f.upper.join("file"), b"artifact").unwrap();
        let parent = f.root.path().join("output-parent");
        let moved = f.root.path().join("original-parent");
        fs::create_dir(&parent).unwrap();
        let output = parent.join("export");
        let mut request = f.request(&[], true);
        request.output = output.clone();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let callback = || {
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 3 {
                if race == "winner" {
                    fs::create_dir(&output)?;
                    fs::write(output.join("sentinel"), b"winner")?;
                } else {
                    fs::rename(&parent, &moved)?;
                    fs::create_dir(&parent)?;
                    fs::write(parent.join("sentinel"), b"replacement")?;
                }
            }
            Ok(())
        };
        let context = ServiceContext {
            check_cancelled: Some(&callback),
            ..Default::default()
        };
        assert!(RuntimeJobService::export(&context, request).is_err());
        if race == "winner" {
            assert_eq!(fs::read(output.join("sentinel")).unwrap(), b"winner");
            assert!(!output.join("files").exists());
            assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
        } else {
            assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"replacement");
            assert!(!output.exists());
            assert_eq!(fs::read_dir(&moved).unwrap().count(), 0);
        }
        assert!(f.upper.join("file").exists());
    }
}

#[test]
fn private_backing_never_falls_back_to_packaging_permissions() {
    for identity in [
        None,
        Some(b"malformed".as_slice()),
        Some(b"1:2:120777".as_slice()),
    ] {
        let f = Fixture::new(false);
        fs::write(f.upper.join("file"), b"artifact").unwrap();
        xattr(&f.upper, pvisor_overlay_core::PRIVATE_BACKING_XATTR, b"1");
        if let Some(identity) = identity {
            xattr(
                &f.upper.join("file"),
                pvisor_overlay_core::OVERRIDE_STAT_XATTR,
                identity,
            );
        }
        assert!(f.export(&[], true).is_err());
        f.no_output();
    }
}

#[test]
fn existing_job_and_attempt_fences_are_not_bypassed() {
    use std::os::fd::AsRawFd;
    let f = Fixture::new(false);
    fs::write(f.upper.join("file"), b"artifact").unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(f.stage.join("job.lock"))
        .unwrap();
    fs::set_permissions(f.stage.join("job.lock"), fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert!(f.export(&[], true).is_err());
    f.no_output();
    drop(lock);
    let (_, lease) = f.record.lock_current().unwrap();
    assert!(f.export(&[], true).is_err());
    f.no_output();
    drop(lease);
    f.export(&[], true).unwrap();
}

// Lease/Job lock files are retained coordination artifacts, not stage contents.
fn stage_contents(stage: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, path: &Path, files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path == root.join("job.lock") || path == root.join("lease.lock") {
                continue;
            }
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                files.insert(relative, Vec::new());
                collect(root, &path, files);
            } else {
                assert!(metadata.is_file());
                files.insert(relative, fs::read(path).unwrap());
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    collect(stage, stage, &mut files);
    files
}

#[test]
fn workspace_and_ancestor_symlink_swaps_cannot_relocate_project_scope() {
    for rootfs in [true, false] {
        for ancestor in [false, true] {
            let mut f = Fixture::new(rootfs);
            let parent = f.root.path().join("projects");
            fs::create_dir(&parent).unwrap();
            let project = parent.join("project");
            fs::rename(&f.project, &project).unwrap();
            f.project = project.clone();
            f.record.workspace = Some(project.clone());
            if rootfs {
                f.upper = f
                    .stage
                    .join("upper")
                    .join(project.strip_prefix("/").unwrap());
                fs::create_dir_all(&f.upper).unwrap();
            } else {
                f.record.overlay.as_mut().unwrap().target = project.clone();
            }
            fs::write(f.upper.join("artifact"), b"project artifact").unwrap();
            f.record.write().unwrap();
            f.bundle();
            // Preserve the identical historical spelling: canonicalizing both
            // copies would incorrectly make the post-run alias agree.
            let bundle_path = RunBundle::path(&f.stage);
            let mut bundle: serde_json::Value =
                serde_json::from_slice(&fs::read(&bundle_path).unwrap()).unwrap();
            bundle["spec"] = json!({"metadata":{"pvisor.workspace":project}});
            fs::write(&bundle_path, serde_json::to_vec(&bundle).unwrap()).unwrap();
            let other = f.root.path().join("environment");
            let other_project = if ancestor {
                other.join("project")
            } else {
                other.clone()
            };
            fs::create_dir_all(&other_project).unwrap();
            fs::write(other_project.join("secret"), b"environment file").unwrap();
            if rootfs {
                let environment_upper = f
                    .stage
                    .join("upper")
                    .join(other_project.strip_prefix("/").unwrap());
                fs::create_dir_all(&environment_upper).unwrap();
                fs::write(environment_upper.join("secret"), b"staged environment file").unwrap();
            }
            let swapped = if ancestor { parent } else { project };
            fs::rename(&swapped, f.root.path().join("original")).unwrap();
            symlink(&other, &swapped).unwrap();
            let before = stage_contents(&f.stage);
            assert!(
                f.export(&[], true).is_err(),
                "rootfs={rootfs} ancestor={ancestor}"
            );
            f.no_output();
            assert_eq!(stage_contents(&f.stage), before);
            assert_eq!(
                fs::read(other_project.join("secret")).unwrap(),
                b"environment file"
            );
        }
    }
}

#[test]
fn lexical_scope_rejects_traversal_and_target_or_historical_aliases() {
    for case in [
        "workspace-dot",
        "workspace-parent",
        "history-alias",
        "target-alias",
    ] {
        let mut f = Fixture::new(false);
        fs::write(f.upper.join("file"), b"artifact").unwrap();
        let alias = f.root.path().join("alias");
        symlink(&f.project, &alias).unwrap();
        match case {
            "workspace-dot" => f.record.workspace = Some(f.project.join(".")),
            "workspace-parent" => f.record.workspace = Some(f.project.join("../project")),
            "target-alias" => f.record.overlay.as_mut().unwrap().target = alias.clone(),
            _ => {}
        }
        f.record.write().unwrap();
        if case == "history-alias" {
            let path = RunBundle::path(&f.stage);
            let mut bundle: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            bundle["spec"] = json!({"metadata":{"pvisor.workspace":alias}});
            fs::write(&path, serde_json::to_vec(&bundle).unwrap()).unwrap();
        }
        let before = stage_contents(&f.stage);
        assert!(f.export(&[], true).is_err(), "{case}");
        f.no_output();
        assert_eq!(stage_contents(&f.stage), before);
    }
}

#[test]
fn no_selected_project_inventory_never_publishes_environment_or_empty_success() {
    for case in [
        "missing-prefix",
        "empty-prefix",
        "unmatched-selection",
        "ordinary-empty",
    ] {
        let f = Fixture::new(case != "ordinary-empty");
        if case == "missing-prefix" {
            fs::remove_dir(&f.upper).unwrap();
        }
        if case == "unmatched-selection" {
            fs::write(f.upper.join("other"), b"not selected").unwrap();
        }
        fs::create_dir_all(f.stage.join("upper/etc")).unwrap();
        fs::write(f.stage.join("upper/etc/environment"), b"environment only").unwrap();
        let before = stage_contents(&f.stage);
        let error = if case == "unmatched-selection" || case == "ordinary-empty" {
            f.export(&["missing"], false).unwrap_err()
        } else {
            f.export(&[], true).unwrap_err()
        };
        assert!(
            error.to_string().contains("no selected project changes"),
            "{case}: {error:#}"
        );
        f.no_output();
        assert_eq!(stage_contents(&f.stage), before);
    }
}

#[test]
fn all_and_paths_conflict_before_resolving_a_job() {
    let f = Fixture::new(false);
    let before = stage_contents(&f.stage);
    let mut request = f.request(&["valid"], true);
    request.job.selector = Some(f.root.path().join("nonexistent-job"));
    let error = RuntimeJobService::export(&ServiceContext::default(), request).unwrap_err();
    assert!(error.to_string().contains("conflicts"), "{error:#}");
    f.no_output();
    assert_eq!(stage_contents(&f.stage), before);
}
