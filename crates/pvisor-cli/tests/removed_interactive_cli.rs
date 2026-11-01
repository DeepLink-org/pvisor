//! Removed approval/UI entrypoints fail before executing a workload.
use std::{fs, process::Command};

fn command(root: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor"));
    command
        .current_dir(root)
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("PVISOR_RUN_HOME", root.join("runs"));
    command
}

#[test]
fn removed_flags_and_ask_access_fail_before_workload_launch() {
    for args in [
        vec!["spawn", "--ask"],
        vec!["spawn", "--tui"],
        vec!["spawn", "--access", ".env:ask"],
        vec!["--ask"],
        vec!["--tui"],
    ] {
        let temp = tempfile::tempdir().unwrap();
        let output = command(temp.path())
            .args(args)
            .args(["--", "/bin/sh", "-c", "touch launched"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("unexpected argument") || error.contains("ask file rules"),
            "{error}"
        );
        assert!(!temp.path().join("launched").exists());
        assert!(!temp.path().join("runs").exists());
    }
}

#[test]
fn legacy_ask_config_is_rejected_without_rewriting_policy() {
    for source in [
        "[[overlayfs.access]]\npath = '.env'\nlevel = 'ask'\n",
        "[policies.session.filesystem]\nask = ['.env']\n",
        "[policies.workspace.filesystem]\nask = ['.env']\n",
        "[policies.user.filesystem]\nask = ['.env']\n",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("run.toml");
        fs::write(&config, source).unwrap();
        let output = command(temp.path())
            .args(["spawn", "--config"])
            .arg(&config)
            .args(["--clear-access", "--", "/bin/sh", "-c", "touch launched"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("ask file policies are no longer supported"),
            "{error}"
        );
        assert!(!temp.path().join("launched").exists());
        assert_eq!(fs::read_to_string(config).unwrap(), source);
    }
}

#[test]
fn user_and_workspace_ask_defaults_are_rejected_before_vm_preparation() {
    for relative in [".pvisor/policy.toml", "config/pvisor/policy.toml"] {
        let temp = tempfile::tempdir().unwrap();
        let policy = temp.path().join(relative);
        fs::create_dir_all(policy.parent().unwrap()).unwrap();
        fs::write(&policy, "[filesystem]\nask = ['.env']\n").unwrap();
        let output = command(temp.path())
            .args([
                "spawn",
                "--no-agent-defaults",
                "--executor",
                "vm",
                "--",
                "/bin/sh",
                "-c",
                "touch launched",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("ask file policies are no longer supported"),
            "{error}"
        );
        assert!(!temp.path().join("launched").exists());
    }
}

#[test]
fn prepared_json_ask_policy_is_rejected_before_stage_creation() {
    let temp = tempfile::tempdir().unwrap();
    let mut spec = pvisor_core::RunSpec::process("legacy-ask", "agent", "/bin/sh");
    spec.policies.session.filesystem = Some(
        pvisor_core::FileAccessPolicy::new_with_ask(vec![], vec![".env".into()], vec![]).unwrap(),
    );
    let path = temp.path().join("spec.json");
    fs::write(&path, serde_json::to_vec(&spec).unwrap()).unwrap();
    let stage = temp.path().join("stage");
    let result = temp.path().join("result.json");
    let output = command(temp.path())
        .args(["spawn", "--spec"])
        .arg(path)
        .arg("--result-file")
        .arg(&result)
        .arg("--stage")
        .arg(&stage)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("ask file policies are no longer supported"),
        "{error}"
    );
    assert!(!stage.exists());
    assert!(!result.exists());
}
