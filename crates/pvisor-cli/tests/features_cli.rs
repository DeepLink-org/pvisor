//! Feature discovery is frontend-local, independent of the persistent Job service.
use std::process::Command;

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn listing_synonyms_json_and_explicit_enable_are_local() {
    for args in [&["feature"][..], &["feature", "list"][..]] {
        let output = cli(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("name\tstage\tdefault\tenabled\tdescription"));
        assert!(
            text.contains(
                "workload-aware-memory-offloading\texperimental\tfalse\tfalse\tEXP-001 M0"
            )
        );
    }
    for (args, enabled) in [
        (vec!["feature", "--json"], false),
        (vec!["feature", "list", "--json"], false),
        (
            vec![
                "--feature",
                "workload-aware-memory-offloading",
                "feature",
                "--json",
            ],
            true,
        ),
        (
            vec![
                "feature",
                "list",
                "--feature",
                "workload-aware-memory-offloading",
                "--json",
            ],
            true,
        ),
    ] {
        let output = cli(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["name"], "workload-aware-memory-offloading");
        assert_eq!(rows[0]["stage"], "experimental");
        assert_eq!(rows[0]["default"], false);
        assert_eq!(rows[0]["enabled"], enabled);
        assert!(
            rows[0]["description"]
                .as_str()
                .unwrap()
                .contains("EXP-001 M0")
        );
    }
}

#[test]
fn unknown_features_and_help_never_need_a_job_service() {
    for args in [
        vec!["--feature", "unknown", "feature"],
        vec!["run", "--feature", "unknown", "--", "true"],
    ] {
        let output = cli(&args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("unknown feature 'unknown'"));
    }
    for args in [
        vec![
            "--feature",
            "workload-aware-memory-offloading",
            "help",
            "run",
        ],
        vec![
            "--feature",
            "workload-aware-memory-offloading",
            "run",
            "--help",
        ],
        vec!["--feature", "workload-aware-memory-offloading", "--help"],
        vec!["help", "feature"],
    ] {
        let output = cli(&args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("--feature"));
    }
}

#[test]
fn old_feature_name_is_rejected_without_an_alias() {
    for args in [
        vec!["--feature", "vm-vcpu-observe", "feature"],
        vec!["feature", "list", "--feature=vm-vcpu-observe"],
        vec!["run", "--feature", "vm-vcpu-observe", "--", "true"],
        vec!["--feature=vm-vcpu-observe", "--", "true"],
    ] {
        let output = cli(&args);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("unknown feature 'vm-vcpu-observe'")
        );
    }
}

#[cfg(unix)]
#[test]
fn companion_help_and_literal_args_keep_forwarding_semantics() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let temp = tempfile::tempdir().unwrap();
    let kernel = temp.path().join("pvisor");
    fs::copy(env!("CARGO_BIN_EXE_pvisor"), &kernel).unwrap();
    let companion = temp.path().join("pvisor-tui");
    fs::write(&companion, "#!/bin/sh\nprintf '%s\\n' \"$@\"\nexit 42\n").unwrap();
    fs::set_permissions(&companion, fs::Permissions::from_mode(0o755)).unwrap();
    for args in [
        vec![
            "--feature",
            "workload-aware-memory-offloading",
            "help",
            "tui",
        ],
        vec![
            "--feature",
            "workload-aware-memory-offloading",
            "tui",
            "--help",
        ],
    ] {
        let output = Command::new(&kernel).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(42));
        assert_eq!(output.stdout, b"--help\n");
    }
    let output = Command::new(&kernel)
        .args(["tui", "--", "--feature", "guest-only"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(42));
    assert_eq!(output.stdout, b"--\n--feature\nguest-only\n");
    let output = Command::new(&kernel)
        .args(["--feature", "--feature", "help", "tui"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_ne!(output.status.code(), Some(42));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown feature '--feature'"));
    use std::os::unix::ffi::OsStringExt;
    let output = Command::new(&kernel)
        .arg("--feature")
        .arg(std::ffi::OsString::from_vec(b"invalid-\xff".to_vec()))
        .args(["help", "tui"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_ne!(output.status.code(), Some(42));
    assert!(String::from_utf8_lossy(&output.stderr).contains("feature name must be UTF-8"));
}
