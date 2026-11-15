#![cfg(target_os = "linux")]

use pvisor::RunBundle;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

fn command(root: &Path, workspace: &Path) -> Command {
    let mut command = Command::new("timeout");
    command
        .args(["--kill-after=2s", "25s", env!("CARGO_BIN_EXE_pvisor")])
        .current_dir(workspace)
        .env("HOME", root.join("h"))
        .env("XDG_CONFIG_HOME", root.join("h/config"))
        .env("XDG_CACHE_HOME", root.join("h/cache"))
        .env("XDG_STATE_HOME", root.join("h/state"))
        .env("PVISOR_RUN_HOME", root.join("runs"))
        .env("TMPDIR", root)
        .stdin(Stdio::null());
    command
}

fn spawn(root: &Path, destination: &Path, prepared: bool) -> Output {
    let workspace = root.join("w");
    let mut command = command(root, &workspace);
    command.arg("spawn");
    if prepared {
        command.arg("--spec").arg(root.join("spec.json"));
    } else {
        command.args([
            "--no-agent-defaults",
            "--safe",
            "--overlaynet-deny-all",
            "--stdio",
            "capture",
        ]);
    }
    command
        .arg("--stage")
        .arg(root.join("stage"))
        .arg("--result-file")
        .arg(destination);
    if !prepared {
        command.args([
            "--",
            "/bin/sh",
            "-c",
            "printf once >> effects; printf changed > accepted",
        ]);
    }
    command.output().unwrap()
}

fn checked(output: Output) -> Output {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn fixture(prepared: bool) -> PathBuf {
    let root = tempfile::Builder::new()
        .prefix("rd")
        .tempdir()
        .unwrap()
        .keep();
    // Retain all failure evidence; only successful, stopped fixtures are removed.
    eprintln!("result destination fixture={}", root.display());
    fs::create_dir(root.join("w")).unwrap();
    fs::create_dir(root.join("h")).unwrap();
    fs::write(root.join("w/sentinel"), b"host unchanged").unwrap();
    if prepared {
        let mut spec = pvisor_core::RunSpec::process("result-destination", "test", "/bin/sh");
        let pvisor_core::RunInvocation::Process(process) = &mut spec.invocation;
        process.cwd = Some(root.join("w").to_str().unwrap().to_owned());
        process.args = vec!["-c".into(), "printf once >> effects".into()];
        fs::write(root.join("spec.json"), serde_json::to_vec(&spec).unwrap()).unwrap();
    }
    root
}

fn assert_unstarted(root: &Path, destination: &Path, prepared: bool) {
    let output = spawn(root, destination, prepared);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("--result-file must be outside Session storage"),
        "expected explicit destination refusal before execution: {output:?}"
    );
    assert_eq!(
        fs::read(root.join("w/sentinel")).unwrap(),
        b"host unchanged"
    );
    assert!(!root.join("w/effects").exists());
    assert!(!root.join("w/accepted").exists());
    // An empty storage root may be resolved during admission, but neither
    // internal artifacts nor guest effects may be published on refusal.
    let stage = root.join("stage");
    if stage.exists() {
        assert_eq!(fs::read_dir(&stage).unwrap().count(), 0);
    }
    let runs = root.join("runs");
    if runs.exists() {
        assert_eq!(fs::read_dir(runs).unwrap().count(), 0);
    }
}

fn successful_external_retry(root: &Path, destination: &Path, prepared: bool) {
    checked(spawn(root, destination, prepared));
    let wire: serde_json::Value = serde_json::from_slice(&fs::read(destination).unwrap()).unwrap();
    let exported: pvisor_core::RunResult = if prepared {
        serde_json::from_value(wire["result"].clone()).unwrap()
    } else {
        serde_json::from_value(wire).unwrap()
    };
    let stage = root.join("stage");
    let bundle = RunBundle::read(&stage).unwrap();
    assert_eq!(exported.run_id.as_str(), bundle.run.run_id);
    assert_eq!(exported.execution_id.as_str(), bundle.run.execution_id);
    assert_eq!(exported.state, pvisor_core::RunState::Completed);
    assert_eq!(exported.exit_code, Some(0));
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(stage.join("completion.json")).unwrap()).unwrap();
    let committed: pvisor_core::RunResult =
        serde_json::from_value(receipt["result"].clone()).unwrap();
    assert_eq!(committed.run_id, exported.run_id);
    assert_eq!(committed.execution_id, exported.execution_id);
    assert_eq!(committed.state, exported.state);
    let status = checked(
        command(root, &root.join("w"))
            .arg("status")
            .arg(&stage)
            .arg("--json")
            .output()
            .unwrap(),
    );
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["live"], false);
    assert_eq!(status["state"], "completed");
    let pid = status["run"]["pid"].as_u64().unwrap();
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
    checked(
        command(root, &root.join("w"))
            .arg("review")
            .arg(&stage)
            .arg("--json")
            .output()
            .unwrap(),
    );
    if prepared {
        assert_eq!(fs::read(root.join("w/effects")).unwrap(), b"once");
    } else {
        let upper = &bundle.filesystem.as_ref().unwrap().upper;
        assert_eq!(fs::read(upper.join("effects")).unwrap(), b"once");
        assert_eq!(fs::read(upper.join("accepted")).unwrap(), b"changed");
        assert!(!root.join("w/effects").exists());
        assert!(!root.join("w/accepted").exists());
        checked(
            command(root, &root.join("w"))
                .arg("drop")
                .arg(&stage)
                .output()
                .unwrap(),
        );
    }
    assert_eq!(
        fs::read(root.join("w/sentinel")).unwrap(),
        b"host unchanged"
    );
    assert!(!stage.join("control.sock").exists());
    assert!(
        !fs::read_to_string("/proc/self/mountinfo")
            .unwrap()
            .contains(root.to_str().unwrap())
    );
    fs::remove_dir_all(root).unwrap();
}

fn rejected_then_retry(artifact: &str, relative: bool) {
    let root = fixture(false);
    let destination = if relative {
        PathBuf::from("../stage/../stage").join(artifact)
    } else {
        root.join("stage").join(artifact)
    };
    assert_unstarted(&root, &destination, false);
    successful_external_retry(&root, &root.join("exports").join(artifact), false);
}

#[test]
fn rejects_absolute_bundle_export_before_execution() {
    rejected_then_retry("run-bundle.json", false);
}

#[test]
fn rejects_absolute_completion_export_before_execution() {
    rejected_then_retry("completion.json", false);
}

#[test]
fn rejects_relative_bundle_export_before_execution() {
    rejected_then_retry("run-bundle.json", true);
}

#[test]
fn rejects_relative_completion_export_before_execution() {
    rejected_then_retry("completion.json", true);
}

#[test]
fn rejects_non_artifact_exports_inside_opaque_stage_storage() {
    for path in [
        "upper/ordinary",
        "preimages/ordinary",
        "export/result.json",
        "",
    ] {
        let root = fixture(false);
        assert_unstarted(&root, &root.join("stage").join(path), false);
        successful_external_retry(&root, &root.join("external.json"), false);
    }
}

#[test]
fn allows_external_same_names_and_stage2_component_boundary() {
    for name in ["run-bundle.json", "completion.json"] {
        let root = fixture(false);
        // Both the existing stage's ".." and a sibling named stage2 are valid.
        let destination = root.join("stage/../stage2").join(name);
        successful_external_retry(&root, &destination, false);
    }
}

#[test]
fn prepared_spec_rejects_internal_export_then_reuses_the_same_stage() {
    for path in ["completion.json", "exports/result.json"] {
        let root = fixture(true);
        assert_unstarted(&root, &root.join("stage").join(path), true);
        successful_external_retry(&root, &root.join("delegated.json"), true);
    }
}
