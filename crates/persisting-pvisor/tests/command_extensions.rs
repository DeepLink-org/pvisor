//! Discovery reads manifests without running executables; dispatch preserves argv and exit.
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn bundled_commands_embed_manifests_and_dispatch_help() {
    for (name, binary) in [
        ("cache", env!("CARGO_BIN_EXE_pvisor-cache")),
        ("tui", env!("CARGO_BIN_EXE_pvisor-tui")),
        ("replay", env!("CARGO_BIN_EXE_pvisor-replay")),
    ] {
        let bytes = fs::read(binary).unwrap();
        let manifest = persisting_pvisor::cli::extensions::embedded_manifest(&bytes).unwrap();
        assert_eq!(manifest.name, name);
        let output = Command::new(binary)
            .arg("--pvisor-manifest")
            .output()
            .unwrap();
        assert!(output.status.success());
        let queried: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(queried["name"], name);
        let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .args([name, "--help"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains(&format!("pvisor-{name}")));
    }
}

#[test]
fn discovery_is_inert_and_dispatch_preserves_arguments_and_exit() {
    let temporary = tempfile::tempdir().unwrap();
    let marker = temporary.path().join("executed");
    let plugin = temporary.path().join("pvisor-probe");
    let manifest = persisting_pvisor::command_manifest!("probe", "Inert discovery probe");
    fs::write(
        &plugin,
        format!(
            "#!/bin/sh\n: > '{}'\nprintf '%s\\n' \"$@\"\nexit 42\n{manifest}",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&plugin, fs::Permissions::from_mode(0o755)).unwrap();
    let list = Command::new(env!("CARGO_BIN_EXE_pvisor"))
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
    assert!(
        entries
            .iter()
            .any(|entry| entry["manifest"]["name"] == "probe")
    );
    assert!(!marker.exists(), "discovery executed the extension");
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args(["probe", "space argument", "--literal", ""])
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
        "run", "cache", "apply", "drop", "status", "kill", "fork", "inspect", "tui", "replay",
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
    let manifest = persisting_pvisor::command_manifest!("status", "Should not override the core");
    let plugin = temporary.path().join("pvisor-status");
    fs::write(
        &plugin,
        format!("#!/bin/sh\n: > '{}'\nexit 42\n{manifest}", marker.display()),
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
