//! Routine diagnostics must be correlated and stay outside workload stdout.
use std::process::Command;

#[test]
fn startup_logs_are_default_correlated_and_route_to_frontend() {
    let temp = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor"));
    command
        .args([
            "run",
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
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("pvisor-startup "));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("pvisor-persistence "));

    let path = temp.path().join("frontend.log");
    std::fs::write(&path, "").unwrap();
    let output = command
        .env_remove("PVISOR_STARTUP_TIMING")
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
    assert!(!String::from_utf8_lossy(&output.stderr).contains("pvisor-startup "));
    let log = std::fs::read_to_string(path).unwrap();
    assert!(log.contains("stage=process.entry"));
    assert!(log.contains("stage=cli.run_finished"));
    assert!(log.contains("pvisor-persistence "));
    // Concurrent host processes share the same frontend append log too.
    std::fs::write(temp.path().join("frontend.log"), "").unwrap();
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let children: Vec<_> = (0..4).map(|_| command.spawn().unwrap()).collect();
    let pids: Vec<_> = children
        .iter()
        .map(|child| child.id().to_string())
        .collect();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"WORKLOAD_READY\n");
    }
    let log = std::fs::read_to_string(temp.path().join("frontend.log")).unwrap();
    // Parsing and execution have separate PIDs. Correlate each request worker
    // with its originating frontend.
    let bindings: Vec<_> = log
        .lines()
        .filter(|line| line.starts_with("pvisor-host-request "))
        .collect();
    assert_eq!(bindings.len(), 4);
    let mut worker_pids = std::collections::HashSet::new();
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
        assert!(worker_pids.insert(field("worker_pid=").to_owned()));
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
            pids.iter().any(|expected| expected == pid) || worker_pids.contains(pid),
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
