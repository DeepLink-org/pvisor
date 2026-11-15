//! Standalone observer integration contracts, without running Criterion sampling.
//! Wire as [[test]] name="observer-contracts", path="src/observer_contract_tests.rs".
#![cfg(target_os = "linux")]

#[path = "startup/observer.rs"]
mod observer;

use std::{collections::BTreeMap, fs, time::Duration};

fn capture(
    directory: &std::path::Path,
    script: &str,
    timeout: Duration,
) -> anyhow::Result<observer::Observation> {
    observer::capture(
        &["/bin/sh".into(), "-c".into(), script.into()],
        directory,
        &BTreeMap::new(),
        directory,
        b"REFERENCE_READY",
        timeout,
    )
}

#[test]
fn ready_excludes_exit_delay_and_retains_original_output_and_argv() {
    let directory = tempfile::tempdir().unwrap();
    let script = "printf 'REFERENCE_READY\\n'; printf diagnostic >&2; sleep .25";
    let observed = capture(directory.path(), script, Duration::from_secs(3)).unwrap();
    assert!(observed.exit_ns - observed.ready_ns >= 200_000_000);
    assert_eq!(observed.exit_code, 0);
    assert_eq!(
        fs::read(directory.path().join("stdout")).unwrap(),
        b"REFERENCE_READY\n"
    );
    assert_eq!(
        fs::read(directory.path().join("stderr")).unwrap(),
        b"diagnostic"
    );
    let argv: Vec<String> =
        serde_json::from_slice(&fs::read(directory.path().join("argv.json")).unwrap()).unwrap();
    assert_eq!(argv, ["/bin/sh", "-c", script]);
    let timing: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.path().join("timing.json")).unwrap()).unwrap();
    assert_eq!(timing["ready_ns"], observed.ready_ns);
    assert_eq!(timing["exit_ns"], observed.exit_ns);
    assert!(!directory.path().join("observer-failure.json").exists());
}

#[test]
fn invalid_output_and_nonzero_exit_keep_diagnostics_without_success_summary() {
    for script in [
        "printf 'not ready\\n'",
        "printf 'REFERENCE_READY\\nREFERENCE_READY\\n'",
        "printf 'REFERENCE_READY suffix\\n'",
        "printf REFERENCE_READY",
        "printf 'REFERENCE_READY\\n'; printf failed >&2; exit 7",
    ] {
        let directory = tempfile::tempdir().unwrap();
        assert!(capture(directory.path(), script, Duration::from_secs(3)).is_err());
        assert!(
            !fs::read(directory.path().join("stdout"))
                .unwrap()
                .is_empty()
        );
        assert!(directory.path().join("stderr").is_file());
        assert!(directory.path().join("observer-failure.json").is_file());
        assert!(!directory.path().join("timing.json").exists());
    }
}

#[test]
fn timeout_after_ready_kills_owned_descendant_and_retains_partial_output() {
    let directory = tempfile::tempdir().unwrap();
    let result = capture(
        directory.path(),
        "sleep 30 & echo $! > child.pid; printf 'REFERENCE_READY\\n'; wait",
        Duration::from_millis(200),
    );
    assert!(result.is_err());
    let pid = fs::read_to_string(directory.path().join("child.pid")).unwrap();
    // An orphan may remain a zombie until the system reaper runs; it must not run.
    if let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", pid.trim())) {
        assert_eq!(
            stat.split_once(") ").unwrap().1.split_whitespace().next(),
            Some("Z")
        );
    }
    assert_eq!(
        fs::read(directory.path().join("stdout")).unwrap(),
        b"REFERENCE_READY\n"
    );
    let failure: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.path().join("observer-failure.json")).unwrap())
            .unwrap();
    assert_eq!(
        failure["ready_observations_ns"].as_array().unwrap().len(),
        1
    );
    assert!(!directory.path().join("timing.json").exists());
}
