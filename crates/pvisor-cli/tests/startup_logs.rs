//! Routine diagnostics must be correlated and stay outside workload stdout.
use std::process::Command;

#[test]
fn informational_commands_do_not_emit_execution_diagnostics() {
    for args in [
        vec![],
        vec!["--help"],
        vec!["-h"],
        vec!["help"],
        vec!["help", "status"],
        vec!["spawn", "--help"],
        vec!["--version"],
        vec!["feature", "--json"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .args(&args)
            .env("PVISOR_STARTUP_TIMING", "1")
            .env("PVISOR_STARTUP_PROFILE", "1")
            .env_remove("PVISOR_DIAGNOSTICS_FD")
            .env_remove("PVISOR_UI_CHILD")
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {:?}", output.stderr);
        assert!(output.stderr.is_empty(), "{args:?}: {:?}", output.stderr);
    }
}

#[test]
fn startup_logs_are_opt_in_correlated_and_route_to_frontend() {
    let temp = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor"));
    command
        .args([
            "spawn",
            "--no-agent-defaults",
            "--overlaynet",
            "off",
            "--stdio",
            "inherit",
            "--",
            "/bin/sh",
            "-c",
            "printf 'WORKLOAD_READY\\n'",
        ])
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env("PVISOR_RUN_HOME", temp.path().join("runs"))
        .env_remove("PVISOR_STARTUP_TIMING")
        .env_remove("PVISOR_STARTUP_PROFILE")
        .env_remove("PVISOR_DIAGNOSTICS_FD")
        .env_remove("PVISOR_UI_CHILD")
        .env_remove("PVISOR_UI_STAGE_FILE")
        .env_remove("PVISOR_UI_LOG_FILE");
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"WORKLOAD_READY\n");
    let log = String::from_utf8(output.stderr).unwrap();
    for prefix in [
        "pvisor-startup ",
        "pvisor-persistence ",
        "pvisor-owner-request ",
    ] {
        assert!(!log.contains(prefix), "unexpected diagnostic: {log}");
    }
    for message in [
        "workspace writes are direct",
        "boundary:",
        "Run Bundle:",
        "run-",
        "Review: pvisor status --review",
    ] {
        assert!(log.contains(message), "missing {message}: {log}");
    }

    let output = command.env("PVISOR_STARTUP_TIMING", "1").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"WORKLOAD_READY\n");
    let log = String::from_utf8(output.stderr).unwrap();
    assert!(log.contains("pvisor-startup level=info timestamp_ms="));
    assert!(log.contains("stage=process.entry"));
    assert!(log.contains("run_id=\"run-"));
    assert!(log.contains("stage=cli.run_finished"));
    assert!(!log.contains("printf"));
    let persistence: Vec<_> = log
        .lines()
        .filter(|line| line.starts_with("pvisor-persistence "))
        .collect();
    assert!(!persistence.is_empty());
    for object in ["run_record", "run_index"] {
        for phase in [
            "serialize",
            "file_write",
            "file_sync",
            "rename",
            "directory_sync",
        ] {
            assert!(
                persistence
                    .iter()
                    .any(|line| line.contains(&format!("object={object} phase={phase} ")))
            );
        }
    }
    for line in persistence {
        assert!(line.contains("run_id=\"run-"));
        assert!(line.contains("outcome=ok"));
        assert!(
            line.split_whitespace()
                .find_map(|field| field.strip_prefix("duration_us="))
                .unwrap()
                .parse::<u128>()
                .is_ok()
        );
    }

    let output = command.env("PVISOR_STARTUP_TIMING", "0").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("pvisor-startup "));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("pvisor-persistence "));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("pvisor-owner-request "));

    let path = temp.path().join("frontend.log");
    std::fs::write(&path, "").unwrap();
    let output = command
        .env("PVISOR_STARTUP_TIMING", "1")
        .env("PVISOR_UI_CHILD", "1")
        .env("PVISOR_UI_STAGE_FILE", temp.path().join("stage"))
        .env("PVISOR_UI_LOG_FILE", &path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"WORKLOAD_READY\n");
    assert!(std::fs::read_to_string(&path).unwrap().is_empty());
    assert!(!temp.path().join("stage").exists());
    let log = String::from_utf8(output.stderr).unwrap();
    assert!(log.contains("stage=process.entry"));
    assert!(log.contains("stage=cli.run_finished"));
    assert!(log.contains("pvisor-persistence "));
    // Each frontend retains its own ordinary stderr diagnostics.
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let children: Vec<_> = (0..4).map(|_| command.spawn().unwrap()).collect();
    let pids: Vec<_> = children
        .iter()
        .map(|child| child.id().to_string())
        .collect();
    let mut log = String::new();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"WORKLOAD_READY\n");
        log.push_str(&String::from_utf8(output.stderr).unwrap());
    }
    // Parsing and execution have separate PIDs. Correlate each Session owner
    // with its originating frontend.
    let bindings: Vec<_> = log
        .lines()
        .filter(|line| line.starts_with("pvisor-owner-request "))
        .collect();
    assert_eq!(bindings.len(), 4);
    let mut owner_pids = std::collections::HashSet::new();
    let mut request_ids = std::collections::HashSet::new();
    let mut frontend_pids = std::collections::HashSet::new();
    for line in bindings {
        let field = |key: &str| {
            line.split_whitespace()
                .find_map(|field| field.strip_prefix(key))
                .unwrap()
        };
        let frontend = field("frontend_pid=");
        assert!(
            pids.iter().any(|pid| pid == frontend),
            "foreign frontend: {line}"
        );
        assert!(frontend_pids.insert(frontend.to_owned()));
        assert!(owner_pids.insert(field("owner_pid=").to_owned()));
        let request: String = serde_json::from_str(field("request_id=")).unwrap();
        uuid::Uuid::parse_str(&request).unwrap();
        assert!(request_ids.insert(request));
    }
    let checkpoints: Vec<_> = log
        .lines()
        .filter(|line| line.starts_with("pvisor-startup "))
        .collect();
    assert_eq!(
        checkpoints
            .iter()
            .filter(|line| line.contains("stage=process.entry"))
            .count(),
        4
    );
    assert_eq!(
        checkpoints
            .iter()
            .filter(|line| line.contains("stage=cli.run_finished"))
            .count(),
        4
    );
    for line in checkpoints {
        let pid = line
            .split_whitespace()
            .find_map(|field| field.strip_prefix("pid="))
            .unwrap();
        assert!(
            pids.iter().any(|expected| expected == pid) || owner_pids.contains(pid),
            "interleaved record: {line}"
        );
        for key in ["stage=", "monotonic_us=", "process_elapsed_us="] {
            assert_eq!(
                line.split_whitespace()
                    .filter(|field| field.starts_with(key))
                    .count(),
                1
            );
        }
    }
}

#[test]
fn default_output_preserves_workload_errors_and_policy_refusals() {
    let temp = tempfile::tempdir().unwrap();
    for (args, expected_code, message) in [
        (
            vec![
                "--",
                "/bin/sh",
                "-c",
                "printf 'WORKLOAD_ERROR\\n' >&2; exit 7",
            ],
            Some(7),
            "WORKLOAD_ERROR",
        ),
        (
            vec![
                "--safe",
                "--filesystem",
                "host",
                "--",
                "/bin/echo",
                "SHOULD_NOT_RUN",
            ],
            None,
            "--safe requires --filesystem sandbox",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
            .args([
                "spawn",
                "--no-agent-defaults",
                "--overlaynet",
                "off",
                "--stdio",
                "inherit",
            ])
            .args(args)
            .current_dir(temp.path())
            .env("HOME", temp.path())
            .env("PVISOR_RUN_HOME", temp.path().join("runs"))
            .env_remove("PVISOR_STARTUP_TIMING")
            .env_remove("PVISOR_STARTUP_PROFILE")
            .env_remove("PVISOR_DIAGNOSTICS_FD")
            .env_remove("PVISOR_UI_CHILD")
            .env_remove("PVISOR_UI_STAGE_FILE")
            .env_remove("PVISOR_UI_LOG_FILE")
            .output()
            .unwrap();
        assert!(!output.status.success());
        if let Some(code) = expected_code {
            assert_eq!(output.status.code(), Some(code));
        }
        assert!(output.stdout.is_empty());
        let log = String::from_utf8(output.stderr).unwrap();
        assert!(log.contains(message), "missing {message}: {log}");
        for prefix in [
            "pvisor-startup ",
            "pvisor-persistence ",
            "pvisor-owner-request ",
        ] {
            assert!(!log.contains(prefix), "unexpected diagnostic: {log}");
        }
    }
}

#[test]
fn opt_in_profiles_use_diagnostics_routing_and_leave_workload_stdout_clean() {
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("profile.log");
    std::fs::write(&log, "").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_pvisor"))
        .args([
            "spawn",
            "--no-agent-defaults",
            "--overlaynet",
            "off",
            "--stdio",
            "inherit",
            "--",
            "/bin/echo",
            "READY",
        ])
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env("PVISOR_RUN_HOME", temp.path().join("runs"))
        .env("PVISOR_STARTUP_TIMING", "0")
        .env("PVISOR_STARTUP_PROFILE", "1")
        .env_remove("PVISOR_DIAGNOSTICS_FD")
        .env("PVISOR_UI_CHILD", "1")
        .env("PVISOR_UI_STAGE_FILE", temp.path().join("stage"))
        .env("PVISOR_UI_LOG_FILE", &log)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"READY\n");
    assert!(std::fs::read_to_string(log).unwrap().is_empty());
    assert!(!temp.path().join("stage").exists());
    let text = String::from_utf8(output.stderr).unwrap();
    assert!(!text.contains("pvisor-startup level="));
    let records: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|line| line.strip_prefix("pvisor-startup-profile "))
        .map(|record| serde_json::from_str(record).unwrap())
        .collect();
    for stage in [
        "cli.entry_to_parsed",
        "session.start",
        "runtime.control_executable_blake3",
        "process.prepare",
    ] {
        let record = records
            .iter()
            .find(|record| record["stage"] == stage)
            .unwrap_or_else(|| panic!("missing {stage}: {text}"));
        assert_eq!(record["schema"], 1);
        assert!(matches!(record["outcome"].as_str(), Some("ok" | "success")));
        assert_eq!(
            record["end_ns"].as_u64().unwrap() - record["start_ns"].as_u64().unwrap(),
            record["duration_ns"].as_u64().unwrap()
        );
    }
}

#[test]
#[cfg(unix)]
#[cfg_attr(target_os = "macos", ignore = "requires macFUSE")]
fn printed_stage_guidance_runs_unchanged_in_a_posix_shell() {
    fn output(command: &mut Command) -> std::process::Output {
        use std::process::Stdio;
        use std::time::{Duration, Instant};
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let captured = child.wait_with_output().unwrap();
                panic!("guidance command timed out: {:?}", captured.stderr);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.wait_with_output().unwrap()
    }

    for name in ["stage with spaces", "owner's stage", "stage$literal[1]"] {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("project");
        std::fs::create_dir(&workspace).unwrap();
        let stage = temp.path().join(name);
        let binary = std::path::Path::new(env!("CARGO_BIN_EXE_pvisor"));
        let path = std::env::join_paths(
            std::iter::once(binary.parent().unwrap().to_path_buf()).chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            )),
        )
        .unwrap();
        let command = |program| {
            let mut command = Command::new(program);
            command
                .current_dir(&workspace)
                .env("HOME", temp.path().join("home"))
                .env("XDG_CONFIG_HOME", temp.path().join("config"))
                .env("PVISOR_RUN_HOME", temp.path().join("runs"))
                .env("PATH", &path)
                .env_remove("PVISOR_DIAGNOSTICS_FD")
                .env_remove("PVISOR_UI_CHILD");
            command
        };
        let spawned = output(command(binary).args([
            "spawn",
            "--no-agent-defaults",
            "--filesystem",
            "host",
            "--overlaynet",
            "off",
            "--stage",
            stage.to_str().unwrap(),
            "--",
            "/bin/sh",
            "-c",
            "printf candidate > result.txt",
        ]));
        assert!(spawned.status.success(), "{:?}", spawned.stderr);
        let text = String::from_utf8(spawned.stderr).unwrap();
        let review = text
            .lines()
            .find_map(|line| line.strip_prefix("Review: "))
            .unwrap();
        let copied = output(command(std::path::Path::new("/bin/sh")).args(["-c", review]));
        assert!(copied.status.success(), "{name}: {:?}", copied.stderr);
        let reviewed = output(command(binary).args(["review", stage.to_str().unwrap(), "--diff"]));
        assert!(reviewed.status.success(), "{:?}", reviewed.stderr);
        let text = String::from_utf8(reviewed.stdout).unwrap();
        for prefix in ["pvisor inspect ", "pvisor drop "] {
            let instruction = text
                .lines()
                .map(str::trim_start)
                .find(|line| line.starts_with(prefix))
                .unwrap();
            let copied = output(command(std::path::Path::new("/bin/sh")).args(["-c", instruction]));
            assert!(
                copied.status.success(),
                "{name}: {instruction}: {:?}",
                copied.stderr
            );
        }
        assert!(!workspace.join("result.txt").exists());
        let status = output(command(binary).args(["status", stage.to_str().unwrap(), "--json"]));
        assert!(status.status.success());
        let state: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(state["run"]["overlay"]["state"], "discarded");
    }
}
