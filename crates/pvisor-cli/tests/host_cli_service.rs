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
fn hidden_owner_mode_requires_private_inherited_socketpair() {
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .arg("--pvisor-internal-owner")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("private inherited socketpair"));
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

#[test]
fn recorded_owner_routes_live_commands_and_legacy_records_stay_disk_only() {
    use std::{
        io::{BufRead, BufReader},
        time::{Duration, Instant},
    };
    struct Running(std::process::Child);
    impl Drop for Running {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                unsafe {
                    libc::kill(self.0.id() as i32, libc::SIGTERM);
                }
                let until = Instant::now() + Duration::from_secs(6);
                while self.0.try_wait().ok().flatten().is_none() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(10));
                }
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
    let root = tempfile::tempdir().unwrap();
    let mut running = Running(
        command(root.path())
            .args([
                "run",
                "--no-agent-defaults",
                "--overlaynet",
                "off",
                "--",
                "/bin/sh",
                "-c",
                "printf 'READY:%s\\n' \"$PPID\"; exec /bin/sleep 60",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(running.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let owner: u32 = line.trim().strip_prefix("READY:").unwrap().parse().unwrap();
    let stage = std::fs::read_dir(root.path().join("runs"))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.join("run.json").exists())
        .unwrap();
    let record = pvisor::RunRecord::read(&stage).unwrap();
    let endpoint = record.live_control.as_ref().unwrap();
    assert_eq!(endpoint.pid, owner);
    assert_ne!(endpoint.pid, running.0.id());
    assert_eq!(unsafe { libc::getpgid(owner as i32) }, owner as i32);
    assert_eq!(unsafe { libc::getsid(owner as i32) }, unsafe {
        libc::getsid(running.0.id() as i32)
    });
    assert!(
        endpoint.path.ends_with(
            std::path::Path::new(&record.run_id)
                .join(record.attempt_id.as_ref().unwrap())
                .join("control.sock")
        )
    );
    let status = command(root.path())
        .args(["status", "last", "--json"])
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&status.stdout).unwrap()["live"],
        true
    );
    for operation in ["apply", "drop"] {
        let refused = command(root.path())
            .args([operation, "last"])
            .output()
            .unwrap();
        assert!(!refused.status.success());
        assert!(String::from_utf8_lossy(&refused.stderr).contains("owner still holds stage"));
    }
    let mut incompatible = record.clone();
    incompatible.live_control.as_mut().unwrap().protocol = "future-instance-2".into();
    incompatible.write().unwrap();
    let refused = command(root.path())
        .args(["status", "last", "--json"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let error = String::from_utf8_lossy(&refused.stderr);
    assert!(
        error.contains("future-instance-2")
            && error.contains(&endpoint.binary.display().to_string()),
        "{error}"
    );
    let mut legacy = record.clone();
    legacy.live_control = None;
    legacy.write().unwrap();
    let disk = command(root.path())
        .args(["status", "last", "--json"])
        .output()
        .unwrap();
    assert!(
        disk.status.success(),
        "{}",
        String::from_utf8_lossy(&disk.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&disk.stdout).unwrap()["live"],
        false
    );
    assert!(
        !command(root.path())
            .args(["kill", "last"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(running.0.try_wait().unwrap().is_none());
    record.write().unwrap();
    let killed = command(root.path())
        .args(["kill", "last", "--json"])
        .output()
        .unwrap();
    assert!(
        killed.status.success(),
        "{}",
        String::from_utf8_lossy(&killed.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&killed.stdout).unwrap()["cooperative"],
        true
    );
    let until = Instant::now() + Duration::from_secs(8);
    while running.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < until, "owner failed to finish after kill");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        pvisor::RunRecord::read(&stage)
            .unwrap()
            .live_control
            .is_none()
    );
    assert!(!endpoint.path.exists());
    assert!(
        command(root.path())
            .args(["status", "last", "--json"])
            .output()
            .unwrap()
            .status
            .success()
    );
}
