//! Real, normalized documentation samples remain readable by the product reader.
use pvisor::{RUN_BUNDLE_FILENAME, RunBundle};
use serde_json::Value;

fn sample() -> Value {
    serde_json::from_str(include_str!(
        "../../../docs/overrides/assets/examples/json/run-bundle.json"
    ))
    .unwrap()
}

fn read(value: &Value) -> anyhow::Result<RunBundle> {
    let directory = tempfile::tempdir()?;
    std::fs::write(
        directory.path().join(RUN_BUNDLE_FILENAME),
        serde_json::to_vec(value)?,
    )?;
    RunBundle::read(directory.path())
}

#[test]
fn real_review_sample_is_readable_and_distinguishes_deletion_from_success() {
    let bundle = read(&sample()).expect("documented schema-4 review output");
    assert_eq!(bundle.run.exit_code, Some(0));
    assert!(bundle.safety.filesystem_read_non_bypassable);
    assert!(bundle.safety.filesystem_write_non_bypassable);
    assert!(bundle.safety.network_non_bypassable);
    let changes = &bundle.filesystem.unwrap().changes;
    assert!(
        changes
            .iter()
            .any(|entry| entry.path == "obsolete.txt" && entry.kind == pvisor::ChangeKind::Deleted)
    );
    assert!(
        changes
            .iter()
            .any(|entry| entry.path == "src/result.txt" && entry.kind == pvisor::ChangeKind::Added)
    );
}

#[test]
fn documented_reader_rejects_unknown_versions_and_missing_receipts() {
    let mut value = sample();
    value["schema_version"] = Value::from(99);
    assert!(
        read(&value)
            .unwrap_err()
            .to_string()
            .contains("unsupported Run Bundle schema")
    );
    let mut value = sample();
    value
        .as_object_mut()
        .unwrap()
        .remove("executor_observations");
    assert!(
        read(&value)
            .unwrap_err()
            .to_string()
            .contains("executor_observations")
    );
}

#[test]
fn documented_omission_is_different_from_an_observed_zero() {
    let value = sample();
    assert_eq!(value["network"]["intercepted"]["requests_seen"], 0);
    let mut without_observations = value.clone();
    without_observations["network"]
        .as_object_mut()
        .unwrap()
        .remove("intercepted");
    assert!(
        read(&without_observations)
            .unwrap()
            .network
            .intercepted
            .is_none()
    );
    assert_eq!(value["run"]["exit_code"], 0);
    let status: Value = serde_json::from_str(include_str!(
        "../../../docs/overrides/assets/examples/json/status.json"
    ))
    .unwrap();
    assert!(status.get("schema_version").is_none());
    assert_eq!(status["observations"]["network"]["requests_seen"], 0);
    let checkpoint: Value = serde_json::from_str(include_str!(
        "../../../docs/overrides/assets/examples/json/checkpoint-list.json"
    ))
    .unwrap();
    assert_eq!(checkpoint["schema_version"], 1);
    assert_eq!(checkpoint["operation"], "checkpoint.list");
    assert_eq!(checkpoint["checkpoints"], serde_json::json!([]));
}

#[test]
#[cfg(target_os = "linux")]
fn documented_ci_failure_and_timeout_keep_reviewable_candidates() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    for (name, shell, timeout, expected_code, expected_kind) in [
        (
            "failure",
            "printf 'candidate\\n' > candidate.txt; exit 7",
            None,
            7,
            "process_exit",
        ),
        (
            "timeout",
            "printf 'candidate\\n' > candidate.txt; sleep 2",
            Some("100ms"),
            1,
            "deadline_exceeded",
        ),
    ] {
        let stage = root.path().join(name);
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_pvisor"));
        command
            .current_dir(&workspace)
            .env("PVISOR_RUN_HOME", root.path().join("runs"))
            .args([
                "run",
                "--safe",
                "--overlaynet-deny-all",
                "--stdio",
                "capture",
                "--stage",
            ])
            .arg(&stage);
        if let Some(timeout) = timeout {
            command.args(["--timeout", timeout]);
        }
        let output = command
            .args(["--", "/bin/sh", "-c", shell])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected_code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let review = std::process::Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .current_dir(&workspace)
            .args(["status", "--review", "--json"])
            .arg(&stage)
            .output()
            .unwrap();
        assert!(
            review.status.success(),
            "{}",
            String::from_utf8_lossy(&review.stderr)
        );
        let bundle: Value = serde_json::from_slice(&review.stdout).unwrap();
        assert_eq!(bundle["run"]["state"], "failed");
        assert_eq!(bundle["run"]["failure"]["kind"], expected_kind);
        assert!(
            bundle["filesystem"]["changes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|change| change["path"] == "candidate.txt")
        );
        assert!(
            !workspace.join("candidate.txt").exists(),
            "candidate stays staged"
        );
    }
}
