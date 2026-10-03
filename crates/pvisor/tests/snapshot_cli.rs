use std::process::Command;

#[test]
fn snapshot_is_a_discoverable_builtin_without_companions() {
    let binary = env!("CARGO_BIN_EXE_pvisor");
    let root = Command::new(binary)
        .arg("--help")
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(root.status.success());
    assert!(String::from_utf8_lossy(&root.stdout).contains("  snapshot "));
    let help = Command::new(binary)
        .args(["snapshot", "--help"])
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);
    for command in ["run", "save", "restore", "list", "delete", "gc"] {
        assert!(text.contains(&format!("  {command} ")), "{text}");
    }
    assert!(!text.contains("  runner "));
    assert!(!text.contains("  ram-watchdog "));
    assert!(text.contains("fork"), "{text}");
}

#[test]
fn invalid_resources_and_conflicting_launch_modes_fail_before_side_effects() {
    let directory = tempfile::tempdir().unwrap();
    let store = directory.path().join("must-not-be-created");
    for arguments in [
        vec![
            "run", "--name", "a", "--rootfs", "/unused", "--cpus", "0", "--", "bash",
        ],
        vec![
            "run", "--name", "a", "--rootfs", "/unused", "--memory", "0", "--", "bash",
        ],
        vec!["run", "--name", "a", "--rootfs", "/unused"],
        vec![
            "run",
            "--name",
            "a",
            "--rootfs",
            "/unused",
            "--ram-storage",
            "invalid",
            "--",
            "bash",
        ],
        vec![
            "run",
            "--name",
            "a",
            "--rootfs",
            "/unused",
            "--native-init",
            "--",
            "bash",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .args(["snapshot", "--store"])
            .arg(&store)
            .args(arguments)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!store.exists());
    }
}
