#![cfg(target_os = "linux")]

use pvisor::RunBundle;
use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn checked(output: Output) -> Output {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn json(output: Output) -> serde_json::Value {
    serde_json::from_slice(&checked(output).stdout).unwrap()
}

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap()
}

/// A frontend export error must not turn a committed safe execution into an
/// unknown/failed execution, rerun it, or consume its staged proposal.
#[test]
fn safe_result_export_collision_retains_committed_stage_and_allows_successor() {
    let first = std::env::var("PVISOR_CHAOS_FIRST_SEED")
        .map(|value| value.parse::<u64>().unwrap())
        .unwrap_or(20261112);
    let count = std::env::var("PVISOR_CHAOS_SEED_COUNT")
        .map(|value| value.parse::<u64>().unwrap())
        .unwrap_or(4);
    assert!((1..=256).contains(&count));
    for seed in first..first.checked_add(count).unwrap() {
        let fixture = tempfile::Builder::new().prefix("r").tempdir().unwrap();
        // Keep failed cases for diagnosis, including timeout/owner evidence.
        // The outer, owned systemd cgroup is the failure cleanup boundary.
        // Delete only successful cases after checking owner/socket/mount state.
        let fixture_path = fixture.keep();
        let root = fixture_path.as_path();
        eprintln!("seed={seed} fixture={}", root.display());
        let workspace = root.join("w");
        let home = root.join("h");
        let runs = root.join("r");
        let stage = root.join("s");
        let successor = root.join("n");
        let exports = root.join("e");
        for directory in [&workspace, &home, &exports] {
            fs::create_dir(directory).unwrap();
        }
        fs::write(workspace.join("sentinel"), b"host-unchanged").unwrap();
        fs::write(workspace.join("original"), b"lower").unwrap();
        let command = || {
            let mut command = Command::new("timeout");
            command
                .args(["--kill-after=2s", "25s", env!("CARGO_BIN_EXE_pvisor")])
                .current_dir(&workspace)
                .env("HOME", &home)
                .env("XDG_CONFIG_HOME", home.join("config"))
                .env("XDG_CACHE_HOME", home.join("cache"))
                .env("XDG_STATE_HOME", home.join("state"))
                .env("PVISOR_RUN_HOME", &runs)
                .env("TMPDIR", root)
                .stdin(Stdio::null());
            command
        };
        let destination = if seed % 2 == 0 {
            let path = exports.join("result");
            fs::create_dir(&path).unwrap();
            fs::write(path.join("obstruction"), b"owned-obstruction").unwrap();
            path
        } else {
            fs::write(exports.join("blocked"), b"owned-obstruction").unwrap();
            exports.join("blocked/result")
        };
        // Different seeds vary payload, chunk count, obstruction and terminal
        // outcome. Rename/write ordering is fixed. The oracle is stage-independent.
        let chunks = seed % 7 + 1;
        let exit = [0, 7, 23][(seed % 3) as usize];
        let payload = format!("seed-{seed}-");
        let expected = payload.repeat(chunks as usize);
        let script = format!(
            "set -eu; printf once >> effects; \
             i=0; while test \"$i\" -lt {chunks}; do printf '{payload}' >> temporary; i=$((i+1)); done; \
             mv temporary accepted; printf scratch > scratch; \
             mv original renamed; printf changed > renamed; exit {exit}"
        );
        let output = command()
            .args([
                "spawn",
                "--no-agent-defaults",
                "--safe",
                "--overlaynet-deny-all",
                "--stdio",
                "capture",
                "--stage",
            ])
            .arg(&stage)
            .arg("--result-file")
            .arg(&destination)
            .args(["--", "/bin/sh", "-c", &script])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("RunResult"),
            "{output:?}"
        );
        let bundle = RunBundle::read(&stage).unwrap();
        assert_eq!(bundle.run.exit_code, Some(exit));
        assert!(bundle.safety.safe_profile_requested);
        assert!(bundle.safety.filesystem_non_bypassable);
        assert!(bundle.safety.filesystem_changes_staged);
        let upper = &bundle.filesystem.as_ref().unwrap().upper;
        assert_eq!(read(&upper.join("accepted")), expected.as_bytes());
        assert_eq!(read(&upper.join("effects")), b"once");
        assert_eq!(read(&upper.join("renamed")), b"changed");
        assert!(!workspace.join("accepted").exists());
        assert!(!workspace.join("effects").exists());
        assert!(!workspace.join("renamed").exists());
        assert_eq!(read(&workspace.join("original")), b"lower");
        let receipt = read(&stage.join("completion.json"));
        let receipt_json: serde_json::Value = serde_json::from_slice(&receipt).unwrap();
        let committed: pvisor_core::RunResult =
            serde_json::from_value(receipt_json["result"].clone()).unwrap();
        assert_eq!(committed.run_id.as_str(), bundle.run.run_id);
        assert_eq!(committed.execution_id.as_str(), bundle.run.execution_id);
        assert_eq!(committed.state, bundle.run.state);
        assert_eq!(committed.exit_code, Some(exit));
        let bundle_bytes = read(&stage.join("run-bundle.json"));
        let status = json(
            command()
                .arg("status")
                .arg(&stage)
                .arg("--json")
                .output()
                .unwrap(),
        );
        assert_eq!(status["live"], false);
        assert_eq!(
            status["state"],
            if exit == 0 { "completed" } else { "failed" }
        );
        assert_eq!(status["run"]["overlay"]["state"], "staged");
        let owner = status["run"]["pid"].as_u64().unwrap();
        assert!(!Path::new(&format!("/proc/{owner}")).exists());
        let review = checked(
            command()
                .arg("review")
                .arg(&stage)
                .arg("--json")
                .output()
                .unwrap(),
        );
        assert!(String::from_utf8_lossy(&review.stdout).contains("accepted"));
        assert_eq!(read(&stage.join("completion.json")), receipt);
        assert_eq!(read(&stage.join("run-bundle.json")), bundle_bytes);
        assert_eq!(read(&upper.join("effects")), b"once");
        // Remove only our known obstruction; do not sweep the export directory.
        if seed % 2 == 0 {
            assert_eq!(read(&destination.join("obstruction")), b"owned-obstruction");
            fs::remove_file(destination.join("obstruction")).unwrap();
            fs::remove_dir(&destination).unwrap();
        } else {
            assert_eq!(read(&exports.join("blocked")), b"owned-obstruction");
            fs::remove_file(exports.join("blocked")).unwrap();
        }
        assert!(fs::read_dir(&exports).unwrap().next().is_none());
        checked(
            command()
                .arg("apply")
                .arg(&stage)
                .args(["--path", "accepted"])
                .output()
                .unwrap(),
        );
        assert_eq!(read(&workspace.join("accepted")), expected.as_bytes());
        assert!(!workspace.join("effects").exists());
        assert!(!workspace.join("scratch").exists());
        assert!(!workspace.join("renamed").exists());
        assert_eq!(read(&upper.join("effects")), b"once");
        assert_eq!(read(&upper.join("scratch")), b"scratch");
        assert_eq!(read(&upper.join("renamed")), b"changed");
        checked(command().arg("drop").arg(&stage).output().unwrap());
        let status = json(
            command()
                .arg("status")
                .arg(&stage)
                .arg("--json")
                .output()
                .unwrap(),
        );
        assert_eq!(status["filesystem"]["changed_files"], 0);
        assert_eq!(status["run"]["overlay"]["state"], "discarded");
        assert_eq!(read(&stage.join("completion.json")), receipt);
        assert_eq!(read(&stage.join("run-bundle.json")), bundle_bytes);
        // Recovery is a fresh execution, never a hidden retry of the first.
        checked(
            command()
                .args([
                    "spawn",
                    "--no-agent-defaults",
                    "--safe",
                    "--overlaynet-deny-all",
                    "--stdio",
                    "capture",
                    "--stage",
                ])
                .arg(&successor)
                .arg("--result-file")
                .arg(&destination)
                .args(["--", "/bin/sh", "-c", "printf healthy > health"])
                .output()
                .unwrap(),
        );
        let result: pvisor_core::RunResult = serde_json::from_slice(&read(&destination)).unwrap();
        assert_eq!(result.exit_code, Some(0));
        let next_bundle = RunBundle::read(&successor).unwrap();
        assert_ne!(next_bundle.run.run_id, bundle.run.run_id);
        assert_eq!(result.run_id.as_str(), next_bundle.run.run_id);
        assert_eq!(
            read(
                &next_bundle
                    .filesystem
                    .as_ref()
                    .unwrap()
                    .upper
                    .join("health")
            ),
            b"healthy"
        );
        checked(command().arg("drop").arg(&successor).output().unwrap());
        let next_status = json(
            command()
                .arg("status")
                .arg(&successor)
                .arg("--json")
                .output()
                .unwrap(),
        );
        assert_eq!(next_status["filesystem"]["changed_files"], 0);
        assert_eq!(next_status["live"], false);
        let next_owner = next_status["run"]["pid"].as_u64().unwrap();
        assert!(!Path::new(&format!("/proc/{next_owner}")).exists());
        assert_eq!(read(&workspace.join("sentinel")), b"host-unchanged");
        assert_eq!(read(&workspace.join("original")), b"lower");
        assert_eq!(read(&workspace.join("accepted")), expected.as_bytes());
        assert!(!workspace.join("health").exists());
        assert!(!stage.join("control.sock").exists());
        assert!(!successor.join("control.sock").exists());
        assert!(
            !fs::read_to_string("/proc/self/mountinfo")
                .unwrap()
                .contains(root.to_str().unwrap())
        );
        eprintln!(
            "seed={seed} chunks={chunks} guest_exit={exit} frontend=1 recovery=0 stages=2 commands=9 owners_gone=true self_namespace_mounts=0"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
