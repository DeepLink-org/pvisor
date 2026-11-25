#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    runs: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project with spaces");
        std::fs::create_dir(&workspace).unwrap();
        let runs = root.path().join("runs");
        Self {
            root,
            workspace,
            runs,
        }
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pvisor"));
        command
            .current_dir(cwd)
            .env("HOME", self.root.path().join("home"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("PVISOR_RUN_HOME", &self.runs)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = self.command(&self.workspace);
        command.args(args);
        finish(command)
    }
}

fn finish(mut command: Command) -> Output {
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "CLI did not finish: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "requires macFUSE")]
fn last_finds_default_and_custom_storage_after_success_failure_and_timeout() {
    for (custom, script, timeout, exit) in [
        (false, "exit 0", "5s", 0),
        (true, "exit 0", "5s", 0),
        (true, "exit 42", "5s", 42),
        (true, "sleep 30", "200ms", 1),
    ] {
        let fixture = Fixture::new();
        let stage = fixture.root.path().join("outside run home/custom stage");
        let mut args = vec![
            "spawn",
            "--filesystem",
            "host",
            "--overlaynet",
            "off",
            "--no-agent-defaults",
            "--timeout",
            timeout,
            "--stdio",
            "capture",
        ];
        if custom {
            args.extend(["--stage", stage.to_str().unwrap()]);
        }
        args.extend(["--", "/bin/sh", "-c", script]);
        let output = fixture.run(&args);
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let selected = fixture.run(&["status", "last", "--json"]);
        assert!(
            selected.status.success(),
            "{}",
            String::from_utf8_lossy(&selected.stderr)
        );
        let status: serde_json::Value = serde_json::from_slice(&selected.stdout).unwrap();
        assert_eq!(
            status["state"],
            if exit == 0 { "completed" } else { "failed" }
        );
        let id = status["run"]["run_id"].as_str().unwrap();
        if custom {
            assert_eq!(status["run"]["stage"], stage.to_str().unwrap());
        }
        assert!(fixture.run(&["review", "last", "--diff"]).status.success());
        assert!(fixture.run(&["status", id, "--json"]).status.success());
        let other_workspace = fixture.root.path().join("other workspace");
        std::fs::create_dir(&other_workspace).unwrap();
        let mut command = fixture.command(&other_workspace);
        command.args(["status", "last", "--json"]);
        let absent = finish(command);
        assert!(!absent.status.success());
        assert!(String::from_utf8_lossy(&absent.stderr).contains("current workspace"));
        let stored = PathBuf::from(status["run"]["stage"].as_str().unwrap());
        std::fs::remove_dir_all(stored).unwrap();
        assert!(!fixture.run(&["status", "last", "--json"]).status.success());
    }
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "requires macFUSE")]
fn last_prefers_the_nearest_workspace_and_its_latest_session() {
    let fixture = Fixture::new();
    let outer = &fixture.workspace;
    let inner = outer.join("inner");
    let inner_child = inner.join("src");
    let outer_child = outer.join("inner-sibling");
    std::fs::create_dir_all(&inner_child).unwrap();
    std::fs::create_dir(&outer_child).unwrap();

    let session_id = |cwd: &Path, selector: Option<&Path>| {
        let mut command = fixture.command(cwd);
        command.arg("status").args(selector).arg("--json");
        let output = finish(command);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        status["run"]["run_id"].as_str().unwrap().to_owned()
    };
    let spawn = |cwd: &Path, stage_name: &str| {
        let stage = fixture.root.path().join(stage_name);
        let mut command = fixture.command(cwd);
        command
            .args([
                "spawn",
                "--filesystem",
                "host",
                "--overlaynet",
                "off",
                "--no-agent-defaults",
                "--stdio",
                "capture",
                "--stage",
            ])
            .arg(&stage)
            .args(["--", "/bin/sh", "-c", "exit 0"]);
        let output = finish(command);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        session_id(cwd, Some(&stage))
    };

    let older_inner = spawn(&inner, "older inner stage");
    let newer_inner = spawn(&inner, "newer inner stage");
    let outer_id = spawn(outer, "outer stage");
    assert_ne!(older_inner, newer_inner);

    let last = Some(Path::new("last"));
    assert_eq!(session_id(&inner, last), newer_inner);
    assert_eq!(session_id(&inner_child, None), newer_inner);
    assert_eq!(session_id(&outer_child, last), outer_id);
    assert_eq!(session_id(outer, last), outer_id);
    assert_eq!(session_id(&inner_child, Some(&fixture.runs)), outer_id);
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "requires macFUSE")]
fn custom_live_session_is_discoverable_and_cancellable_through_last() {
    let fixture = Fixture::new();
    let stage = fixture.root.path().join("custom stage");
    let mut command = fixture.command(&fixture.workspace);
    command.args([
        "spawn",
        "--filesystem",
        "host",
        "--overlaynet",
        "off",
        "--no-agent-defaults",
        "--stage",
        stage.to_str().unwrap(),
        "--timeout",
        "10s",
        "--",
        "/bin/sh",
        "-c",
        "printf ready > ready; sleep 30",
    ]);
    let mut owner = command.spawn().unwrap();
    let ready = Instant::now() + Duration::from_secs(5);
    let selected = loop {
        let selected = fixture.run(&["status", "last", "--lifecycle", "--json"]);
        let running = selected.status.success()
            && serde_json::from_slice::<serde_json::Value>(&selected.stdout).unwrap()["state"]
                == "running";
        if running || owner.try_wait().unwrap().is_some() || Instant::now() >= ready {
            break selected;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let cancelled = if selected.status.success() {
        fixture.run(&["kill", "-s", "TERM", "last"])
    } else {
        fixture.run(&["kill", "-s", "TERM", stage.to_str().unwrap()])
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while owner.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            owner.kill().unwrap();
            owner.wait().unwrap();
            panic!("cancelled Session frontend did not exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = owner.wait_with_output().unwrap();
    assert!(
        selected.status.success(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&selected.stdout).unwrap();
    assert_eq!(status["state"], "running");
    assert_eq!(status["live"], true);
    assert!(
        cancelled.status.success(),
        "{}",
        String::from_utf8_lossy(&cancelled.stderr)
    );
    assert!(!output.status.success());
    let stopped = fixture.run(&["status", "last", "--json"]);
    assert!(stopped.status.success());
    let status: serde_json::Value = serde_json::from_slice(&stopped.stdout).unwrap();
    assert_eq!(status["live"], false);
}

#[test]
#[cfg_attr(target_os = "macos", ignore = "requires macFUSE")]
fn failed_discovery_registration_drains_and_preserves_the_explicit_stage() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(&fixture.runs).unwrap();
    let obstruction = fixture.runs.join(".pvisor");
    std::fs::write(&obstruction, b"keep existing entry").unwrap();
    let stage = fixture.root.path().join("custom stage");
    let output = fixture.run(&[
        "spawn",
        "--filesystem",
        "host",
        "--overlaynet",
        "off",
        "--no-agent-defaults",
        "--stage",
        stage.to_str().unwrap(),
        "--timeout",
        "10s",
        "--",
        "/bin/sh",
        "-c",
        "sleep 30",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("discovery registration failed after launch"),
        "{error}"
    );
    assert!(error.contains("workload effects may remain"), "{error}");
    assert!(error.contains(stage.to_str().unwrap()), "{error}");
    assert_eq!(std::fs::read(obstruction).unwrap(), b"keep existing entry");
    let selected = fixture.run(&["status", stage.to_str().unwrap(), "--json"]);
    assert!(
        selected.status.success(),
        "{}",
        String::from_utf8_lossy(&selected.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&selected.stdout).unwrap();
    assert_eq!(status["live"], false);
    assert_ne!(status["state"], "running");
    assert!(stage.join("completion.json").is_file());
}
