//! First-party discovery is inert; dispatch preserves argv and exit.
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn discovery_is_inert_and_dispatch_preserves_arguments_and_exit() {
    let temporary = tempfile::tempdir().unwrap();
    let kernel = temporary.path().join("pvisor");
    fs::copy(env!("CARGO_BIN_EXE_pvisor"), &kernel).unwrap();
    let marker = temporary.path().join("executed");
    let plugin = temporary.path().join("pvisor-tui");
    fs::write(
        &plugin,
        format!(
            "#!/bin/sh\n: > '{}'\nprintf '%s\\n' \"$@\"\nexit 42\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&plugin, fs::Permissions::from_mode(0o755)).unwrap();
    let list = Command::new(&kernel)
        .arg("extensions")
        .env("PATH", temporary.path())
        .output()
        .unwrap();
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let entries: Vec<serde_json::Value> = serde_json::from_slice(&list.stdout).unwrap();
    assert!(entries.iter().any(|entry| entry["name"] == "tui"));
    assert!(!marker.exists(), "discovery executed the extension");
    let output = Command::new(&kernel)
        .args(["tui", "space argument", "--literal", ""])
        .env("PATH", format!("{}:/bin", temporary.path().display()))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(42));
    assert_eq!(output.stdout, b"space argument\n--literal\n\n");
    assert!(marker.exists());
}

#[test]
fn kernel_help_discovers_commands_and_default_execution_dispatches_run() {
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(!help.contains("\n  env "));
    assert!(help.contains("execution kernel"));
    let removed = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .arg("env")
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!removed.status.success());
    assert!(
        String::from_utf8_lossy(&removed.stderr).contains("pvisor-env extension is not installed")
    );
    for name in [
        "run", "service", "apply", "drop", "status", "kill", "fork", "inspect",
    ] {
        assert!(help.contains(&format!("  {name} ")), "{help}");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["--", "/bin/sh", "-c", "printf kernel-dispatched"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("kernel-dispatched"));
}

#[test]
fn service_tools_are_nested_and_retired_commands_never_execute_a_workload() {
    let temporary = tempfile::tempdir().unwrap();
    let kernel = temporary.path().join("pvisor");
    fs::copy(env!("CARGO_BIN_EXE_pvisor"), &kernel).unwrap();
    let marker = temporary.path().join("executed");
    for tool in ["cluster", "worker", "cache", "memory-pool"] {
        let companion = temporary.path().join(format!("pvisor-{tool}"));
        fs::write(
            &companion,
            format!(
                "#!/bin/sh\n: > '{}'\nprintf '%s\\n' \"$@\"\nexit 42\n",
                marker.display()
            ),
        )
        .unwrap();
        fs::set_permissions(companion, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let help = Command::new(&kernel).arg("--help").output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    for name in ["cluster", "worker", "cache", "memory-pool", "snapshot"] {
        assert!(!help.contains(&format!("\n  {name} ")), "{help}");
        let output = Command::new(&kernel)
            .args([name, "--help"])
            .env("PATH", temporary.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("was removed"));
        assert!(!marker.exists(), "retired command was executed");
        let output = Command::new(&kernel).args(["help", name]).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
    let list = Command::new(&kernel).arg("extensions").output().unwrap();
    assert!(list.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&list.stdout).unwrap(),
        serde_json::json!([])
    );
    assert!(!marker.exists(), "discovery executed a service companion");
    let help = Command::new(&kernel)
        .args(["service", "--help"])
        .output()
        .unwrap();
    assert!(
        help.status.success(),
        "{}",
        String::from_utf8_lossy(&help.stderr)
    );
    let help = String::from_utf8_lossy(&help.stdout);
    for name in [
        "run",
        "status",
        "restart",
        "stop",
        "cluster",
        "worker",
        "cache",
        "memory-pool",
    ] {
        assert!(help.contains(&format!("  {name} ")), "{help}");
    }
    assert!(!help.contains("  node "));
    for tool in ["cluster", "worker", "cache", "memory-pool"] {
        let output = Command::new(&kernel)
            .args(["service", tool, "space argument", "--literal", ""])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(42));
        assert_eq!(output.stdout, b"space argument\n--literal\n\n");
        let output = Command::new(&kernel)
            .args(["service", tool, "--help"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(42));
        assert_eq!(output.stdout, b"--help\n");
        let output = Command::new(&kernel)
            .args(["help", "service", tool, "submit"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(42));
        assert_eq!(output.stdout, b"submit\n--help\n");
    }
    // A retired name is still a valid workload when explicitly placed after --.
    let output = Command::new(&kernel)
        .args(["--", "/bin/sh", "-c", "printf explicit-workload"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("explicit-workload"));
}

#[test]
fn isolated_core_keeps_job_commands_and_runs_without_extensions() {
    let temporary = tempfile::tempdir().unwrap();
    let kernel = temporary.path().join("pvisor");
    fs::copy(env!("CARGO_BIN_EXE_pvisor"), &kernel).unwrap();
    let output = Command::new(&kernel)
        .env("PATH", temporary.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    for name in [
        "run",
        "status",
        "kill",
        "inspect",
        "fork",
        "apply",
        "drop",
        "extensions",
        "help",
    ] {
        assert!(help.contains(&format!("\n  {name} ")), "{help}");
    }
    for name in ["env", "cache", "tui", "replay"] {
        assert!(!help.contains(&format!("\n  {name} ")), "{help}");
    }
    let output = Command::new(&kernel)
        .args(["run", "--", "/bin/sh", "-c", "printf standalone-kernel"])
        .env("PATH", temporary.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("standalone-kernel"));
    let marker = temporary.path().join("shadowed");
    let plugin = temporary.path().join("pvisor-status");
    fs::write(
        &plugin,
        format!("#!/bin/sh\n: > '{}'\nexit 42\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&plugin, fs::Permissions::from_mode(0o755)).unwrap();
    let output = Command::new(&kernel)
        .args(["status", "--help"])
        .env("PATH", temporary.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!marker.exists(), "extension shadowed a core command");
}

#[test]
fn path_cannot_supply_companions_and_unknown_commands_are_not_discovered() {
    let installation = tempfile::tempdir().unwrap();
    let untrusted = tempfile::tempdir().unwrap();
    let kernel = installation.path().join("pvisor");
    fs::copy(env!("CARGO_BIN_EXE_pvisor"), &kernel).unwrap();
    for name in ["tui", "probe"] {
        let path = untrusted.path().join(format!("pvisor-{name}"));
        fs::write(&path, "#!/bin/sh\nexit 42\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let output = Command::new(&kernel)
            .args(["help", name])
            .env("PATH", untrusted.path())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("extension is not installed"));
    }
    fs::copy(
        untrusted.path().join("pvisor-probe"),
        installation.path().join("pvisor-probe"),
    )
    .unwrap();
    let output = Command::new(&kernel).arg("extensions").output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!([])
    );
}
