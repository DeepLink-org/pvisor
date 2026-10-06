#![cfg(unix)]

use std::{
    io::Write,
    process::{Command, Stdio},
};

fn command(root: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor"));
    command
        .current_dir(root)
        .env("PVISOR_RUN_HOME", root.join("runs"));
    command
}

#[test]
fn hidden_host_mode_requires_private_inherited_capability() {
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .arg("--pvisor-internal-host")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("capability"));
}

#[test]
fn removed_ctrl_is_not_a_builtin() {
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("  ctrl "));
    let root = tempfile::tempdir().unwrap();
    let tools = root.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let marker = root.path().join("accidental-agent");
    let ctrl = tools.join("ctrl");
    std::fs::write(
        &ctrl,
        format!("#!/bin/sh\nprintf invoked > '{}'\n", marker.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&ctrl, std::fs::Permissions::from_mode(0o755)).unwrap();
    for args in [
        vec!["ctrl", "--socket", "/retired.sock", "status"],
        vec!["ctrl", "--help"],
        vec!["help", "ctrl"],
    ] {
        let rejected = command(root.path())
            .env("PATH", &tools)
            .args(args)
            .output()
            .unwrap();
        assert!(!rejected.status.success());
        assert!(
            String::from_utf8_lossy(&rejected.stderr).contains("`pvisor ctrl` has been retired"),
            "{}",
            String::from_utf8_lossy(&rejected.stderr)
        );
        assert!(
            !marker.exists(),
            "retired ctrl must not be normalized to an Agent execution"
        );
        assert!(
            !root.path().join("runs").exists(),
            "retired ctrl must not admit a Job"
        );
    }
}

#[test]
fn stdin_and_large_output_use_descriptors_and_preserve_exit_code() {
    let root = tempfile::tempdir().unwrap();
    let mut child = command(root.path())
        .args([
            "run",
            "--no-agent-defaults",
            "--overlaynet",
            "off",
            "--",
            "/bin/sh",
            "-c",
            "cat; exit 23",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let bytes = vec![b'x'; 2 * 1024 * 1024];
    let mut input = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        input.write_all(&bytes).unwrap();
    });
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(23),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout.len(), 2 * 1024 * 1024);
    assert!(output.stdout.iter().all(|b| *b == b'x'));
}

#[test]
fn cancellation_is_acknowledged_with_signal_exit_status() {
    use std::io::{BufRead, BufReader};
    let root = tempfile::tempdir().unwrap();
    let mut child = command(root.path())
        .args([
            "run",
            "--no-agent-defaults",
            "--overlaynet",
            "off",
            "--",
            "/bin/sh",
            "-c",
            "printf 'READY\\n'; exec /bin/sleep 60",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line, "READY\n");
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(130),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("request cancelled"));
    let status = command(root.path())
        .args(["status", "last", "--json"])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(value["live"], false);
    assert_ne!(value["run"]["state"], "running");
}

#[test]
fn simultaneous_requests_isolate_cwd_and_environment_and_leave_stopped_jobs_addressable() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let invoke = |root: &std::path::Path, marker: &str| {
        command(root)
            .env("HOST_SERVICE_TEST_MARKER", marker)
            .args([
                "run",
                "--no-agent-defaults",
                "--overlaynet",
                "off",
                "--pass-env",
                "HOST_SERVICE_TEST_MARKER",
                "--",
                "/bin/sh",
                "-c",
                "printf '%s\\n' \"$HOST_SERVICE_TEST_MARKER\"; pwd",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let first = invoke(a.path(), "first");
    let second = invoke(b.path(), "second");
    for (child, root, marker) in [(first, a.path(), "first"), (second, b.path(), "second")] {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{marker}\n{}\n", root.canonicalize().unwrap().display())
        );
        let status = command(root)
            .args(["status", "last", "--json"])
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(value["live"], false);
        assert_eq!(value["run"]["state"], "completed");
    }
}
