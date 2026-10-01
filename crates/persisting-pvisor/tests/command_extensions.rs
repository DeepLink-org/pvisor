//! Discovery reads manifests without running executables; dispatch preserves argv and exit.
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn bundled_commands_embed_manifests_and_dispatch_help() {
    for (name, binary) in [
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
