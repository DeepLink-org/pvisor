//! Review presentation contracts, using persisted fixtures without a workload or model account.
#![cfg(unix)]

use pvisor::{RunBundle, RunRecord};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

struct Fixture {
    root: tempfile::TempDir,
    record: RunRecord,
    bundle: Value,
}

impl Fixture {
    fn new(workspace: Option<&Path>, target: &Path) -> Self {
        let root = tempfile::tempdir().unwrap();
        let stage = root.path().join("stage");
        fs::create_dir_all(stage.join("upper")).unwrap();
        let record: RunRecord = serde_json::from_value(json!({
            "schema_version": 1, "run_id": "review-fixture", "session_id": "review-session",
            "attempt_id": "review-attempt", "agent": "fixture-agent", "pid": 0,
            "command": ["/bin/true"], "state": "completed", "started_at_unix_ms": 10,
            "finished_at_unix_ms": 20, "storage": stage, "workspace": workspace,
            "network": {}, "gateway_listen": null, "overlay": null
        }))
        .unwrap();
        let mut bundle: Value = serde_json::from_str(include_str!(
            "../../pvisor/tests/fixtures/bundles/v1-minimal.json"
        ))
        .unwrap();
        bundle["schema_version"] = json!(4);
        bundle["safety"]["filesystem_changes_staged"] = json!(true);
        bundle["run"]["run_id"] = json!(record.run_id);
        bundle["agentctl"]["run_id"] = json!(record.run_id);
        bundle["executor_observations"] =
            serde_json::to_value(pvisor_core::ExecutorObservations::default()).unwrap();
        bundle["filesystem"] = json!({
            "state": "staged", "target": target, "upper": stage.join("upper"),
            "changed_files": 0, "whiteouts": 0, "changes": [], "sample_paths": []
        });
        Self {
            root,
            record,
            bundle,
        }
    }

    fn save(&self) {
        self.record.write().unwrap();
        let mut value = self.bundle.clone();
        value["safety"]["filesystem_changes_staged"] = json!(!value["filesystem"].is_null());
        let bundle: RunBundle = serde_json::from_value(value).unwrap();
        bundle.write(&self.record.stage_dir()).unwrap();
    }

    fn review(&self, args: &[&str]) -> String {
        self.save();
        let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .arg("review")
            .arg(self.record.stage_dir())
            .args(args)
            .env("PVISOR_RUN_HOME", self.root.path().join("runs"))
            .env("HOME", self.root.path())
            .current_dir(self.root.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.review(&["--json"])).unwrap()
    }

    fn changes(&mut self, paths: &[&str]) {
        self.bundle["filesystem"]["changes"] = Value::Array(
            paths
                .iter()
                .map(|path| {
                    json!({
                        "path": path, "kind": "added", "new_type": "file", "mode": 420
                    })
                })
                .collect(),
        );
        self.bundle["filesystem"]["changed_files"] = json!(paths.len());
    }

    fn enable_overlay(&mut self) {
        let stage = self.record.stage_dir();
        self.record.overlay = Some(serde_json::from_value(json!({
            "id": self.record.run_id, "generation": 1, "target": self.bundle["filesystem"]["target"],
            "upper": {"upper_dir": stage.join("upper"), "work_dir": stage.join("work")},
            "merged_dir": stage.join("merged"), "stage_dir": stage,
            "auto_apply": false, "state": "staged"
        })).unwrap());
    }
}

#[test]
fn root_stage_partitions_by_components_and_preserves_every_bundle_field() {
    let mut fixture = Fixture::new(Some(Path::new("/home/agent/project")), Path::new("/"));
    fixture.changes(&[
        "home/agent/project/src/main.rs",
        "home/agent/project/target/generated",
        "home/agent/project/.cache/build",
        "home/agent/project-other/not-project",
        "etc/config",
        "var/lib/packages",
    ]);
    let reviewed = fixture.json();
    let original: RunBundle = serde_json::from_value(fixture.bundle.clone()).unwrap();
    for (key, value) in serde_json::to_value(original).unwrap().as_object().unwrap() {
        assert_eq!(&reviewed[key], value, "changed Bundle field {key}");
    }
    assert_eq!(reviewed["handoff"]["schema_version"], 1);
    assert_eq!(reviewed["schema_version"], 4);
    assert_eq!(reviewed["handoff"]["project_root"], "/home/agent/project");
    assert_eq!(reviewed["handoff"]["project_prefix"], "home/agent/project");
    assert_eq!(reviewed["handoff"]["counts"]["project"], 3);
    assert_eq!(reviewed["handoff"]["counts"]["environment"], 3);
    let text = fixture.review(&[]);
    let headings = [
        "outcome:",
        "Project results",
        "Experimental environment changes",
        "Safety boundary",
        "Next:",
    ];
    for pair in headings.windows(2) {
        assert!(
            text.find(pair[0]).unwrap() < text.find(pair[1]).unwrap(),
            "{text}"
        );
    }
    assert!(
        !text.contains("etc/config"),
        "environment manifest leaked into default text"
    );
    assert!(text.contains("not a safety certification"));
    assert!(!text.contains("sudo"));
}

#[test]
fn missing_root_relative_or_mismatched_workspace_is_unavailable_not_empty() {
    for workspace in [
        None,
        Some(Path::new("/")),
        Some(Path::new("relative/project")),
        Some(Path::new("/home/../project")),
    ] {
        let mut fixture = Fixture::new(workspace, Path::new("/"));
        fixture.changes(&["etc/config"]);
        let reviewed = fixture.json();
        assert_eq!(reviewed["handoff"]["classification_available"], false);
        assert_eq!(reviewed["handoff"]["counts"]["project"], Value::Null);
        assert_eq!(reviewed["handoff"]["counts"]["unclassified"], 1);
        assert_eq!(reviewed["handoff"]["non_root_export_available"], false);
    }
    let fixture = Fixture::new(Some(Path::new("/project")), Path::new("/other"));
    assert_eq!(fixture.json()["handoff"]["counts"]["project"], Value::Null);
}

#[test]
fn incomplete_manifest_and_absent_filesystem_are_not_known_empty() {
    let mut fixture = Fixture::new(Some(Path::new("/project")), Path::new("/"));
    fixture.bundle["filesystem"]["changed_files"] = json!(2);
    assert_eq!(fixture.json()["handoff"]["classification_available"], false);
    fixture.bundle["filesystem"] = Value::Null;
    assert_eq!(fixture.json()["handoff"]["counts"]["project"], Value::Null);
}

#[test]
fn workspace_symlink_is_not_resolved_into_stage_control_paths() {
    let mut fixture = Fixture::new(None, Path::new("/"));
    let alias = fixture.root.path().join("project-alias");
    std::os::unix::fs::symlink(fixture.record.stage_dir().join("upper"), &alias).unwrap();
    fixture.record.workspace = Some(alias.clone());
    let backing = fixture.record.stage_dir().join("upper/control");
    fixture.changes(&[backing.strip_prefix("/").unwrap().to_str().unwrap()]);
    let reviewed = fixture.json();
    assert_eq!(reviewed["handoff"]["project_root"], json!(alias));
    assert_eq!(reviewed["handoff"]["counts"]["project"], 0);
    assert_eq!(reviewed["handoff"]["counts"]["environment"], 1);
}

#[test]
fn unsupported_entries_and_partial_outcomes_are_explicit() {
    let mut fixture = Fixture::new(Some(Path::new("/project")), Path::new("/"));
    fixture.changes(&[
        "project/link",
        "project/device",
        "project/replace",
        "project/opaque",
        "project/report",
    ]);
    fixture.bundle["filesystem"]["changes"][0]["new_type"] = json!("symlink");
    fixture.bundle["filesystem"]["changes"][1]["new_type"] = json!("other");
    fixture.bundle["filesystem"]["changes"][2]["kind"] = json!("type_changed");
    fixture.bundle["filesystem"]["changes"][3]["kind"] = json!("opaque");
    assert_eq!(
        fixture.json()["handoff"]["counts"]["unsupported_project"],
        4
    );
    for state in ["failed", "cancelled"] {
        fixture.bundle["run"]["state"] = json!(state);
        assert_eq!(fixture.json()["handoff"]["partial_results"], true);
        assert!(fixture.review(&[]).contains("partial results may remain"));
    }
}

#[test]
fn root_stage_diff_contains_only_project_content_and_handoff_matches_platform() {
    let mut fixture = Fixture::new(
        Some(Path::new("/pvisor-review-fixture-project")),
        Path::new("/"),
    );
    let upper = fixture.record.stage_dir().join("upper");
    fs::create_dir_all(upper.join("pvisor-review-fixture-project/src")).unwrap();
    fs::create_dir_all(upper.join("pvisor-review-fixture-environment")).unwrap();
    fs::write(
        upper.join("pvisor-review-fixture-project/src/result"),
        "PROJECT_RESULT_MARKER\n",
    )
    .unwrap();
    fs::write(
        upper.join("pvisor-review-fixture-environment/package"),
        "ENVIRONMENT_MARKER\n",
    )
    .unwrap();
    fixture.enable_overlay();
    let text = fixture.review(&["--diff"]);
    assert!(text.contains("+PROJECT_RESULT_MARKER"), "{text}");
    assert!(!text.contains("ENVIRONMENT_MARKER"));
    let export_available = cfg!(target_os = "linux");
    assert_eq!(
        text.contains("--output NEWDIR --path src"),
        export_available,
        "{text}"
    );
    assert_eq!(text.contains("pvisor export"), export_available, "{text}");
    assert_eq!(
        text.contains("Project handoff unavailable"),
        !export_available,
        "{text}"
    );
    assert!(!text.contains("pvisor apply"), "{text}");
    let reviewed = fixture.json();
    assert_eq!(
        reviewed["handoff"]["non_root_export_available"],
        export_available
    );
    assert!(
        reviewed["filesystem"]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "pvisor-review-fixture-environment/package")
    );
}

#[test]
fn root_stage_recorded_apply_scope_does_not_override_platform_support() {
    let mut fixture = Fixture::new(None, Path::new("/"));
    let workspace = fixture.root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    fixture.record.workspace = Some(workspace.clone());
    fixture.record.project_apply_scope =
        Some(pvisor::job_service::ProjectApplyScope::capture(&workspace).unwrap());
    fixture.bundle["project_apply_scope"] =
        serde_json::to_value(&fixture.record.project_apply_scope).unwrap();
    let relative = workspace.strip_prefix("/").unwrap().join("result");
    fixture.changes(&[relative.to_str().unwrap()]);

    // This tests advisory guidance, not authorization: apply still revalidates
    // the launch contract, completion binding and source before writeback.
    let text = fixture.review(&[]);
    let apply_available = cfg!(target_os = "linux");
    assert_eq!(text.contains("pvisor apply"), apply_available, "{text}");
    assert_eq!(
        text.contains("--path <project-relative-path> --dry-run"),
        apply_available,
        "{text}"
    );
    assert_eq!(
        text.contains("interactive project picker"),
        apply_available,
        "{text}"
    );
    assert!(!text.contains("pvisor export"), "{text}");
    assert!(text.contains("pvisor inspect"), "{text}");
    assert!(text.contains("pvisor drop"), "{text}");
}

#[test]
fn ordinary_workspace_keeps_all_changes_and_project_diff() {
    let mut fixture = Fixture::new(None, Path::new("/unused"));
    let workspace = fixture.root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    fixture.record.workspace = Some(workspace.clone());
    fixture.bundle["filesystem"]["target"] = json!(workspace);
    let upper = fixture.record.stage_dir().join("upper");
    fs::write(upper.join("result"), "ORDINARY_PROJECT\n").unwrap();
    fixture.enable_overlay();
    let reviewed = fixture.json();
    assert_eq!(reviewed["handoff"]["counts"]["project"], 1);
    assert_eq!(reviewed["handoff"]["counts"]["environment"], 0);
    let text = fixture.review(&["--diff"]);
    assert!(text.contains("+ORDINARY_PROJECT"));
    assert!(text.contains("pvisor apply"));
}

#[test]
fn known_empty_is_distinct_from_unavailable_and_control_paths_are_not_results() {
    let mut fixture = Fixture::new(Some(Path::new("/project")), Path::new("/"));
    let reviewed = fixture.json();
    assert_eq!(reviewed["handoff"]["classification_available"], true);
    assert_eq!(reviewed["handoff"]["counts"]["project"], 0);
    fixture.record.workspace = Some(fixture.root.path().to_path_buf());
    let control = fixture.record.stage_dir().join("upper/control");
    let project = fixture.root.path().join("src/result");
    fixture.changes(&[
        control.strip_prefix("/").unwrap().to_str().unwrap(),
        project.strip_prefix("/").unwrap().to_str().unwrap(),
    ]);
    let reviewed = fixture.json();
    assert_eq!(reviewed["handoff"]["counts"]["project"], 1);
    assert_eq!(reviewed["handoff"]["counts"]["environment"], 1);
    fixture.record.workspace = Some(fixture.record.stage_dir().join("upper"));
    assert_eq!(fixture.json()["handoff"]["classification_available"], false);
}

#[test]
fn opaque_ancestor_and_invalid_paths_are_not_exportable_project_results() {
    let mut fixture = Fixture::new(Some(Path::new("/home/project")), Path::new("/"));
    fixture.changes(&[
        "home",
        "home/project/result",
        "../home/project/escape",
        "/home/project/absolute",
    ]);
    fixture.bundle["filesystem"]["changes"][0]["kind"] = json!("opaque");
    let reviewed = fixture.json();
    assert_eq!(reviewed["handoff"]["counts"]["project"], 1);
    assert_eq!(reviewed["handoff"]["counts"]["unsupported_project"], 1);
    assert_eq!(reviewed["handoff"]["counts"]["unclassified"], 2);
    assert_eq!(reviewed["handoff"]["non_root_export_available"], false);
}

#[test]
fn unknown_completion_retains_original_response_without_handoff_promise() {
    let mut fixture = Fixture::new(Some(Path::new("/project")), Path::new("/"));
    fixture.record.state = pvisor::RunRecordState::Unknown;
    let reviewed = fixture.json();
    assert_eq!(reviewed["state"], "unknown");
    assert_eq!(reviewed["code"], "COMPLETION_UNCOMMITTED");
    assert_eq!(reviewed["stage_retained"], true);
    assert_eq!(reviewed["workspace_mutation_allowed"], false);
    assert!(reviewed.get("handoff").is_none());
    let text = fixture.review(&[]);
    assert!(text.contains("UNKNOWN"));
    assert!(!text.contains("pvisor export"));
    assert!(!text.contains("0 project changes"));
}
