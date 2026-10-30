//! External handoff contracts; no VM, FUSE, model account or privileged apply.
#![cfg(unix)]

use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(args)
        .current_dir(root)
        .env("HOME", root)
        .env("PVISOR_RUN_HOME", root.join("runs"))
        .env("PVISOR_DOCKER_HOME", root.join("docker"))
        .env_remove("PVISOR_STARTUP_TIMING")
        .env_remove("PVISOR_STARTUP_PROFILE")
        .output()
        .unwrap()
}

fn help(root: &Path, args: &[&str]) -> String {
    let output = cli(root, args);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn export_help_is_grouped_and_inert_and_stage_explains_both_network_choices() {
    let root = tempfile::tempdir().unwrap();
    let text = help(root.path(), &["--help"]);
    let (containers, rest) = text.split_once("Agent Commands:").unwrap();
    assert!(
        !containers
            .lines()
            .any(|l| l.split_whitespace().next() == Some("export"))
    );
    let agents = rest.split("Tools:").next().unwrap();
    let names: Vec<_> = agents
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    let review = names.iter().position(|n| *n == "review").unwrap();
    assert_eq!(&names[review..review + 3], &["review", "export", "apply"]);
    let direct = help(root.path(), &["export", "--help"]);
    assert_eq!(direct, help(root.path(), &["help", "export"]));
    for text in [
        "[JOB]",
        "--output",
        "--storage",
        "--path",
        "--all",
        "--json",
        "manifest.json",
        "Linux",
        "deletions",
        "last",
        "Symlinks",
    ] {
        assert!(direct.contains(text), "missing {text}: {direct}");
    }
    assert!(!direct.contains("-o,"));
    let stage = help(root.path(), &["stage", "--help"]);
    for text in [
        "coding agent stagebox",
        "Networked Agent:",
        "Offline script:",
        "--network bridge",
        "--network none",
        "credentials",
        "approval settings",
        "--cpus",
        "--memory",
        "Advanced only:",
        "non-root",
    ] {
        assert!(stage.contains(text), "missing {text}: {stage}");
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn clap_requires_destination_and_exclusive_explicit_selection_before_io() {
    let root = tempfile::tempdir().unwrap();
    for args in [
        vec!["export"],
        vec!["export", "--all"],
        vec!["export", "--output", "out"],
        vec!["export", "--output", "out", "--all", "--path", "src"],
        vec!["export", "--output", "out", "--path"],
        vec!["export", "--output", "out", "--all", "-o", "store"],
    ] {
        let output = cli(root.path(), &args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("error:"));
        assert!(output.stdout.is_empty());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn run_and_spawn_help_remain_byte_identical_to_pre_handoff_baseline() {
    let root = tempfile::tempdir().unwrap();
    use sha2::{Digest, Sha256};
    for (command, digest) in [
        (
            "run",
            "adcaad687de5ad492bbdaecc1dabc7024c77a379a20762ef543c51f8d487cc32",
        ),
        (
            "spawn",
            "06abd21de89c4a0ab939603e8b2a10ef0bbb56adcc53b203b090612928908659",
        ),
    ] {
        let text = help(root.path(), &[command, "--help"]);
        assert_eq!(
            Sha256::digest(text.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            digest,
            "{command} help changed"
        );
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use pvisor::{RunBundle, RunRecord};

    // Minimal persisted fixture following runtime_export.rs; no owner or workload.
    fn fixture(root: &Path, rootfs: bool) -> (RunRecord, std::path::PathBuf) {
        fixture_with_outcome(root, rootfs, "completed", Some(0))
    }

    fn fixture_with_outcome(
        root: &Path,
        rootfs: bool,
        state: &str,
        exit_code: Option<i32>,
    ) -> (RunRecord, std::path::PathBuf) {
        let project = root.join("project");
        let stage = root.join("stage");
        fs::create_dir(&project).unwrap();
        for dir in ["upper", "work", "merged"] {
            fs::create_dir_all(stage.join(dir)).unwrap();
        }
        let upper = if rootfs {
            stage.join("upper").join(project.strip_prefix("/").unwrap())
        } else {
            stage.join("upper")
        };
        fs::create_dir_all(upper.join("src")).unwrap();
        fs::write(upper.join("src/result"), "artifact").unwrap();
        fs::write(upper.join("unselected"), "dependency").unwrap();
        fs::write(project.join("old"), "keep").unwrap();
        fs::write(upper.join(".wh.old"), "").unwrap();
        let record: RunRecord = serde_json::from_value(json!({
            "schema_version":1,"run_id":"handoff-job","session_id":"handoff-session","attempt_id":"handoff-attempt",
            "agent":"test","pid":0,"command":["/bin/true"],"state":state,
            "started_at_unix_ms":10,"finished_at_unix_ms":20,"storage":stage,"workspace":project,"network":{},
            "overlay":{"id":"handoff-job","generation":7,"target":if rootfs { Path::new("/") } else { &project },
                "upper":{"upper_dir":stage.join("upper"),"work_dir":stage.join("work")},
                "merged_dir":stage.join("merged"),"stage_dir":stage,"auto_apply":false,"state":"staged"}
        })).unwrap();
        record.write().unwrap();
        let result = serde_json::from_value(json!({
            "run_id":"handoff-job","attempt_id":"handoff-attempt","state":state,
            "started_at_unix_ms":10,"finished_at_unix_ms":20,"exit_code":exit_code
        }))
        .unwrap();
        let agentctl = serde_json::from_value(json!({"run_id":"handoff-job","attempt_id":"handoff-attempt","directive":{"kind":"continue"},"clients":[]})).unwrap();
        RunBundle::capture(&record, &result, agentctl, false)
            .unwrap()
            .write(&stage)
            .unwrap();
        (record, upper)
    }

    #[test]
    fn exports_relative_and_absolute_outputs_without_consuming_or_applying() {
        for rootfs in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (record, upper) = fixture(root.path(), rootfs);
            let stage = record.stage_dir();
            let before = fs::read(stage.join("run.json")).unwrap();
            let output = cli(
                root.path(),
                &[
                    "export",
                    stage.to_str().unwrap(),
                    "--output",
                    "out",
                    "--path",
                    "src",
                    "--path",
                    "old",
                    "--storage",
                    "unused",
                    "--json",
                ],
            );
            assert!(output.status.success(), "{output:?}");
            assert!(output.stderr.is_empty(), "{output:?}");
            let response: Value = serde_json::from_slice(&output.stdout).unwrap();
            let manifest: Value =
                serde_json::from_slice(&fs::read(root.path().join("out/manifest.json")).unwrap())
                    .unwrap();
            assert_eq!(response, manifest);
            assert_eq!(response["job_id"], "handoff-job");
            assert_eq!(response["deletions"][0]["path"], "old");
            assert_eq!(
                fs::read_to_string(root.path().join("out/files/src/result")).unwrap(),
                "artifact"
            );
            assert!(!root.path().join("out/files/unselected").exists());
            assert!(!root.path().join("out/files/old").exists());
            let destination = root.path().join("second");
            let output = cli(
                root.path(),
                &[
                    "export",
                    stage.to_str().unwrap(),
                    "--output",
                    destination.to_str().unwrap(),
                    "--all",
                ],
            );
            assert!(output.status.success(), "{output:?}");
            let text = String::from_utf8(output.stdout).unwrap();
            for expected in [
                "manifest.json",
                "deletions",
                "not a complete patch",
                "Nothing was automatically applied",
                "environment changes were not accepted",
            ] {
                assert!(text.contains(expected), "{text}");
            }
            assert!(destination.join("files/unselected").exists());
            assert_eq!(before, fs::read(stage.join("run.json")).unwrap());
            assert!(upper.join("src/result").exists());
            assert!(
                !record
                    .workspace
                    .as_ref()
                    .unwrap()
                    .join("src/result")
                    .exists()
            );
            assert_eq!(
                fs::read_to_string(record.workspace.unwrap().join("old")).unwrap(),
                "keep"
            );
        }
    }

    #[test]
    fn task_outcomes_are_explicit_in_human_output_and_json_matches_manifest() {
        for (state, exit_code, partial, human_state) in [
            ("failed", Some(1), true, "Failed"),
            ("cancelled", None, true, "Cancelled"),
            ("completed", Some(7), true, "Completed"),
            ("completed", Some(0), false, "Completed"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let (mut record, _) = fixture_with_outcome(root.path(), false, state, exit_code);
            let stage = record.stage_dir();
            record.schema_version = 2;
            record.completion_protocol = Some(1);
            record.write().unwrap();
            let result = json!({
                "run_id": "handoff-job", "attempt_id": "handoff-attempt", "state": state,
                "started_at_unix_ms": 10, "finished_at_unix_ms": 20, "exit_code": exit_code
            });
            let run_result = serde_json::from_value(result.clone()).unwrap();
            let agentctl = serde_json::from_value(json!({
                "run_id": "handoff-job", "attempt_id": "handoff-attempt",
                "directive": {"kind": "continue"}, "clients": []
            }))
            .unwrap();
            RunBundle::capture(&record, &run_result, agentctl, false)
                .unwrap()
                .write(&stage)
                .unwrap();
            let bundle_bytes = fs::read(RunBundle::path(&stage)).unwrap();
            fs::write(
                stage.join("completion.json"),
                serde_json::to_vec(&json!({
                    "version": 1,
                    "result": result,
                    "bundle_blake3": blake3::hash(&bundle_bytes).to_hex().to_string()
                }))
                .unwrap(),
            )
            .unwrap();

            RunRecord::read(&stage).unwrap();
            for json_output in [false, true] {
                let destination = if json_output { "json" } else { "human" };
                let mut args = vec![
                    "export",
                    stage.to_str().unwrap(),
                    "--output",
                    destination,
                    "--all",
                ];
                if json_output {
                    args.push("--json");
                }
                let output = cli(root.path(), &args);
                assert!(output.status.success(), "{state}/{exit_code:?}: {output:?}");
                assert!(output.stderr.is_empty(), "{output:?}");
                let manifest: Value = serde_json::from_slice(
                    &fs::read(root.path().join(destination).join("manifest.json")).unwrap(),
                )
                .unwrap();
                assert_eq!(manifest["schema_version"], 1);
                assert_eq!(manifest["attempt_id"], "handoff-attempt");
                assert_eq!(manifest["state"], state);
                assert_eq!(manifest["exit_code"], json!(exit_code));
                assert_eq!(manifest["partial_results"], partial);
                if json_output {
                    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
                    assert_eq!(response, manifest);
                } else {
                    let text = String::from_utf8(output.stdout).unwrap();
                    let code =
                        exit_code.map_or_else(|| "unknown".to_owned(), |code| code.to_string());
                    assert!(
                        text.contains(&format!("outcome: {human_state}, exit code: {code}")),
                        "{text}"
                    );
                    assert_eq!(text.contains("Warning: partial results"), partial, "{text}");
                    assert_eq!(
                        text.contains("Export success does not mean task success"),
                        partial,
                        "{text}"
                    );
                    assert!(text.contains("Nothing was automatically applied"), "{text}");
                }
            }
        }
    }

    #[test]
    fn omitted_selector_and_explicit_last_use_runtime_resolution() {
        let root = tempfile::tempdir().unwrap();
        let (record, _) = fixture(root.path(), false);
        let stage = record.stage_dir();
        for (selector, destination) in [(None, "implicit"), (Some("last"), "explicit")] {
            let mut args = vec!["export"];
            args.extend(selector);
            let destination = root.path().join(destination);
            args.extend(["--output", destination.to_str().unwrap(), "--all", "--json"]);
            let output = cli(&stage, &args);
            assert!(output.status.success(), "{output:?}");
            let response: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(response["job_id"], "handoff-job");
            assert!(destination.join("manifest.json").exists());
        }
    }

    #[test]
    fn runtime_errors_never_publish_output() {
        for case in [
            "missing", "symlink", "dropped", "unknown", "absolute", "parent",
        ] {
            let root = tempfile::tempdir().unwrap();
            let (mut record, upper) = fixture(root.path(), false);
            let stage = record.stage_dir();
            let mut selection = "src";
            match case {
                "missing" => fs::remove_file(RunBundle::path(&stage)).unwrap(),
                "symlink" => {
                    fs::remove_file(upper.join("src/result")).unwrap();
                    std::os::unix::fs::symlink("/etc/passwd", upper.join("src/result")).unwrap();
                }
                "dropped" => {
                    record.overlay.as_mut().unwrap().state =
                        pvisor_core::overlay::OverlayState::Discarded;
                    record.write().unwrap();
                }
                "unknown" => {
                    record.state = pvisor::RunRecordState::Unknown;
                    record.write().unwrap();
                }
                "absolute" => selection = "/src",
                "parent" => selection = "../src",
                _ => unreachable!(),
            }
            let output = cli(
                root.path(),
                &[
                    "export",
                    stage.to_str().unwrap(),
                    "--output",
                    "out",
                    "--path",
                    selection,
                ],
            );
            assert!(!output.status.success(), "{case}: {output:?}");
            assert!(!root.path().join("out").exists());
            assert!(!fs::read_dir(root.path()).unwrap().any(|e| {
                e.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".pvisor-export-")
            }));
            assert!(upper.exists());
        }
    }
}
