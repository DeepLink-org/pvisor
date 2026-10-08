//! CLI failures retain runtime-readable review candidates.
#[cfg(target_os = "linux")]
use serde_json::Value;

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
